//! Application state and event handling. Rendering lives in `ui.rs`.

use crate::cache::Cache;
use crate::config::{Config, PollMode};
use crate::console::{Builtin, Console, ConsoleEvent, LineKind};
use crate::download::{self, Source};
use crate::event::AppEvent;
use crate::layout;
use crate::local::LocalPanel;
use crate::model::{RemoteFile, SampleInfo, Solve, human_bytes, now_secs};
use crate::notify;
use crate::remote::session::HostKey;
use crate::remote::{DownloadEvent, DownloadJob, RemoteCmd, RemoteEvent, RemoteHandle};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

pub const MAX_LOGIN_ATTEMPTS: u32 = 3;
const SCAN_DEBOUNCE: Duration = Duration::from_millis(700);
/// Re-scan a selected sample with running solves at most this often.
const RESCAN_RUNNING_SECS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Local,
    Alba,
    Stats,
    Console,
}

impl Focus {
    fn next(self) -> Focus {
        match self {
            Focus::Local => Focus::Alba,
            Focus::Alba => Focus::Stats,
            Focus::Stats => Focus::Console,
            Focus::Console => Focus::Local,
        }
    }

    fn prev(self) -> Focus {
        match self {
            Focus::Local => Focus::Console,
            Focus::Alba => Focus::Local,
            Focus::Stats => Focus::Alba,
            Focus::Console => Focus::Stats,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conn {
    Offline,
    Connecting,
    Connected(String),
    Reconnecting(Option<u64>),
    Lost(String),
}

pub struct Login {
    pub user: String,
    pub pass: String,
    pub field: usize,
    pub busy: bool,
    pub error: Option<String>,
}

pub enum Pending {
    Frames {
        date: String,
        sample: String,
        files: Vec<RemoteFile>,
    },
    Quit,
}

pub enum InputAction {
    NewFolder,
}

pub enum Dialog {
    Login(Login),
    Input {
        title: String,
        value: String,
        action: InputAction,
    },
    Confirm {
        title: String,
        lines: Vec<String>,
        action: Pending,
    },
    Help,
    Message {
        title: String,
        text: String,
        exit: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbaRow {
    /// `..` back to the beamtime list.
    Up,
    Date(String),
    Sample { date: String, name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsRow {
    Solve { run: usize, solve: usize },
    Frames { run: usize },
}

pub struct Active {
    pub id: u64,
    pub label: String,
    pub done: u64,
    pub total: Option<u64>,
    pub started: Instant,
}

#[derive(Default)]
pub struct Downloads {
    pub queued: usize,
    pub active: Option<Active>,
    pub done: usize,
    pub failed: usize,
    pub bytes: u64,
}

pub struct App {
    pub cfg: Config,
    pub config_path: Option<PathBuf>,
    pub base: PathBuf,
    pub focus: Focus,
    prev_focus: Focus,
    pub dialog: Option<Dialog>,
    pub conn: Conn,
    pub login_attempts: u32,
    pub local: LocalPanel,
    pub cache: Cache,
    cache_dirty: bool,
    cache_saved: Instant,
    /// Beamtime opened in the ALBA panel; `None` shows the beamtime list.
    pub alba_date: Option<String>,
    pub alba_sel: usize,
    pub stats_sel: usize,
    /// Marked solves, by `Solve::key()` (contains the full remote path).
    pub marks: HashSet<String>,
    /// Frame-run checkbox overrides, keyed `date/sample/prefix_run`.
    pub frame_marks: HashMap<String, bool>,
    pub console: Console,
    pub remote: RemoteHandle,
    tx: Sender<AppEvent>,
    pub downloads: Downloads,
    next_job: u64,
    pub poll_mode: PollMode,
    pub next_poll: Option<Instant>,
    pub show_test: bool,
    pub sound: bool,
    pub include_logs: bool,
    /// Samples that may still deserve a "new sample" notification.
    pending_notify: HashSet<(String, String)>,
    listed_this_session: HashSet<String>,
    pub scanning: HashSet<(String, String)>,
    sel_changed: Option<Instant>,
    pub status: Option<(String, LineKind, Instant)>,
    /// Shown in the ALBA panel when the beamtime listing came back empty or failed.
    pub alba_note: Option<String>,
    /// Proposal folder on the server for this session.
    pub remote_root: Option<String>,
    experiments_listed: bool,
    pub quit: bool,
}

impl App {
    pub fn new(
        cfg: Config,
        config_path: Option<PathBuf>,
        base: PathBuf,
        remote: RemoteHandle,
        tx: Sender<AppEvent>,
        user: Option<String>,
        offline: bool,
    ) -> App {
        let cache = Cache::load(&base, &cfg.host);
        let mut app = App {
            poll_mode: cfg.poll_mode,
            show_test: cfg.show_test,
            sound: cfg.sound,
            include_logs: cfg.include_logs,
            local: LocalPanel::new(base.clone()),
            config_path,
            base,
            focus: Focus::Alba,
            prev_focus: Focus::Alba,
            dialog: None,
            conn: Conn::Offline,
            login_attempts: 0,
            cache,
            cache_dirty: false,
            cache_saved: Instant::now(),
            alba_date: None,
            alba_sel: 0,
            stats_sel: 0,
            marks: HashSet::new(),
            frame_marks: HashMap::new(),
            console: Console::default(),
            remote,
            tx,
            downloads: Downloads::default(),
            next_job: 1,
            next_poll: None,
            pending_notify: HashSet::new(),
            listed_this_session: HashSet::new(),
            scanning: HashSet::new(),
            sel_changed: None,
            status: None,
            alba_note: None,
            remote_root: None,
            experiments_listed: false,
            quit: false,
            cfg,
        };
        app.console
            .open_log(&crate::cache::state_dir(&app.base).join("alba_watch.log"));
        app.console.push(
            LineKind::Info,
            format!(
                "alba_watch {} · base {} · press ? for keys",
                env!("CARGO_PKG_VERSION"),
                app.base.display()
            ),
        );
        if let Some(p) = &app.config_path {
            app.console.push(LineKind::Info, format!("config: {}", p.display()));
        }
        if !app.cache.dates.is_empty() {
            app.console.push(
                LineKind::Info,
                format!("loaded cached listing of {} beamtimes", app.cache.dates.len()),
            );
        }
        if offline {
            app.console
                .push(LineKind::Info, "offline mode: press C to connect");
        } else {
            app.open_login(user.unwrap_or_default());
        }
        app
    }

    // ---------------------------------------------------------------- helpers

    fn say(&mut self, kind: LineKind, text: impl Into<String>) {
        let text = text.into();
        self.status = Some((text.clone(), kind, Instant::now()));
        self.console.push(kind, text);
    }

    pub fn connected(&self) -> bool {
        matches!(self.conn, Conn::Connected(_))
    }

    fn open_login(&mut self, user: String) {
        let field = if user.is_empty() { 0 } else { 1 };
        self.dialog = Some(Dialog::Login(Login {
            user,
            pass: String::new(),
            field,
            busy: false,
            error: None,
        }));
    }

    pub fn info(&self, date: &str, sample: &str) -> Option<&SampleInfo> {
        self.cache.experiments.get(date)?.infos.get(sample)
    }

    pub fn is_test_sample(&self, date: &str, name: &str) -> bool {
        layout::is_test_name(name)
            || self
                .info(date, name)
                .is_some_and(|i| !i.has_data() && i.runs.iter().any(|r| r.characterisation))
    }

    /// Rows of the ALBA panel, like a folder listing: the beamtimes, or the samples
    /// of the opened beamtime behind a `..` row.
    pub fn alba_rows(&self) -> Vec<AlbaRow> {
        let Some(date) = &self.alba_date else {
            return self.cache.dates.iter().cloned().map(AlbaRow::Date).collect();
        };
        let mut rows = vec![AlbaRow::Up];
        if let Some(names) = self.cache.experiments.get(date).and_then(|e| e.samples.as_ref()) {
            for n in names {
                if self.show_test || !self.is_test_sample(date, n) {
                    rows.push(AlbaRow::Sample {
                        date: date.clone(),
                        name: n.clone(),
                    });
                }
            }
        }
        rows
    }

    pub fn selected_row(&self) -> Option<AlbaRow> {
        self.alba_rows().get(self.alba_sel).cloned()
    }

    pub fn selected_sample(&self) -> Option<(String, String)> {
        match self.selected_row()? {
            AlbaRow::Sample { date, name } => Some((date, name)),
            AlbaRow::Date(_) | AlbaRow::Up => None,
        }
    }

    pub fn stats_rows(info: &SampleInfo) -> Vec<StatsRow> {
        let mut rows = Vec::new();
        for (ri, run) in info.runs.iter().enumerate() {
            for si in 0..run.solves.len() {
                rows.push(StatsRow::Solve { run: ri, solve: si });
            }
        }
        for ri in 0..info.runs.len() {
            rows.push(StatsRow::Frames { run: ri });
        }
        rows
    }

    pub fn frame_key(date: &str, sample: &str, run_key: &str) -> String {
        format!("{date}/{sample}/{run_key}")
    }

    pub fn frames_marked(&self, date: &str, sample: &str, run: &crate::model::Run) -> bool {
        self.frame_marks
            .get(&Self::frame_key(date, sample, &run.key()))
            .copied()
            .unwrap_or(!run.characterisation)
    }

    fn store_info(&mut self, date: &str, info: SampleInfo) {
        let exp = self.cache.experiment_mut(date);
        exp.infos.insert(info.name.clone(), info);
        self.cache_dirty = true;
    }

    fn request_scan(&mut self, date: &str, sample: &str, background: bool) {
        if !self.connected() {
            return;
        }
        let key = (date.to_string(), sample.to_string());
        if !background && self.scanning.contains(&key) {
            return;
        }
        self.scanning.insert(key);
        let flat = self.cache.experiments.get(date).is_some_and(|e| e.flat);
        self.remote.send(RemoteCmd::ScanSample {
            date: date.to_string(),
            sample: sample.to_string(),
            flat,
            known: self.info(date, sample).cloned(),
            background,
        });
    }

    fn list_samples(&mut self, date: &str, from_poll: bool) {
        if self.connected() {
            self.remote.send(RemoteCmd::ListSamples {
                date: date.to_string(),
                from_poll,
            });
        }
    }

    fn reschedule_poll(&mut self) {
        self.next_poll = self
            .cfg
            .poll_interval(self.poll_mode)
            .map(|d| Instant::now() + d);
    }

    // ---------------------------------------------------------------- events

    pub fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Input(Event::Key(k)) => self.on_key(k),
            AppEvent::Input(_) => {}
            AppEvent::Remote(e) => {
                let trace: String = format!("{e:?}").chars().take(300).collect();
                self.console.log_only(&trace);
                self.on_remote(e);
            }
            AppEvent::Console(e) => self.on_console(e),
        }
    }

    pub fn tick(&mut self) {
        if self.connected()
            && let Some(at) = self.next_poll
            && Instant::now() >= at
        {
            self.poll_now();
            self.reschedule_poll();
        }
        if let Some(t) = self.sel_changed
            && t.elapsed() >= SCAN_DEBOUNCE
        {
            self.sel_changed = None;
            self.auto_scan_selected();
        }
        if self.cache_dirty && self.cache_saved.elapsed() >= Duration::from_secs(2) {
            self.save_cache();
        }
    }

    pub fn save_cache(&mut self) {
        if let Err(e) = self.cache.save(&self.base) {
            self.console
                .push(LineKind::Error, format!("cannot save cache: {e}"));
        }
        self.cache_dirty = false;
        self.cache_saved = Instant::now();
    }

    pub fn shutdown(&mut self) {
        self.save_cache();
        self.console.kill_running();
        self.remote.send(RemoteCmd::Shutdown);
    }

    fn poll_now(&mut self) {
        self.remote.send(RemoteCmd::ListExperiments);
        let newest = self.cache.dates.first().cloned();
        if let Some(d) = &newest {
            self.list_samples(d, true);
        }
        if let Some(open) = self.alba_date.clone()
            && Some(&open) != newest.as_ref()
        {
            self.list_samples(&open, true);
        }
    }

    fn auto_scan_selected(&mut self) {
        let Some((date, name)) = self.selected_sample() else {
            return;
        };
        let stale = match self.info(&date, &name) {
            None => true,
            Some(i) => i.has_running() && now_secs().saturating_sub(i.scanned_at) > RESCAN_RUNNING_SECS,
        };
        if stale {
            self.request_scan(&date, &name, false);
        }
    }

    fn on_remote(&mut self, e: RemoteEvent) {
        match e {
            RemoteEvent::Connected { user, host_key, root } => {
                let first = !matches!(self.conn, Conn::Reconnecting(_));
                self.conn = Conn::Connected(user.clone());
                self.alba_note = None;
                if matches!(self.dialog, Some(Dialog::Login(_))) {
                    self.dialog = None;
                }
                match host_key {
                    HostKey::Added { fingerprint } => self.console.push(
                        LineKind::Info,
                        format!("trusted host key {fingerprint} on first use"),
                    ),
                    HostKey::Unchecked => self
                        .console
                        .push(LineKind::Warn, "host key not checked (no config dir)"),
                    _ => {}
                }
                self.say(
                    LineKind::Good,
                    format!("connected as {user} to {}:{}", self.cfg.host, self.cfg.port),
                );
                self.console
                    .push(LineKind::Info, format!("proposal folder: {root}"));
                self.remote_root = Some(root);
                self.remote.send(RemoteCmd::ListExperiments);
                if first {
                    self.reschedule_poll();
                }
            }
            RemoteEvent::ConnectFailed { auth, message } => {
                if let Some(Dialog::Login(l)) = &mut self.dialog {
                    l.busy = false;
                    l.pass.clear();
                    if auth {
                        self.login_attempts += 1;
                        if self.login_attempts >= MAX_LOGIN_ATTEMPTS {
                            self.dialog = Some(Dialog::Message {
                                title: "Login failed".into(),
                                text: format!(
                                    "{MAX_LOGIN_ATTEMPTS} failed login attempts.\n\
                                     alba_watch stops here so the account doesn't get locked.\n\n\
                                     Press any key to exit."
                                ),
                                exit: true,
                            });
                        } else {
                            l.error = Some(format!(
                                "{message} (attempt {}/{MAX_LOGIN_ATTEMPTS})",
                                self.login_attempts
                            ));
                        }
                    } else {
                        l.error = Some(message);
                    }
                    self.conn = Conn::Offline;
                } else {
                    self.conn = Conn::Lost(message.clone());
                    self.say(LineKind::Error, format!("connection lost: {message}"));
                }
            }
            RemoteEvent::Disconnected { message } => {
                self.conn = Conn::Reconnecting(None);
                self.scanning.clear();
                self.say(LineKind::Warn, format!("disconnected: {message}"));
            }
            RemoteEvent::Reconnecting { in_secs } => {
                self.conn = Conn::Reconnecting(Some(in_secs));
            }
            RemoteEvent::Experiments { ids, home, top } => {
                if ids.is_empty() {
                    let mut shown: Vec<String> = top.iter().take(15).cloned().collect();
                    if top.len() > 15 {
                        shown.push("…".into());
                    }
                    let hint = match &self.config_path {
                        Some(p) => format!("set remote_root in {}", p.display()),
                        None => "set remote_root in config.toml".into(),
                    };
                    self.alba_note = Some(format!(
                        "No beamtime folders (YYYYMMDD) in {home}. If your data is elsewhere, {hint}."
                    ));
                    self.say(
                        LineKind::Warn,
                        format!("no beamtime folders in {home}; it contains: {}", shown.join(", ")),
                    );
                    self.cache.set_dates(Vec::new());
                    self.cache_dirty = true;
                    self.experiments_listed = true;
                    self.clamp_alba();
                    return;
                }
                self.alba_note = None;
                let before: Vec<String> = self.cache.dates.clone();
                self.cache.set_dates(ids);
                self.cache_dirty = true;
                if !self.experiments_listed {
                    self.experiments_listed = true;
                    self.console.push(
                        LineKind::Info,
                        format!("{} beamtimes: {}", self.cache.dates.len(), self.cache.dates.join(" ")),
                    );
                    if self
                        .alba_date
                        .as_ref()
                        .is_some_and(|d| !self.cache.dates.contains(d))
                    {
                        self.alba_date = None;
                    }
                    // List the newest beamtime once, as the baseline for new-sample alerts.
                    if let Some(d) = self.cache.dates.first().cloned() {
                        self.list_samples(&d, false);
                    }
                } else {
                    let new: Vec<String> = self
                        .cache
                        .dates
                        .iter()
                        .filter(|d| !before.contains(d))
                        .cloned()
                        .collect();
                    for d in new {
                        self.say(LineKind::Good, format!("new beamtime folder on ALBA: {d}"));
                        self.list_samples(&d, false);
                    }
                }
                self.clamp_alba();
            }
            RemoteEvent::ExperimentsFailed(message) => {
                self.alba_note = Some(format!("Listing beamtimes failed: {message}"));
                self.say(LineKind::Error, format!("listing beamtimes failed: {message}"));
            }
            RemoteEvent::Samples {
                date,
                mut names,
                flat,
                from_poll,
            } => {
                names.sort_by_key(|n| n.to_lowercase());
                let baseline = self.listed_this_session.insert(date.clone());
                let exp = self.cache.experiment_mut(&date);
                let old = exp.samples.clone().unwrap_or_default();
                exp.infos.retain(|k, _| names.contains(k));
                exp.samples = Some(names.clone());
                exp.flat = flat;
                self.cache_dirty = true;
                if !baseline {
                    for n in names.iter().filter(|n| !old.contains(n)) {
                        if layout::is_test_name(n) {
                            continue;
                        }
                        self.console
                            .push(LineKind::Info, format!("new sample folder: {date}/{n}"));
                        self.pending_notify.insert((date.clone(), n.clone()));
                    }
                }
                if from_poll {
                    let mut rescan: Vec<String> = self
                        .pending_notify
                        .iter()
                        .filter(|(d, _)| *d == date)
                        .map(|(_, n)| n.clone())
                        .collect();
                    if let Some(exp) = self.cache.experiments.get(&date) {
                        rescan.extend(
                            exp.infos
                                .values()
                                .filter(|i| i.has_running())
                                .map(|i| i.name.clone()),
                        );
                    }
                    rescan.sort();
                    rescan.dedup();
                    for n in rescan {
                        self.request_scan(&date, &n, true);
                    }
                }
                self.clamp_alba();
            }
            RemoteEvent::SampleProgress { date, info } => {
                self.store_info(&date, info);
            }
            RemoteEvent::SampleScanned { date, info } => {
                let key = (date.clone(), info.name.clone());
                self.scanning.remove(&key);
                let has_data = info.has_data();
                let name = info.name.clone();
                self.store_info(&date, info);
                if has_data && self.pending_notify.remove(&key) {
                    self.say(LineKind::Good, format!("♪ new sample with results: {date}/{name}"));
                    if self.sound {
                        notify::beep();
                    }
                }
                self.clamp_stats();
            }
            RemoteEvent::ScanFailed { date, sample, message } => {
                self.scanning.remove(&(date, sample.clone()));
                self.say(LineKind::Error, format!("scan of {sample} failed: {message}"));
            }
            RemoteEvent::Frames { date, sample, files } => self.confirm_frames(date, sample, files),
            RemoteEvent::Download(d) => self.on_download(d),
            RemoteEvent::Error(m) => self.say(LineKind::Error, m),
        }
    }

    fn on_download(&mut self, d: DownloadEvent) {
        match d {
            DownloadEvent::Started { id, label, total } => {
                self.downloads.active = Some(Active {
                    id,
                    label,
                    done: 0,
                    total,
                    started: Instant::now(),
                });
            }
            DownloadEvent::Progress { id, done, total } => {
                if let Some(a) = &mut self.downloads.active
                    && a.id == id
                {
                    a.done = done;
                    a.total = total;
                }
            }
            DownloadEvent::Finished {
                id,
                label,
                bytes,
                skipped,
            } => {
                self.downloads.queued = self.downloads.queued.saturating_sub(1);
                self.downloads.done += 1;
                if self.downloads.active.as_ref().is_some_and(|a| a.id == id) {
                    self.downloads.active = None;
                }
                if skipped {
                    self.console
                        .push(LineKind::Info, format!("= {label} (already here)"));
                } else {
                    self.downloads.bytes += bytes;
                    self.console
                        .push(LineKind::Good, format!("↓ {label}  {}", human_bytes(bytes)));
                }
                if self.downloads.queued == 0 {
                    self.status = Some(("downloads finished".into(), LineKind::Good, Instant::now()));
                    self.local.refresh();
                }
            }
            DownloadEvent::Failed { id, label, message } => {
                self.downloads.queued = self.downloads.queued.saturating_sub(1);
                self.downloads.failed += 1;
                if self.downloads.active.as_ref().is_some_and(|a| a.id == id) {
                    self.downloads.active = None;
                }
                self.say(LineKind::Error, format!("✗ {label}: {message}"));
                if self.downloads.queued == 0 {
                    self.local.refresh();
                }
            }
        }
    }

    fn on_console(&mut self, e: ConsoleEvent) {
        match e {
            ConsoleEvent::Line(kind, text) => self.console.push(kind, text),
            ConsoleEvent::Exited { label, code, external } => {
                if !external {
                    self.console.running = None;
                }
                let kind = if code == Some(0) { LineKind::Info } else { LineKind::Warn };
                let code = code.map(|c| c.to_string()).unwrap_or_else(|| "?".into());
                self.console
                    .push(kind, format!("[{label} exited with code {code}]"));
                self.local.refresh();
            }
        }
    }

    // ---------------------------------------------------------------- keys

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if self.dialog.is_some() {
            self.dialog_key(key);
            return;
        }
        if self.focus == Focus::Console && self.console_key(key) {
            return;
        }
        let shift_tab = key.code == KeyCode::BackTab;
        match key.code {
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Tab => self.set_focus(self.focus.next()),
            _ if shift_tab => self.set_focus(self.focus.prev()),
            KeyCode::Char('?') => self.dialog = Some(Dialog::Help),
            KeyCode::Char(':') => self.set_focus(Focus::Console),
            KeyCode::Char('p') => self.cycle_poll(),
            KeyCode::Char('n') => {
                self.sound = !self.sound;
                let s = if self.sound { "on" } else { "off" };
                self.say(LineKind::Info, format!("new-sample sound {s}"));
            }
            KeyCode::Char('L') => {
                self.include_logs = !self.include_logs;
                let s = if self.include_logs { "with" } else { "without" };
                self.say(LineKind::Info, format!("downloads now {s} logs"));
            }
            KeyCode::Char('t') => {
                self.show_test = !self.show_test;
                let s = if self.show_test { "shown" } else { "hidden" };
                self.say(LineKind::Info, format!("test / characterisation-only samples {s}"));
                self.clamp_alba();
            }
            KeyCode::Char('X') => {
                if self.downloads.queued > 0 {
                    self.cancel_downloads();
                    self.say(LineKind::Warn, "cancelling queued downloads…");
                }
            }
            KeyCode::Char('C') => {
                if self.connected() {
                    self.say(LineKind::Info, "already connected");
                } else if self.login_attempts >= MAX_LOGIN_ATTEMPTS {
                    self.say(LineKind::Error, "too many failed logins this session");
                } else {
                    self.open_login(String::new());
                }
            }
            _ => match self.focus {
                Focus::Local => self.local_key(key),
                Focus::Alba => self.alba_key(key),
                Focus::Stats => self.stats_key(key),
                Focus::Console => {}
            },
        }
    }

    fn set_focus(&mut self, f: Focus) {
        if f == Focus::Console && self.focus != Focus::Console {
            self.prev_focus = self.focus;
        }
        self.focus = f;
    }

    fn request_quit(&mut self) {
        let mut lines = Vec::new();
        if self.downloads.queued > 0 {
            lines.push(format!(
                "{} download(s) still queued or running will be stopped.",
                self.downloads.queued
            ));
        }
        if let Some(r) = &self.console.running {
            lines.push(format!("`{}` is still running in the console and will be stopped.", r.label));
        }
        lines.push("Quit alba_watch? (y/n)".into());
        self.dialog = Some(Dialog::Confirm {
            title: "Quit".into(),
            lines,
            action: Pending::Quit,
        });
    }

    fn cycle_poll(&mut self) {
        self.poll_mode = self.poll_mode.next();
        self.reschedule_poll();
        let text = match self.cfg.poll_interval(self.poll_mode) {
            None => "polling: manual (press r to refresh)".to_string(),
            Some(d) => format!(
                "polling: {:?} — newest beamtime every {} min",
                self.poll_mode,
                (d.as_secs_f64() / 60.0).round()
            )
            .to_lowercase(),
        };
        self.say(LineKind::Info, text);
    }

    fn dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        match dialog {
            Dialog::Login(mut l) => {
                if l.busy {
                    self.dialog = Some(Dialog::Login(l));
                    return;
                }
                match key.code {
                    KeyCode::Esc => {
                        self.conn = Conn::Offline;
                        self.console.push(
                            LineKind::Info,
                            "offline: browsing cached data and local files. Press C to connect.",
                        );
                        return;
                    }
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => l.field = 1 - l.field,
                    KeyCode::Enter => {
                        if l.field == 0 && !l.user.is_empty() {
                            l.field = 1;
                        } else if !l.user.trim().is_empty() && !l.pass.is_empty() {
                            l.busy = true;
                            l.error = None;
                            self.conn = Conn::Connecting;
                            self.remote.send(RemoteCmd::Connect {
                                user: l.user.trim().to_string(),
                                password: std::mem::take(&mut l.pass),
                            });
                        }
                    }
                    KeyCode::Backspace => {
                        if l.field == 0 {
                            l.user.pop();
                        } else {
                            l.pass.pop();
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if l.field == 0 {
                            l.user.push(c);
                        } else {
                            l.pass.push(c);
                        }
                    }
                    _ => {}
                }
                self.dialog = Some(Dialog::Login(l));
            }
            Dialog::Input {
                title,
                mut value,
                action,
            } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => match action {
                    InputAction::NewFolder => match self.local.make_dir(&value) {
                        Ok(()) => self.say(LineKind::Good, format!("created folder {}", value.trim())),
                        Err(e) => {
                            self.say(LineKind::Error, e);
                            self.dialog = Some(Dialog::Input { title, value, action });
                        }
                    },
                },
                KeyCode::Backspace => {
                    value.pop();
                    self.dialog = Some(Dialog::Input { title, value, action });
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    value.push(c);
                    self.dialog = Some(Dialog::Input { title, value, action });
                }
                _ => self.dialog = Some(Dialog::Input { title, value, action }),
            },
            Dialog::Confirm { title, lines, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => match action {
                    Pending::Quit => self.quit = true,
                    Pending::Frames { date, sample, files } => self.enqueue_frames(&date, &sample, files),
                },
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                _ => self.dialog = Some(Dialog::Confirm { title, lines, action }),
            },
            Dialog::Help => {}
            Dialog::Message { exit, .. } => {
                if exit {
                    self.quit = true;
                }
            }
        }
    }

    /// Returns true when the console consumed the key.
    fn console_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => return false,
            KeyCode::Esc => self.focus = self.prev_focus,
            KeyCode::Char('c') if ctrl => {
                if self.console.running.is_some() {
                    self.console.kill_running();
                } else {
                    self.console.input.clear();
                    self.console.home();
                }
            }
            KeyCode::Char('l') if ctrl => self.console.lines.clear(),
            KeyCode::Char(c) if !ctrl => self.console.insert_char(c),
            KeyCode::Enter => {
                let cwd = self.local.cwd.clone();
                let cfg = self.cfg.clone();
                let selected = self.local.selected_file().map(str::to_string);
                match self
                    .console
                    .submit(&cwd, selected.as_deref(), |p| cfg.is_external(p), &self.tx)
                {
                    Some(Builtin::Cd(dir)) => {
                        let target = if dir.is_empty() {
                            self.base.clone()
                        } else {
                            cwd.join(dir)
                        };
                        self.local.cd(target);
                        if let Some(e) = self.local.error.take() {
                            self.console.push(LineKind::Error, e);
                        }
                    }
                    Some(Builtin::Clear) => self.console.lines.clear(),
                    Some(Builtin::Help) => self.dialog = Some(Dialog::Help),
                    None => {}
                }
            }
            KeyCode::Backspace => self.console.backspace(),
            KeyCode::Delete => self.console.delete(),
            KeyCode::Left => self.console.left(),
            KeyCode::Right => self.console.right(),
            KeyCode::Home => self.console.home(),
            KeyCode::End => self.console.end(),
            KeyCode::Up => self.console.history_prev(),
            KeyCode::Down => self.console.history_next(),
            KeyCode::PageUp => self.console.scroll_up(10),
            KeyCode::PageDown => self.console.scroll_down(10),
            _ => return false,
        }
        true
    }

    fn local_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.local.move_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.local.move_by(1),
            KeyCode::PageUp => self.local.move_by(-10),
            KeyCode::PageDown => self.local.move_by(10),
            KeyCode::Home | KeyCode::Char('g') => self.local.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.local.select_last(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.local.enter();
            }
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.local.up(),
            KeyCode::Char('r') => self.local.refresh(),
            KeyCode::Char('b') | KeyCode::Char('~') => self.local.cd(self.base.clone()),
            KeyCode::Char('N') | KeyCode::F(7) => {
                self.dialog = Some(Dialog::Input {
                    title: format!("New folder in {}", self.local.cwd.display()),
                    value: String::new(),
                    action: InputAction::NewFolder,
                });
            }
            KeyCode::Char('o') => {
                if let Some(p) = self.local.selected_path() {
                    open_with_default_app(&p);
                    self.say(LineKind::Info, format!("opening {}", p.display()));
                }
            }
            _ => {}
        }
        if let Some(e) = self.local.error.take() {
            self.say(LineKind::Error, e);
        }
    }

    fn clamp_alba(&mut self) {
        let n = self.alba_rows().len();
        self.alba_sel = self.alba_sel.min(n.saturating_sub(1));
    }

    fn clamp_stats(&mut self) {
        let n = self
            .selected_sample()
            .and_then(|(d, s)| self.info(&d, &s).map(|i| Self::stats_rows(i).len()))
            .unwrap_or(0);
        self.stats_sel = self.stats_sel.min(n.saturating_sub(1));
    }

    fn move_alba(&mut self, delta: isize) {
        let n = self.alba_rows().len();
        if n == 0 {
            return;
        }
        let new = (self.alba_sel as isize + delta).clamp(0, n as isize - 1) as usize;
        if new != self.alba_sel {
            self.alba_sel = new;
            self.stats_sel = 0;
            self.sel_changed = Some(Instant::now());
        }
    }

    fn alba_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.move_alba(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_alba(1),
            KeyCode::PageUp => self.move_alba(-10),
            KeyCode::PageDown => self.move_alba(10),
            KeyCode::Home | KeyCode::Char('g') => self.move_alba(-(self.alba_sel as isize)),
            KeyCode::End | KeyCode::Char('G') => self.move_alba(isize::MAX / 2),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => match self.selected_row() {
                Some(AlbaRow::Date(d)) => self.open_date(&d),
                Some(AlbaRow::Up) => self.close_date(),
                Some(AlbaRow::Sample { date, name }) => {
                    if self.info(&date, &name).is_none() {
                        self.request_scan(&date, &name, false);
                    }
                    self.focus = Focus::Stats;
                }
                None => {}
            },
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => self.close_date(),
            KeyCode::Char('s') => self.rescan_selected(),
            KeyCode::Char('S') => self.fetch_all(),
            KeyCode::Char('r') => self.refresh_remote(),
            _ => {}
        }
    }

    /// Opens a beamtime like a folder: only its samples are shown.
    fn open_date(&mut self, date: &str) {
        self.alba_date = Some(date.to_string());
        let listed = self
            .cache
            .experiments
            .get(date)
            .is_some_and(|e| e.samples.is_some());
        if !listed {
            if self.connected() {
                self.list_samples(date, false);
            } else {
                self.say(LineKind::Warn, "offline: this beamtime was never listed");
            }
        }
        self.alba_sel = if self.alba_rows().len() > 1 { 1 } else { 0 };
        self.stats_sel = 0;
        self.sel_changed = Some(Instant::now());
    }

    /// Back to the beamtime list, with the beamtime we came from selected.
    fn close_date(&mut self) {
        let Some(date) = self.alba_date.take() else {
            return;
        };
        self.alba_sel = self.cache.dates.iter().position(|d| *d == date).unwrap_or(0);
        self.stats_sel = 0;
    }

    /// The beamtime the ALBA panel is in, or the highlighted one in the list.
    fn current_date(&self) -> Option<String> {
        self.alba_date.clone().or_else(|| match self.selected_row()? {
            AlbaRow::Date(d) => Some(d),
            _ => None,
        })
    }

    fn rescan_selected(&mut self) {
        if !self.connected() {
            self.say(LineKind::Warn, "offline: press C to connect");
            return;
        }
        if let Some((date, name)) = self.selected_sample() {
            self.request_scan(&date, &name, false);
            self.say(LineKind::Info, format!("scanning {name}…"));
        }
    }

    fn fetch_all(&mut self) {
        if !self.connected() {
            self.say(LineKind::Warn, "offline: press C to connect");
            return;
        }
        let Some(date) = self.current_date() else {
            return;
        };
        let todo: Vec<String> = self
            .alba_rows()
            .into_iter()
            .filter_map(|r| match r {
                AlbaRow::Sample { date: d, name } if d == date => Some(name),
                _ => None,
            })
            .filter(|n| self.info(&date, n).is_none_or(|i| i.has_running()))
            .collect();
        if todo.is_empty() {
            self.say(LineKind::Info, format!("all samples of {date} already have stats"));
            return;
        }
        let n = todo.len();
        for name in todo {
            self.request_scan(&date, &name, true);
        }
        self.say(
            LineKind::Info,
            format!("fetching stats for {n} samples of {date} in the background (one every 3 s)"),
        );
    }

    fn refresh_remote(&mut self) {
        if !self.connected() {
            self.say(LineKind::Warn, "offline: press C to connect");
            return;
        }
        self.remote.send(RemoteCmd::ListExperiments);
        if let Some(d) = self.current_date().or_else(|| self.cache.dates.first().cloned()) {
            self.list_samples(&d, true);
            self.say(LineKind::Info, format!("refreshing {d}"));
        }
    }

    fn stats_key(&mut self, key: KeyEvent) {
        let Some((date, sample)) = self.selected_sample() else {
            return;
        };
        let Some(info) = self.info(&date, &sample).cloned() else {
            if key.code == KeyCode::Char('s') {
                self.rescan_selected();
            }
            return;
        };
        let rows = Self::stats_rows(&info);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.stats_sel = self.stats_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.stats_sel = (self.stats_sel + 1).min(rows.len().saturating_sub(1))
            }
            KeyCode::Home | KeyCode::Char('g') => self.stats_sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.stats_sel = rows.len().saturating_sub(1),
            KeyCode::Char(' ') => match rows.get(self.stats_sel) {
                Some(StatsRow::Solve { run, solve }) => {
                    let k = info.runs[*run].solves[*solve].key();
                    if !self.marks.remove(&k) {
                        self.marks.insert(k);
                    }
                    self.stats_sel = (self.stats_sel + 1).min(rows.len().saturating_sub(1));
                }
                Some(StatsRow::Frames { run }) => {
                    let r = &info.runs[*run];
                    let now = self.frames_marked(&date, &sample, r);
                    self.frame_marks
                        .insert(Self::frame_key(&date, &sample, &r.key()), !now);
                }
                None => {}
            },
            KeyCode::Char('a') => {
                let keys: Vec<String> = info
                    .solves()
                    .filter(|s| !s.hkl.is_empty())
                    .map(Solve::key)
                    .collect();
                if keys.iter().all(|k| self.marks.contains(k)) {
                    for k in &keys {
                        self.marks.remove(k);
                    }
                } else {
                    self.marks.extend(keys);
                }
            }
            KeyCode::Char('d') => self.download_solves(&date, &info),
            KeyCode::Char('F') => self.request_frames(&date, &info),
            KeyCode::Char('s') => self.rescan_selected(),
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Alba,
            _ => {}
        }
    }

    // ---------------------------------------------------------------- downloads

    fn download_solves(&mut self, date: &str, info: &SampleInfo) {
        if !self.connected() {
            self.say(LineKind::Warn, "offline: press C to connect");
            return;
        }
        let mut picked: Vec<(usize, &Solve)> = info
            .runs
            .iter()
            .enumerate()
            .flat_map(|(ri, r)| r.solves.iter().map(move |s| (ri, s)))
            .filter(|(_, s)| self.marks.contains(&s.key()))
            .collect();
        if picked.is_empty()
            && let Some(StatsRow::Solve { run, solve }) = Self::stats_rows(info).get(self.stats_sel)
        {
            picked.push((*run, &info.runs[*run].solves[*solve]));
        }
        if picked.is_empty() {
            self.say(LineKind::Warn, "mark solves with Space (or highlight one), then press d");
            return;
        }
        let multi = info.distinct_prefixes() > 1;
        let cwd = self.local.cwd.clone();
        let mut jobs = Vec::new();
        let mut folders = Vec::new();
        let mut total: u64 = 0;
        for (ri, solve) in picked {
            let run = &info.runs[ri];
            let mut files = solve.hkl.clone();
            if self.include_logs {
                files.extend(solve.logs.iter().cloned());
            }
            if solve.hkl.is_empty() {
                let why = solve.note.clone().unwrap_or_else(|| "no HKL file on the server".into());
                self.console
                    .push(LineKind::Warn, format!("{} run {}: {why}", solve.kind.label(), run.run));
            }
            if files.is_empty() {
                continue;
            }
            let tag = download::solve_tag(solve, run, multi);
            let dir = download::solve_dir(&cwd, &tag, date, &info.name);
            let what = solve.kind.label().to_string();
            let Some(mut batch) = self.plan_folder(&dir, &files, date, &info.name, &what, run.run) else {
                continue;
            };
            total += files.iter().filter_map(|f| f.size).sum::<u64>();
            folders.push(folder_label(&cwd, &dir));
            jobs.append(&mut batch);
            self.marks.remove(&solve.key());
        }
        self.enqueue(jobs, total, &folders);
    }

    /// Jobs for `files` into `dir`, recording their origin in `dir/.alba_source.json`.
    fn plan_folder(
        &mut self,
        dir: &std::path::Path,
        files: &[RemoteFile],
        date: &str,
        sample: &str,
        what: &str,
        run: u32,
    ) -> Option<Vec<DownloadJob>> {
        let mut sources = download::read_sources(dir);
        let mut taken = HashSet::new();
        let folder = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let mut jobs = Vec::new();
        for f in files {
            let name = download::local_name(dir, &sources, &taken, &f.path, &folder);
            taken.insert(name.clone());
            sources.files.insert(
                name.clone(),
                Source {
                    date: date.to_string(),
                    sample: sample.to_string(),
                    remote: f.path.clone(),
                    what: what.to_string(),
                    run,
                    downloaded_at: now_secs(),
                },
            );
            jobs.push(self.job(f, dir.join(&name), format!("{folder}/{name}")));
        }
        if let Err(e) = download::write_sources(dir, &sources) {
            self.say(LineKind::Error, format!("cannot create {}: {e}", dir.display()));
            return None;
        }
        Some(jobs)
    }

    fn job(&mut self, f: &RemoteFile, local: PathBuf, label: String) -> DownloadJob {
        let id = self.next_job;
        self.next_job += 1;
        DownloadJob {
            id,
            remote: f.path.clone(),
            local,
            size: f.size,
            label: label.replace('\\', "/"),
        }
    }

    fn enqueue(&mut self, jobs: Vec<DownloadJob>, total: u64, folders: &[String]) {
        if jobs.is_empty() {
            return;
        }
        let n = jobs.len();
        self.downloads.queued += n;
        self.remote.send(RemoteCmd::Download(jobs));
        let size = if total > 0 {
            format!(" ({}+)", human_bytes(total))
        } else {
            String::new()
        };
        self.say(
            LineKind::Info,
            format!("queued {n} file(s){size} into {}", folders.join(", ")),
        );
    }

    fn request_frames(&mut self, date: &str, info: &SampleInfo) {
        if !self.connected() {
            self.say(LineKind::Warn, "offline: press C to connect");
            return;
        }
        let runs: Vec<(String, u32)> = info
            .runs
            .iter()
            .filter(|r| self.frames_marked(date, &info.name, r))
            .map(|r| (r.prefix.clone(), r.run))
            .collect();
        if runs.is_empty() {
            self.say(LineKind::Warn, "no frame runs ticked (Space on a 'frames' row)");
            return;
        }
        self.remote.send(RemoteCmd::ListFrames {
            date: date.to_string(),
            sample: info.name.clone(),
            runs,
        });
        self.say(LineKind::Info, format!("listing raw frames of {}…", info.name));
    }

    fn confirm_frames(&mut self, date: String, sample: String, files: Vec<RemoteFile>) {
        if files.is_empty() {
            self.say(LineKind::Warn, format!("no frames found for {sample}"));
            return;
        }
        let total: u64 = files.iter().filter_map(|f| f.size).sum();
        let mut lines = vec![format!("{} frames, {} in total", files.len(), human_bytes(total))];
        if let Some(info) = self.info(&date, &sample) {
            for r in &info.runs {
                let n = files
                    .iter()
                    .filter(|f| layout::is_frame_of(f.name(), &r.prefix, r.run))
                    .count();
                if n > 0 {
                    let kind = if r.characterisation { " (characterisation)" } else { "" };
                    lines.push(format!("  run {}{kind}: {n} frames", r.run));
                }
            }
        }
        lines.push(String::new());
        lines.push(format!("into {}{}frames_r…", self.local.cwd.display(), std::path::MAIN_SEPARATOR));
        lines.push(String::new());
        lines.push("Download? (y/n)".into());
        self.dialog = Some(Dialog::Confirm {
            title: format!("Raw frames: {sample}"),
            lines,
            action: Pending::Frames { date, sample, files },
        });
    }

    fn enqueue_frames(&mut self, date: &str, sample: &str, files: Vec<RemoteFile>) {
        let Some(info) = self.info(date, sample).cloned() else {
            return;
        };
        let cwd = self.local.cwd.clone();
        let multi = info.distinct_prefixes() > 1;
        let mut jobs = Vec::new();
        let mut folders = Vec::new();
        let mut total = 0;
        for r in &info.runs {
            let mine: Vec<RemoteFile> = files
                .iter()
                .filter(|f| layout::is_frame_of(f.name(), &r.prefix, r.run))
                .cloned()
                .collect();
            if mine.is_empty() {
                continue;
            }
            let dir = download::solve_dir(&cwd, &download::frames_tag(r, multi), date, sample);
            let Some(mut batch) = self.plan_folder(&dir, &mine, date, sample, "frames", r.run) else {
                continue;
            };
            total += mine.iter().filter_map(|f| f.size).sum::<u64>();
            folders.push(folder_label(&cwd, &dir));
            jobs.append(&mut batch);
        }
        self.enqueue(jobs, total, &folders);
    }

    pub fn cancel_downloads(&mut self) {
        self.remote.send(RemoteCmd::CancelDownloads);
    }

    pub fn status_text(&self) -> Option<(&str, LineKind)> {
        self.status
            .as_ref()
            .filter(|(_, _, t)| t.elapsed() < Duration::from_secs(8))
            .map(|(s, k, _)| (s.as_str(), *k))
    }

}

