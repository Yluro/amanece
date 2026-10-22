//! Background tokio tasks owning the single SSH connection: one command loop for
//! listings/scans and one sequential download loop. The UI talks to them through
//! `RemoteHandle` and receives `RemoteEvent`s on its std channel.

use super::scan;
use super::session::{self, ConnectError, Connection, HostKey, Target};
use super::sftp::{Limiter, ReadOnlySftp};
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

/// Pause between background scans ("fetch all stats", poll rescans).
const BACKGROUND_GAP: Duration = Duration::from_secs(3);
const MAX_BACKOFF: Duration = Duration::from_secs(600);

#[derive(Debug, Clone)]
pub struct DownloadJob {
    pub id: u64,
    pub remote: String,
    pub local: PathBuf,
    pub size: Option<u64>,
    pub label: String,
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
    Shutdown,
}

#[derive(Debug)]
pub enum DownloadEvent {
    Started { id: u64, label: String, total: Option<u64> },
    Progress { id: u64, done: u64, total: Option<u64> },
    Finished { id: u64, label: String, bytes: u64, skipped: bool },
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
    Error(String),
}

pub struct Settings {
    pub host: String,
    pub port: u16,
    pub rate: f64,
    pub burst: u32,
    pub known_hosts: Option<PathBuf>,
    pub beamline: String,
    pub remote_root: Option<String>,
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

#[derive(Default)]
struct Shared {
    sftp: Mutex<Option<Arc<ReadOnlySftp>>>,
    /// Bumped by CancelDownloads; jobs from older generations are dropped.
    generation: AtomicU64,
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
}

pub fn spawn(rt: &tokio::runtime::Runtime, settings: Settings, events: std::sync::mpsc::Sender<AppEvent>) -> RemoteHandle {
    let (tx, rx) = unbounded_channel();
    let (dl_tx, dl_rx) = unbounded_channel();
    let requests = Arc::new(AtomicU64::new(0));
    let limiter = Arc::new(Limiter::new(settings.rate, settings.burst, requests.clone()));
    let shared = Arc::new(Shared::default());
    rt.spawn(command_loop(
        rx,
        tx.clone(),
        dl_tx,
        shared.clone(),
        settings,
        limiter,
        events.clone(),
    ));
    rt.spawn(download_loop(dl_rx, shared, events));
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
    dl_tx: UnboundedSender<(u64, DownloadJob)>,
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
        beamline: &settings.beamline,
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
                        let root = c.sftp.root().to_string();
                        shared.set(Some(c.sftp.clone()));
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
                        let root = c.sftp.root().to_string();
                        shared.set(Some(c.sftp.clone()));
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
            }
            RemoteCmd::Download(jobs) => {
                let generation = shared.generation.load(Ordering::SeqCst);
                for j in jobs {
                    let _ = dl_tx.send((generation, j));
                }
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

async fn download_loop(
    mut rx: UnboundedReceiver<(u64, DownloadJob)>,
    shared: Arc<Shared>,
    events: std::sync::mpsc::Sender<AppEvent>,
) {
    while let Some((generation, job)) = rx.recv().await {
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
        let Some(sftp) = shared.get() else {
            fail("not connected".into());
            continue;
        };
        if let Err(e) = download_one(&sftp, &job, generation, &shared, &events).await {
            fail(format!("{e:#}"));
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
    let local_len = std::fs::metadata(&job.local).ok().map(|m| m.len());
    if let (Some(want), Some(have)) = (job.size, local_len)
        && want == have
    {
        emit(
            events,
            RemoteEvent::Download(DownloadEvent::Finished {
                id: job.id,
                label: job.label.clone(),
                bytes: have,
                skipped: true,
            }),
        );
        return Ok(());
    }
    let mut reader = sftp.open_read(&job.remote).await?;
    let total = reader.size.or(job.size);
    if let (Some(want), Some(have)) = (total, local_len)
        && want == have
    {
        reader.close().await;
        emit(
            events,
            RemoteEvent::Download(DownloadEvent::Finished {
                id: job.id,
                label: job.label.clone(),
                bytes: have,
                skipped: true,
            }),
        );
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
    let mut part_name = job.local.file_name().unwrap_or_default().to_os_string();
    part_name.push(".part");
    let part = job.local.with_file_name(part_name);
    let mut out = tokio::fs::File::create(&part).await?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
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
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    if let Some(t) = total
        && t != done
    {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(anyhow!("incomplete: got {done} of {t} bytes"));
    }
    tokio::fs::rename(&part, &job.local).await?;
    emit(
        events,
        RemoteEvent::Download(DownloadEvent::Finished {
            id: job.id,
            label: job.label.clone(),
            bytes: done,
            skipped: false,
        }),
    );
    Ok(())
}
