//! Background tokio tasks sharing the single SSH connection: one command loop for
//! listings/scans, and download workers. HKL/log downloads go to a priority queue that
//! every worker serves first, plus an express worker that serves only it, so they start
//! at once even while frames fill the bulk workers. The UI talks to them through
//! `RemoteHandle` and receives `RemoteEvent`s on its std channel.

use super::scan;
use super::session::{self, ConnectError, Connection, HostKey, Target};
use super::sftp::{Limiter, ReadOnlySftp};
use crate::cbf;
use crate::event::AppEvent;
use crate::model::{RemoteFile, SampleInfo};
use anyhow::{Result, anyhow};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// Upper bound for `parallel_downloads`.
pub const MAX_PARALLEL_DOWNLOADS: usize = 4;

/// Pause between background scans ("fetch all stats", poll rescans).
const BACKGROUND_GAP: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(600);
/// Tries in a row without any new bytes before a file is given up (reconnecting in
/// between). Tries that move the download forward don't count: it resumes from `.part`.
const DOWNLOAD_ATTEMPTS: usize = 3;

/// Answer to `RemoteCmd::ConnectionLost`.
#[derive(Debug)]
pub enum Recovery {
    /// The session is fine: the error was about the file itself.
    Alive,
    /// The session had died and a new one is up: retry.
    Reconnected,
    /// The session is gone and could not be restored now.
    Gone,
}

#[derive(Debug, Clone)]
pub struct DownloadJob {
    pub id: u64,
    pub remote: String,
    pub local: PathBuf,
    pub size: Option<u64>,
    pub label: String,
    /// HKL/logs: jump ahead of bulk downloads (frames) and get a slot of their own.
    pub priority: bool,
}