/// `xia2chem_r3/` relative to the Local panel's folder, for messages.
fn folder_label(cwd: &std::path::Path, dir: &std::path::Path) -> String {
    let rel = dir.strip_prefix(cwd).unwrap_or(dir).display().to_string();
    format!("{rel}{}", std::path::MAIN_SEPARATOR)
}

fn open_with_default_app(path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!("/C start \"\" \"{}\"", path.display()))
            .creation_flags(0x0800_0000)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// Drives the real App + worker through login and the first listings, headless.
/// `ALBA_USER=… ALBA_PASS=… cargo test live_app_flow -- --ignored --nocapture`
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn live_app_flow() {
        let (Ok(user), Ok(pass)) = (std::env::var("ALBA_USER"), std::env::var("ALBA_PASS")) else {
            panic!("set ALBA_USER and ALBA_PASS");
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let remote = crate::remote::spawn(
            &rt,
            crate::remote::Settings {
                host: "userdata.cells.es".into(),
                port: 443,
                rate: 3.0,
                burst: 6,
                known_hosts: Some(std::env::temp_dir().join("alba_watch_live_known_hosts")),
                beamline: "bl13".into(),
                remote_root: None,
            },
            tx.clone(),
        );
        let base = std::env::temp_dir().join(format!("alba_watch_flow_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = App::new(Config::default(), None, base.clone(), remote, tx, Some(user.clone()), false);
        if let Some(Dialog::Login(l)) = &mut app.dialog {
            l.pass = pass;
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(40) {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                println!("[{:>5.1}s] event: {}", t.elapsed().as_secs_f64(), format!("{ev:?}").chars().take(160).collect::<String>());
                app.handle(ev);
            }
            app.tick();
            let listed = app
                .cache
                .dates
                .first()
                .and_then(|d| app.cache.experiments.get(d))
                .is_some_and(|e| e.samples.is_some());
            if listed {
                break;
            }
        }
        println!("conn: {:?}\ndates: {:?}\nrows: {}", app.conn, app.cache.dates, app.alba_rows().len());

        // Optional: open a beamtime, scan one sample, download its best solve into the temp folder.
        if let (Ok(date), Ok(sample)) = (std::env::var("ALBA_DATE"), std::env::var("ALBA_SAMPLE")) {
            app.open_date(&date);
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(30)
                && app.cache.experiments.get(&date).is_none_or(|e| e.samples.is_none())
            {
                if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                    app.handle(ev);
                }
            }
            println!("opened {date}: {} rows, first {:?}", app.alba_rows().len(), app.alba_rows().get(1));
            let row = app
                .alba_rows()
                .iter()
                .position(|r| matches!(r, AlbaRow::Sample { name, .. } if *name == sample))
                .expect("sample row");
            app.alba_sel = row;
            app.request_scan(&date, &sample, false);
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(60) && app.scanning.contains(&(date.clone(), sample.clone())) {
                if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                    app.handle(ev);
                }
            }
            let info = app.info(&date, &sample).cloned().expect("scanned");
            let best = crate::rank::best(&info);
            println!("best: {best:?}");
            if std::env::var("ALBA_DOWNLOAD").is_ok()
                && let Some((key, _)) = best
            {
                app.marks.insert(key);
                app.download_solves(&date, &info);
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(120) && app.downloads.queued > 0 {
                    if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
                        app.handle(ev);
                    }
                }
                for e in std::fs::read_dir(&base).unwrap().flatten() {
                    println!("local: {} {} bytes", e.file_name().to_string_lossy(), e.metadata().unwrap().len());
                    if e.path().is_dir() {
                        for f in std::fs::read_dir(e.path()).unwrap().flatten() {
                            println!("local:   {} {} bytes", f.file_name().to_string_lossy(), f.metadata().unwrap().len());
                        }
                    }
                }
            }
        }
        for l in &app.console.lines {
            println!("console: {}", l.text);
        }
        app.shutdown();
        std::thread::sleep(Duration::from_millis(500));
        std::fs::remove_dir_all(&base).ok();
    }
}