#[derive(Debug)]
pub enum RemoteCmd {
    Connect { user: String, password: String },
    ListExperiments,
    ListSamples { date: String, from_poll: bool },
    ScanSample {
        date: String,
        sample: String,
        flat: bool,
        known: Option<SampleInfo>,
        background: bool,
    },
    ListFrames {
        date: String,
        sample: String,
        runs: Vec<(String, u32)>,
    },
    Download(Vec<DownloadJob>),
    CancelDownloads,
    Reconnect,
    /// From a download worker after a failed file: did the session it used (`seen`,
    /// see `Shared::conn_gen`) die? Reconnect if so.
    ConnectionLost {
        seen: u64,
        reply: tokio::sync::oneshot::Sender<Recovery>,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum DownloadEvent {
    Started { id: u64, label: String, total: Option<u64> },
    Progress { id: u64, done: u64, total: Option<u64> },
    /// `verified`: a frame (`.cbf`) whose bytes match its own Content-MD5, whether
    /// just downloaded or already here (`skipped`). False for files with nothing to check.
    Finished { id: u64, label: String, bytes: u64, skipped: bool, verified: bool },
    Failed { id: u64, label: String, message: String },
}

#[derive(Debug)]
pub enum RemoteEvent {
    Connected {
        user: String,
        host_key: HostKey,
        root: String,
    },
    ConnectFailed { auth: bool, message: String },
    Disconnected { message: String },
    Reconnecting { in_secs: u64 },
    Experiments {
        ids: Vec<String>,
        home: String,
        top: Vec<String>,
    },
    ExperimentsFailed(String),
    Samples {
        date: String,
        names: Vec<String>,
        flat: bool,
        from_poll: bool,
    },
    SampleProgress { date: String, info: SampleInfo },
    SampleScanned { date: String, info: SampleInfo },
    ScanFailed { date: String, sample: String, message: String },
    Frames { date: String, sample: String, files: Vec<RemoteFile> },
    Download(DownloadEvent),
    /// Something worth a line in the console (not an error).
    Notice(String),
    Error(String),
}

pub struct Settings {
    pub host: String,
    pub port: u16,
    pub rate: f64,
    pub burst: u32,
    pub known_hosts: Option<PathBuf>,
    pub beamlines: Vec<String>,
    pub remote_root: Option<String>,
    /// Bulk download workers (frames); clamped to 1..=MAX_PARALLEL_DOWNLOADS.
    pub parallel_downloads: usize,
}

pub struct RemoteHandle {
    tx: UnboundedSender<RemoteCmd>,
    /// Remote requests made this session (shown in the status bar).
    pub requests: Arc<AtomicU64>,
}

impl RemoteHandle {
    pub fn send(&self, cmd: RemoteCmd) {
        let _ = self.tx.send(cmd);
    }
}

struct Queued {
    generation: u64,
    job: DownloadJob,
}

#[derive(Default)]
struct Queues {
    priority: VecDeque<Queued>,
    bulk: VecDeque<Queued>,
}

#[derive(Default)]
struct Shared {
    sftp: Mutex<Option<Arc<ReadOnlySftp>>>,
    /// Bumped by CancelDownloads; jobs from older generations are dropped.
    generation: AtomicU64,
    /// Bumped on every (re)connect, so a worker can tell "my session died" from
    /// "another worker already reconnected".
    conn_gen: AtomicU64,
    queues: Mutex<Queues>,
    wake: tokio::sync::Notify,
}

impl Shared {
    fn get(&self) -> Option<Arc<ReadOnlySftp>> {
        self.sftp.lock().ok().and_then(|g| g.clone())
    }

    fn set(&self, s: Option<Arc<ReadOnlySftp>>) {
        if let Ok(mut g) = self.sftp.lock() {
            *g = s;
        }
    }

    fn install(&self, s: Arc<ReadOnlySftp>) {
        self.set(Some(s));
        self.conn_gen.fetch_add(1, Ordering::SeqCst);
    }

    fn push(&self, generation: u64, jobs: Vec<DownloadJob>) {
        if let Ok(mut q) = self.queues.lock() {
            for job in jobs {
                let item = Queued { generation, job };
                if item.job.priority {
                    q.priority.push_back(item);
                } else {
                    q.bulk.push_back(item);
                }
            }
        }
        self.wake.notify_waiters();
    }

    /// Next job: priority first; bulk only for workers that take it.
    fn take(&self, priority_only: bool) -> Option<Queued> {
        let mut q = self.queues.lock().ok()?;
        q.priority
            .pop_front()
            .or_else(|| if priority_only { None } else { q.bulk.pop_front() })
    }

    fn drain(&self) -> Vec<Queued> {
        let Ok(mut q) = self.queues.lock() else {
            return Vec::new();
        };
        let mut all: Vec<Queued> = q.priority.drain(..).collect();
        all.extend(q.bulk.drain(..));
        all
    }
}

pub fn spawn(rt: &tokio::runtime::Runtime, settings: Settings, events: std::sync::mpsc::Sender<AppEvent>) -> RemoteHandle {
    let (tx, rx) = unbounded_channel();
    let requests = Arc::new(AtomicU64::new(0));
    let limiter = Arc::new(Limiter::new(settings.rate, settings.burst, requests.clone()));
    let shared = Arc::new(Shared::default());
    let workers = settings.parallel_downloads.clamp(1, MAX_PARALLEL_DOWNLOADS);
    rt.spawn(command_loop(rx, tx.clone(), shared.clone(), settings, limiter, events.clone()));
    for _ in 0..workers {
        rt.spawn(download_worker(false, tx.clone(), shared.clone(), events.clone()));
    }
    // the express slot: only priority jobs, so they never wait behind frames
    rt.spawn(download_worker(true, tx.clone(), shared, events));
    RemoteHandle { tx, requests }
}

fn emit(events: &std::sync::mpsc::Sender<AppEvent>, e: RemoteEvent) {
    let _ = events.send(AppEvent::Remote(e));
}

struct Background {
    date: String,
    sample: String,
    flat: bool,
    known: Option<SampleInfo>,
}

async fn command_loop(
    mut rx: UnboundedReceiver<RemoteCmd>,
    self_tx: UnboundedSender<RemoteCmd>,
    shared: Arc<Shared>,
    settings: Settings,
    limiter: Arc<Limiter>,
    events: std::sync::mpsc::Sender<AppEvent>,
) {
    let target = Target {
        host: &settings.host,
        port: settings.port,
        known_hosts: settings.known_hosts.clone(),
        limiter,
        beamlines: &settings.beamlines,
        remote_root: settings.remote_root.as_deref(),
    };
    let mut conn: Option<Connection> = None;
    let mut creds: Option<(String, String)> = None;
    let mut backoff = Duration::from_secs(30);
    let mut reconnect_pending = false;
    let mut background: VecDeque<Background> = VecDeque::new();
    let mut last_background = Instant::now() - BACKGROUND_GAP;

    loop {
        let gap = BACKGROUND_GAP.saturating_sub(last_background.elapsed());
        let cmd = tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(c) => c,
                None => break,
            },
            _ = tokio::time::sleep(gap), if !background.is_empty() && conn.is_some() => {
                let job = background.pop_front().expect("non-empty");
                last_background = Instant::now();
                RemoteCmd::ScanSample {
                    date: job.date,
                    sample: job.sample,
                    flat: job.flat,
                    known: job.known,
                    background: false,
                }
            }
        };

        match cmd {
            RemoteCmd::Shutdown => break,
            RemoteCmd::Connect { user, password } => {
                if let Some(c) = conn.take() {
                    shared.set(None);
                    c.close().await;
                }
                match session::connect(&target, &user, &password).await {
                    Ok((c, host_key)) => {
                        let root = c.sftp.describe();
                        shared.install(c.sftp.clone());
                        conn = Some(c);
                        creds = Some((user.clone(), password));
                        backoff = Duration::from_secs(30);
                        emit(&events, RemoteEvent::Connected { user, host_key, root });
                    }
                    Err(e) => emit(
                        &events,
                        RemoteEvent::ConnectFailed {
                            auth: matches!(e, ConnectError::Auth(_)),
                            message: e.to_string(),
                        },
                    ),
                }
            }
            RemoteCmd::Reconnect => {
                reconnect_pending = false;
                if conn.is_some() {
                    continue;
                }
                let Some((user, password)) = creds.clone() else {
                    continue;
                };
                match session::connect(&target, &user, &password).await {
                    Ok((c, host_key)) => {
                        let root = c.sftp.describe();
                        shared.install(c.sftp.clone());
                        conn = Some(c);
                        backoff = Duration::from_secs(30);
                        emit(&events, RemoteEvent::Connected { user, host_key, root });
                    }
                    Err(ConnectError::Auth(m)) => {
                        // Never retry a rejected password automatically.
                        creds = None;
                        emit(&events, RemoteEvent::ConnectFailed { auth: true, message: m });
                    }
                    Err(e) => {
                        emit(&events, RemoteEvent::Error(format!("reconnect failed: {e}")));
                        schedule_reconnect(&self_tx, backoff, &mut reconnect_pending, &events);
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                    }
                }
            }
            RemoteCmd::CancelDownloads => {
                shared.generation.fetch_add(1, Ordering::SeqCst);
                for q in shared.drain() {
                    emit(
                        &events,
                        RemoteEvent::Download(DownloadEvent::Failed {
                            id: q.job.id,
                            label: q.job.label,
                            message: "cancelled".into(),
                        }),
                    );
                }
            }
            RemoteCmd::ConnectionLost { seen, reply } => {
                if conn.as_ref().is_some_and(|c| !c.handle.is_closed()) {
                    // alive: either the error was about the file, or another worker
                    // already reconnected after the session this one used had died
                    let replaced = shared.conn_gen.load(Ordering::SeqCst) != seen;
                    let _ = reply.send(if replaced { Recovery::Reconnected } else { Recovery::Alive });
                    continue;
                }
                shared.set(None);
                if let Some(c) = conn.take() {
                    let _ = tokio::time::timeout(Duration::from_secs(2), c.close()).await;
                }
                let Some((user, password)) = creds.clone() else {
                    let _ = reply.send(Recovery::Gone);
                    continue;
                };
                emit(
                    &events,
                    RemoteEvent::Disconnected {
                        message: "connection lost during a download; reconnecting".into(),
                    },
                );
                match session::connect(&target, &user, &password).await {
                    Ok((c, host_key)) => {
                        let root = c.sftp.describe();
                        shared.install(c.sftp.clone());
                        conn = Some(c);
                        backoff = Duration::from_secs(30);
                        emit(&events, RemoteEvent::Connected { user, host_key, root });
                        let _ = reply.send(Recovery::Reconnected);
                    }
                    Err(ConnectError::Auth(m)) => {
                        creds = None;
                        emit(&events, RemoteEvent::ConnectFailed { auth: true, message: m });
                        let _ = reply.send(Recovery::Gone);
                    }
                    Err(e) => {
                        emit(&events, RemoteEvent::Error(format!("reconnect failed: {e}")));
                        schedule_reconnect(&self_tx, backoff, &mut reconnect_pending, &events);
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        let _ = reply.send(Recovery::Gone);
                    }
                }
            }
            RemoteCmd::Download(jobs) => {
                shared.push(shared.generation.load(Ordering::SeqCst), jobs);
            }
            RemoteCmd::ScanSample {
                date,
                sample,
                flat,
                known,
                background: true,
            } => {
                if !background.iter().any(|b| b.date == date && b.sample == sample) {
                    background.push_back(Background {
                        date,
                        sample,
                        flat,
                        known,
                    });
                }
            }
            op => {
                let Some(sftp) = shared.get() else {
                    emit(&events, RemoteEvent::Error(not_connected(&op)));
                    continue;
                };
                if let Err(e) = run_op(&sftp, op, &events).await {
                    let closed = conn.as_ref().is_none_or(|c| c.handle.is_closed());
                    if closed {
                        shared.set(None);
                        conn = None;
                        emit(&events, RemoteEvent::Disconnected { message: e.to_string() });
                        if creds.is_some() {
                            schedule_reconnect(&self_tx, Duration::from_secs(2), &mut reconnect_pending, &events);
                        }
                    }
                }
            }
        }
    }
    shared.set(None);
    if let Some(c) = conn.take() {
        c.close().await;
    }
}

fn not_connected(op: &RemoteCmd) -> String {
    match op {
        RemoteCmd::ScanSample { sample, .. } => format!("not connected: cannot scan {sample}"),
        _ => "not connected".into(),
    }
}

fn schedule_reconnect(
    tx: &UnboundedSender<RemoteCmd>,
    after: Duration,
    pending: &mut bool,
    events: &std::sync::mpsc::Sender<AppEvent>,
) {
    if *pending {
        return;
    }
    *pending = true;
    emit(events, RemoteEvent::Reconnecting { in_secs: after.as_secs() });
    let tx = tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        let _ = tx.send(RemoteCmd::Reconnect);
    });
}

/// Runs one listing/scan command. Errors are reported here; the returned error
/// only tells the caller to check whether the connection dropped.
async fn run_op(sftp: &ReadOnlySftp, op: RemoteCmd, events: &std::sync::mpsc::Sender<AppEvent>) -> Result<()> {
    match op {
        RemoteCmd::ListExperiments => match scan::list_experiments(sftp).await {
            Ok(x) => emit(
                events,
                RemoteEvent::Experiments {
                    ids: x.ids,
                    home: sftp.root().to_string(),
                    top: x.top,
                },
            ),
            Err(e) => {
                emit(events, RemoteEvent::ExperimentsFailed(format!("{e:#}")));
                return Err(e);
            }
        },
        RemoteCmd::ListSamples { date, from_poll } => match scan::list_samples(sftp, &date).await {
            Ok((names, flat)) => emit(
                events,
                RemoteEvent::Samples {
                    date,
                    names,
                    flat,
                    from_poll,
                },
            ),
            Err(e) => {
                emit(events, RemoteEvent::Error(format!("{e:#}")));
                return Err(e);
            }
        },
        RemoteCmd::ScanSample {
            date,
            sample,
            flat,
            known,
            ..
        } => {
            let res = scan::scan_sample(sftp, &date, &sample, flat, known.as_ref(), |info| {
                emit(
                    events,
                    RemoteEvent::SampleProgress {
                        date: date.clone(),
                        info: info.clone(),
                    },
                )
            })
            .await;
            match res {
                Ok(info) => emit(events, RemoteEvent::SampleScanned { date, info }),
                Err(e) => {
                    emit(
                        events,
                        RemoteEvent::ScanFailed {
                            date,
                            sample,
                            message: format!("{e:#}"),
                        },
                    );
                    return Err(e);
                }
            }
        }
        RemoteCmd::ListFrames { date, sample, runs } => match scan::list_frames(sftp, &date, &sample, &runs).await {
            Ok(files) => emit(events, RemoteEvent::Frames { date, sample, files }),
            Err(e) => {
                emit(events, RemoteEvent::Error(format!("{e:#}")));
                return Err(e);
            }
        },
        _ => {}
    }
    Ok(())
}

/// One download worker. `priority_only`: the express slot, which never takes bulk jobs.
async fn download_worker(
    priority_only: bool,
    cmd_tx: UnboundedSender<RemoteCmd>,
    shared: Arc<Shared>,
    events: std::sync::mpsc::Sender<AppEvent>,
) {
    loop {
        // registered before looking, so a push in between still wakes us
        let notified = shared.wake.notified();
        let Some(Queued { generation, job }) = shared.take(priority_only) else {
            notified.await;
            continue;
        };
        let fail = |message: String| {
            emit(
                &events,
                RemoteEvent::Download(DownloadEvent::Failed {
                    id: job.id,
                    label: job.label.clone(),
                    message,
                }),
            )
        };
        if generation != shared.generation.load(Ordering::SeqCst) {
            fail("cancelled".into());
            continue;
        }
        let mut stuck = 0;
        loop {
            let seen = shared.conn_gen.load(Ordering::SeqCst);
            let before = part_len(&job.local);
            let Some(sftp) = shared.get() else {
                fail("not connected".into());
                break;
            };
            let Err(e) = download_one(&sftp, &job, generation, &shared, &events).await else {
                break;
            };
            stuck = if part_len(&job.local) > before { 0 } else { stuck + 1 };
            if generation != shared.generation.load(Ordering::SeqCst) || stuck >= DOWNLOAD_ATTEMPTS {
                discard_part(&job.local);
                fail(format!("{e:#}"));
                break;
            }
            // the whole frame arrived but fails its own checksum: the session is fine, so
            // fetch it again from scratch (download_one already removed the .part)
            if e.is::<cbf::Corrupt>() {
                emit(
                    &events,
                    RemoteEvent::Notice(format!("{}: {e:#}; downloading it again", job.label)),
                );
                continue;
            }
            // Did the session die under us? The command loop checks and reconnects once,
            // however many workers ask.
            let (tx, answer) = tokio::sync::oneshot::channel();
            if cmd_tx.send(RemoteCmd::ConnectionLost { seen, reply: tx }).is_err() {
                fail(format!("{e:#}"));
                break;
            }
            match answer.await {
                Ok(Recovery::Reconnected) => {
                    if job.priority {
                        emit(
                            &events,
                            RemoteEvent::Notice(format!("connection restored, retrying {}", job.label)),
                        );
                    }
                }
                _ => {
                    discard_part(&job.local);
                    fail(format!("{e:#}"));
                    break;
                }
            }
        }
    }
}

fn part_path(local: &std::path::Path) -> PathBuf {
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    local.with_file_name(name)
}

fn part_len(local: &std::path::Path) -> u64 {
    std::fs::metadata(part_path(local)).map(|m| m.len()).unwrap_or(0)
}

fn discard_part(local: &std::path::Path) {
    let _ = std::fs::remove_file(part_path(local));
}

/// Frames (`job.local` is a `.cbf`) are checked against their own Content-MD5; other
/// files pass unread. `path` is the file to read: the `.part` or the local file.
async fn self_check(job: &DownloadJob, path: &std::path::Path) -> Result<cbf::Check> {
    if !cbf::is_cbf(&job.local) {
        return Ok(cbf::Check::Unchecked);
    }
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || cbf::check_file(&path)).await?
}

/// The local file already has the remote size. A frame must also pass its checksum to
/// be kept (true, reported as skipped); a corrupt copy is fetched again (false).
async fn keep_local(job: &DownloadJob, have: u64, events: &std::sync::mpsc::Sender<AppEvent>) -> bool {
    match self_check(job, &job.local).await {
        Ok(check) => {
            emit(
                events,
                RemoteEvent::Download(DownloadEvent::Finished {
                    id: job.id,
                    label: job.label.clone(),
                    bytes: have,
                    skipped: true,
                    verified: check == cbf::Check::Verified,
                }),
            );
            true
        }
        Err(e) => {
            emit(
                events,
                RemoteEvent::Notice(format!("{}: local copy failed its check ({e:#}); downloading it again", job.label)),
            );
            false
        }
    }
}

async fn download_one(
    sftp: &ReadOnlySftp,
    job: &DownloadJob,
    generation: u64,
    shared: &Shared,
    events: &std::sync::mpsc::Sender<AppEvent>,
) -> Result<()> {
    let mut local_len = std::fs::metadata(&job.local).ok().map(|m| m.len());
    if let (Some(want), Some(have)) = (job.size, local_len)
        && want == have
    {
        if keep_local(job, have, events).await {
            return Ok(());
        }
        local_len = None;
    }
    let mut reader = sftp.open_download(&job.remote).await?;
    let total = reader.size.or(job.size);
    if let (Some(want), Some(have)) = (total, local_len)
        && want == have
        && keep_local(job, have, events).await
    {
        reader.close().await;
        return Ok(());
    }
    emit(
        events,
        RemoteEvent::Download(DownloadEvent::Started {
            id: job.id,
            label: job.label.clone(),
            total,
        }),
    );

    if let Some(dir) = job.local.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let part = part_path(&job.local);
    // resume what an earlier try (cut by a dropped session) already wrote
    let mut done = part_len(&job.local);
    if total.is_some_and(|t| done > t) {
        done = 0;
    }
    if done > 0 && reader.seek(done).await.is_err() {
        done = 0;
    }
    let mut out = if done > 0 {
        tokio::fs::OpenOptions::new().append(true).open(&part).await?
    } else {
        tokio::fs::File::create(&part).await?
    };
    if done > 0 {
        emit(
            events,
            RemoteEvent::Download(DownloadEvent::Progress { id: job.id, done, total }),
        );
    }
    let mut buf = vec![0u8; 256 * 1024];
    let mut last = Instant::now();
    let result: Result<()> = async {
        loop {
            if generation != shared.generation.load(Ordering::SeqCst) {
                return Err(anyhow!("cancelled"));
            }
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).await?;
            done += n as u64;
            if last.elapsed() >= Duration::from_millis(200) {
                last = Instant::now();
                emit(
                    events,
                    RemoteEvent::Download(DownloadEvent::Progress { id: job.id, done, total }),
                );
            }
        }
        out.flush().await?;
        Ok(())
    }
    .await;
    drop(out);
    reader.close().await;
    // a failed try keeps its .part so the next one resumes (the worker discards it when
    // it gives up or the download is cancelled)
    result?;
    if let Some(t) = total
        && t != done
    {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(anyhow!("incomplete: got {done} of {t} bytes"));
    }
    // a frame that fails its own checksum never gets its final name; the worker retries
    let check = match self_check(job, &part).await {
        Ok(c) => c,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    tokio::fs::rename(&part, &job.local).await?;
    emit(
        events,
        RemoteEvent::Download(DownloadEvent::Finished {
            id: job.id,
            label: job.label.clone(),
            bytes: done,
            skipped: false,
            verified: check == cbf::Check::Verified,
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64, priority: bool) -> DownloadJob {
        DownloadJob {
            id,
            remote: format!("/r/{id}"),
            local: PathBuf::from(format!("{id}")),
            size: None,
            label: format!("{id}"),
            priority,
        }
    }

    #[test]
    fn priority_jobs_jump_the_queue() {
        let s = Shared::default();
        s.push(0, vec![job(1, false), job(2, false), job(3, false)]);
        s.push(0, vec![job(9, true)]);
        // the express slot only takes priority jobs
        assert_eq!(s.take(true).map(|q| q.job.id), Some(9));
        assert!(s.take(true).is_none());
        // bulk workers: priority first, then frames in order
        s.push(0, vec![job(10, true)]);
        let order: Vec<u64> = std::iter::from_fn(|| s.take(false).map(|q| q.job.id)).collect();
        assert_eq!(order, vec![10, 1, 2, 3]);
    }

    #[tokio::test]
    async fn only_frames_are_self_checked() {
        let dir = std::env::temp_dir().join(format!("amanece_selfcheck_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bad = cbf::tests::tiny_cbf(Some(cbf::tests::BINARY_MD5));
        let binary = bad.windows(4).position(|w| w == b"\x0c\x1a\x04\xd5").unwrap() + 4;
        bad[binary + 2] ^= 0x01;
        let frame = |name: &str| DownloadJob {
            local: dir.join(name),
            ..job(1, false)
        };

        let good = frame("good_0001.cbf");
        std::fs::write(&good.local, cbf::tests::tiny_cbf(Some(cbf::tests::BINARY_MD5))).unwrap();
        assert_eq!(self_check(&good, &good.local).await.unwrap(), cbf::Check::Verified);

        // the download's .part is checked by the final name's extension
        let corrupt = frame("bad_0001.cbf");
        let part = part_path(&corrupt.local);
        std::fs::write(&part, &bad).unwrap();
        assert!(self_check(&corrupt, &part).await.unwrap_err().is::<cbf::Corrupt>());

        // anything but a frame passes unread, whatever it holds
        let hkl = frame("XDS_ASCII.HKL");
        std::fs::write(&hkl.local, &bad).unwrap();
        assert_eq!(self_check(&hkl, &hkl.local).await.unwrap(), cbf::Check::Unchecked);

        // keep_local: a corrupt frame of the right size is not kept, a good one is
        let (tx, rx) = std::sync::mpsc::channel();
        std::fs::write(&corrupt.local, &bad).unwrap();
        assert!(!keep_local(&corrupt, bad.len() as u64, &tx).await);
        assert!(keep_local(&good, 1, &tx).await);
        let events: Vec<AppEvent> = rx.try_iter().collect();
        assert!(
            matches!(&events[0], AppEvent::Remote(RemoteEvent::Notice(m)) if m.contains("Content-MD5 mismatch")),
            "{events:?}"
        );
        assert!(
            matches!(
                &events[1],
                AppEvent::Remote(RemoteEvent::Download(DownloadEvent::Finished {
                    skipped: true,
                    verified: true,
                    ..
                }))
            ),
            "{events:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancel_drains_both_queues() {
        let s = Shared::default();
        s.push(0, vec![job(1, false), job(2, true)]);
        assert_eq!(s.drain().len(), 2);
        assert!(s.take(false).is_none());
    }
}
