//! `config.toml` in the user config dir. Never holds credentials. The same folder
//! holds amanece's other files: `known_hosts`, the caches, `amanece.log` and, only
//! when "remember username" is ticked at login, `login.toml` with the usernames.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Polls never run more often than this, whatever the config says.
pub const MIN_POLL: Duration = Duration::from_secs(5);
/// Below this interval a poll only re-lists the open/newest beamtime; the beamtime
/// list and sample rescans happen at most this often (see `App::poll_now`).
pub const HEAVY_POLL_GAP: Duration = Duration::from_secs(60);
/// Lowest SFTP request rate accepted (a typo like 0.01 would freeze the app).
pub const MIN_REQUESTS_PER_SECOND: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PollMode {
    Manual,
    /// `slow_poll_minutes` (10 min)
    Slow,
    /// `fast_poll_minutes` (2 min)
    Fast,
    /// every 30 s
    #[serde(rename = "30s")]
    Every30s,
    /// every 5 s
    #[serde(rename = "5s")]
    Every5s,
}

impl PollMode {
    pub fn next(self) -> PollMode {
        match self {
            PollMode::Manual => PollMode::Slow,
            PollMode::Slow => PollMode::Fast,
            PollMode::Fast => PollMode::Every30s,
            PollMode::Every30s => PollMode::Every5s,
            PollMode::Every5s => PollMode::Manual,
        }
    }
}

/// "10 min", "30 s" …
pub fn interval_label(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 && s % 60 == 0 {
        format!("{} min", s / 60)
    } else {
        format!("{s} s")
    }
}

/// The layout of a server's folders: a facility amanece knows (`type = "alba"`,
/// `type = "bruker"`), or plain folders (no `type`, or `type = "nas"`). A new facility is
/// a new variant plus its scan code; every `match` on the type then asks for it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ServerKind {
    /// Browse folders one at a time from `root` (or the login folder); cells from
    /// SHELX and XDS headers.
    #[default]
    #[serde(alias = "nas")]
    #[value(alias = "nas")]
    Folders,
    /// ALBA proposal folders: beamtimes by date, autoprocessing results, frames.
    Alba,
    /// The group's Bruker diffractometer: a folder per person, an experiment folder per
    /// sample, cells from the `.p4p` files, `.sfrm` frames split into runs (docs/BRUKER.md).
    Bruker,
}

/// Where a Bruker server's browsing starts when its `root` is not set.
pub const BRUKER_DEFAULT_ROOT: &str = "/xray/data/user";

impl ServerKind {
    /// Beamtime ids are `<source id>/<folder path>` (see `remote::nas::source_id`), not
    /// ALBA dates: the panel shows paths and the id says which server and root it is under.
    pub fn source_ids(self) -> bool {
        matches!(self, ServerKind::Folders | ServerKind::Bruker)
    }

    /// Folders are rows of their own (`name/`, opened with Enter) and the samples are
    /// metadata files: the NAS way. ALBA and Bruker samples are folders.
    pub fn browses_folders(self) -> bool {
        self == ServerKind::Folders
    }

    /// Samples named "test…" are test shots: hidden unless `show_test` (ALBA, Bruker).
    pub fn hides_test_names(self) -> bool {
        matches!(self, ServerKind::Alba | ServerKind::Bruker)
    }
}

/// One server the login dialog can connect to: a built-in one or a `[[servers]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerProfile {
    pub name: String,
    pub host: String,
    /// SFTP port, 22 unless set.
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, rename = "type")]
    pub kind: ServerKind,
    /// Folders: where browsing starts (absolute SFTP path; default the login folder).
    /// ALBA: the proposal folder, if automatic detection doesn't find it.
    #[serde(default, alias = "remote_root")]
    pub root: Option<String>,
}

fn default_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// The active server's address, set at startup and on every server switch
    /// (built-in ALBA until then; not read from the file).
    #[serde(skip)]
    pub host: String,
    #[serde(skip)]
    pub port: u16,
    pub poll_mode: PollMode,
    pub slow_poll_minutes: f64,
    pub fast_poll_minutes: f64,
    pub sound: bool,
    pub include_logs: bool,
    pub show_test: bool,
    /// Programs always started detached in their own window. Windows GUI programs
    /// (xprep, shelxle, olex2, …) are detected automatically and need no entry.
    pub external_window: Vec<String>,
    /// Sustained SFTP requests per second; 0 = no limit. Polite by default, no upper cap.
    pub max_requests_per_second: f64,
    pub burst: u32,
    /// Pause between background scans ("fetch all stats", poll rescans), from the start
    /// of one to the start of the next; 0 = back to back.
    pub background_gap_seconds: f64,
    /// Frames downloaded at a time over the one connection (1–16). HKL/log downloads
    /// always get a slot of their own on top.
    pub parallel_downloads: usize,
    /// Beamline folders searched for the proposal (`/bl13/projects/cycle*/...`):
    /// bl13 = XALOC, bl06 = XAIRA.
    pub beamlines: Vec<String>,
    /// ALBA's proposal folder, if automatic detection doesn't find it. After startup:
    /// the active server's `root`.
    pub remote_root: Option<String>,
    /// The active server's type, set at startup and on every server switch.
    #[serde(skip)]
    pub kind: ServerKind,
    /// Servers of your own (`[[servers]]` at the end of the file), after the built-in ones.
    pub servers: Vec<ServerProfile>,
    /// Local folder amanece always opens in, wherever it was started from.
    pub default_folder: Option<String>,
    /// Limits for the checkCIF-style alerts in the Stats panel.
    pub checkcif: crate::checkcif::Limits,
    /// Unit-cell database: two reduced cells match within this many % on each edge…
    pub cell_match_pct: f64,
    /// …and this many degrees on each angle.
    pub cell_match_deg: f64,
    /// Ask GitHub for a newer amanece at startup and once a day (`U` updates).
    pub check_updates: bool,
    /// CellCheckCSD install folder, when it is not in the default place
    /// (`%ProgramFiles%\CCDC\CellCheckCSD`).
    pub cellcheck_dir: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: "userdata.cells.es".into(),
            port: 443,
            poll_mode: PollMode::Manual,
            slow_poll_minutes: 10.0,
            fast_poll_minutes: 2.0,
            sound: true,
            include_logs: true,
            show_test: false,
            external_window: vec!["xprep".into()],
            max_requests_per_second: 3.0,
            burst: 6,
            background_gap_seconds: 3.0,
            parallel_downloads: 3,
            beamlines: vec!["bl13".into(), "bl06".into()],
            remote_root: None,
            kind: ServerKind::Alba,
            servers: Vec::new(),
            default_folder: None,
            checkcif: crate::checkcif::Limits::default(),
            cell_match_pct: 1.5,
            cell_match_deg: 2.0,
            check_updates: true,
            cellcheck_dir: None,
        }
    }
}

const TEMPLATE: &str = r#"# amanece configuration. Credentials are never stored here.
# ALBA is built in; servers of your own go in [[servers]] at the end of this file.

# "manual", "slow", "fast", "30s" or "5s" at startup (cycle with `p` in the app).
# At 30 s / 5 s a poll only re-lists the open beamtime; the beamtime list and
# sample rescans still happen at most once a minute.
poll_mode = "manual"
slow_poll_minutes = 10.0
fast_poll_minutes = 2.0

# beep when a new named sample gets processing results
sound = true
# also download logs (summary.html, xia2.txt, CORRECT.LP, ...) with the HKL files
include_logs = true
# show samples named "test..." and characterisation-only samples
show_test = false

# programs always started in their own window instead of the console panel.
# Windows GUI programs (xprep, shelxle, olex2, ...) are detected automatically.
external_window = ["xprep"]

# SFTP politeness. The defaults are gentle on shared servers (ALBA is a facility
# server used by many groups); raise them only on a server you may load harder.
# Sustained requests per second (0 = no limit) and burst size:
max_requests_per_second = 3.0
burst = 6
# seconds between background sample scans ("S" fetch all, poll rescans); 0 = back to back
background_gap_seconds = 3.0
# frames downloaded at a time over the one connection (1-16); HKL/log downloads
# always get a slot of their own on top and never wait behind frames
parallel_downloads = 3

# ALBA: where the beamtime folders (YYYYMMDD) live. Normally found automatically:
# either the login folder itself, or /<beamline>/projects/cycle*/<account digits>-<pi>
# on each of these beamlines (bl13 = XALOC, bl06 = XAIRA)
beamlines = ["bl13", "bl06"]
# remote_root = "/bl13/projects/cycle2026-I/<account digits>-<pi>"

# Local folder amanece always opens in, wherever it was started from. Unset, it opens
# where it was when it was last closed (and the folder it was started in the first time).
# default_folder = "C:/Users/you/Documents/alba"

# unit-cell database (cells.toml in this folder): the Stats panel lists the samples
# whose reduced cell matches within this many % on each edge and degrees on each angle
cell_match_pct = 1.5
cell_match_deg = 2.0

# ask GitHub for a newer amanece at startup and once a day (one small request;
# nothing is downloaded until you press U and say yes)
check_updates = true

# CellCheckCSD (free from the CCDC) checks a cell against the CSD, offline. amanece finds
# it in the default install folder; set this if you installed it elsewhere.
# cellcheck_dir = "C:/Program Files/CCDC/CellCheckCSD"

# checkCIF-style alerts in the Stats panel. Each list gives the limits for alert
# levels C, B, A; a value past one turns its cell yellow (C), orange (B) or red (A).
[checkcif]
# THETM01: sin(theta_max)/lambda in 1/A, i.e. d_min above 0.85 / 0.87 / 0.91 A
sin_theta_lambda = [0.590, 0.575, 0.550]
# PLAT029: completeness in %, below (checkCIF expects 99 % or more)
completeness = [99.0, 97.0, 95.0]
# RINTA01: Rint in %, above; compared with the overall Rmerge (Rmeas if no Rmerge)
rint = [10.0, 15.0, 20.0]

# Servers of your own, offered in the login dialog after the built-in ALBA (←→ in its
# first row picks one). Put them at the END of the file: any [table] after a
# [[servers]] entry belongs to that entry.
#   type: a facility amanece knows ("alba", "bruker"), or leave it out (or "nas") to
#         browse plain folders: any SFTP server, cells read from SHELX and XDS files
#   root: where browsing starts (absolute SFTP path); default the login folder
#         (for "bruker": /xray/data/user)
#   port: 22 unless set
#
# [[servers]]
# name = "xray-nas"
# host = "nas.example.org"
# port = 22222
# root = "/data/XRay"
"#;

impl Config {
    /// Servers amanece knows without any config, each with its layout in code; a new
    /// facility adds its entry here. A top-level `remote_root` sets ALBA's proposal folder.
    pub fn builtin_servers(&self) -> Vec<ServerProfile> {
        vec![ServerProfile {
            name: "ALBA".into(),
            host: self.host.clone(),
            port: self.port,
            kind: ServerKind::Alba,
            root: self.remote_root.clone(),
        }]
    }

    /// The servers the login dialog offers: the built-in ones, then `[[servers]]`.
    pub fn server_list(&self) -> Vec<ServerProfile> {
        let mut list = self.builtin_servers();
        list.extend(self.servers.iter().cloned());
        list
    }

    /// Makes `p` the active server: the fields the rest of the app reads.
    pub fn apply(&mut self, p: &ServerProfile) {
        self.host = p.host.clone();
        self.port = p.port;
        self.kind = p.kind;
        self.remote_root = p.root.clone();
    }

    /// Each server needs a host and a name no other one has; a folders server's
    /// `root`, if set, must be an absolute SFTP path.
    pub fn check_servers(list: &[ServerProfile]) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for s in list {
            anyhow::ensure!(!s.name.trim().is_empty(), "a [[servers]] entry has no name");
            anyhow::ensure!(
                seen.insert(s.name.as_str()),
                "two servers are named \"{}\" (ALBA is built in)",
                s.name
            );
            anyhow::ensure!(!s.host.trim().is_empty(), "server \"{}\" has no host", s.name);
            match (s.kind, &s.root) {
                (ServerKind::Folders | ServerKind::Bruker, Some(root)) => {
                    crate::remote::nas::validate_root(root).with_context(|| format!("server \"{}\"", s.name))?
                }
                (ServerKind::Folders | ServerKind::Bruker, None) | (ServerKind::Alba, _) => {}
            }
        }
        Ok(())
    }

    /// Caches of folder servers are kept apart per host, port and root.
    pub fn cache_host(&self) -> String {
        match self.kind {
            ServerKind::Alba => self.host.clone(),
            ServerKind::Bruker => format!(
                "bruker:v1:{}:{}:{}",
                self.host,
                self.port,
                self.remote_root.as_deref().unwrap_or(BRUKER_DEFAULT_ROOT).trim_end_matches('/')
            ),
            ServerKind::Folders => format!(
                "nas:browse-v2:{}:{}:{}",
                self.host,
                self.port,
                self.remote_root.as_deref().unwrap_or("").trim_end_matches('/')
            ),
        }
    }

    pub fn dir() -> Option<PathBuf> {
        let dir = directories::ProjectDirs::from("", "", "amanece")?.config_dir().to_path_buf();
        Some(dir)
    }

    /// Console log, mirrored line by line.
    pub fn log_path(dir: &Path) -> PathBuf {
        dir.join("amanece.log")
    }

    pub fn known_hosts_path() -> Option<PathBuf> {
        Self::dir().map(|d| d.join("known_hosts"))
    }

    /// Loads the config, writing the commented template on first run.
    pub fn load() -> Result<(Config, Option<PathBuf>)> {
        let Some(dir) = Self::dir() else {
            return Ok((Config::default(), None));
        };
        let path = dir.join("config.toml");
        if !path.exists() {
            std::fs::create_dir_all(&dir).ok();
            std::fs::write(&path, TEMPLATE).ok();
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let cfg: Config =
            toml::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))?;
        Ok((cfg, Some(path)))
    }

    pub fn poll_interval(&self, mode: PollMode) -> Option<Duration> {
        let minutes = match mode {
            PollMode::Manual => return None,
            PollMode::Slow => self.slow_poll_minutes,
            PollMode::Fast => self.fast_poll_minutes,
            PollMode::Every30s => 0.5,
            PollMode::Every5s => 5.0 / 60.0,
        };
        let d = Duration::from_secs_f64((minutes * 60.0).max(0.0));
        Some(d.max(MIN_POLL))
    }

    /// Requests per second for the limiter: infinite for 0 (no limit), else at least
    /// `MIN_REQUESTS_PER_SECOND`.
    pub fn request_rate(&self) -> f64 {
        let r = self.max_requests_per_second;
        if r == 0.0 {
            f64::INFINITY
        } else if r.is_nan() || r < 0.0 {
            Config::default().max_requests_per_second
        } else {
            r.max(MIN_REQUESTS_PER_SECOND)
        }
    }

    pub fn background_gap(&self) -> Duration {
        let s = self.background_gap_seconds;
        Duration::from_secs_f64(if s.is_finite() && s > 0.0 { s.min(3600.0) } else { 0.0 })
    }

    pub fn cell_tolerance(&self) -> crate::cells::Tolerance {
        crate::cells::Tolerance {
            length_pct: self.cell_match_pct.clamp(0.1, 10.0),
            angle_deg: self.cell_match_deg.clamp(0.1, 10.0),
        }
    }

    pub fn is_external(&self, program: &str) -> bool {
        let p = program.to_ascii_lowercase();
        let p = p.strip_suffix(".exe").unwrap_or(&p);
        self.external_window
            .iter()
            .any(|e| e.to_ascii_lowercase().trim_end_matches(".exe") == p)
    }
}

const LOGIN_FILE: &str = "login.toml";

/// At most this many usernames are remembered; the least recently used one drops off.
pub const MAX_REMEMBERED: usize = 10;

#[derive(Serialize, Deserialize, Default, PartialEq, Debug)]
struct Remembered {
    /// The names of each server, by host (as its saved passwords), most recently used first.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    servers: BTreeMap<String, Names>,
}

#[derive(Serialize, Deserialize, Default, PartialEq, Debug, Clone)]
struct Names {
    users: Vec<String>,
}

/// Trimmed, non-empty, each once, at most `MAX_REMEMBERED`.
fn tidy_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in names {
        let u = u.trim().to_string();
        if !u.is_empty() && !out.contains(&u) {
            out.push(u);
        }
    }
    out.truncate(MAX_REMEMBERED);
    out
}

fn read_remembered(dir: &Path) -> Remembered {
    let Ok(text) = std::fs::read_to_string(dir.join(LOGIN_FILE)) else {
        return Remembered::default();
    };
    let mut r: Remembered = toml::from_str(&text).unwrap_or_default();
    for n in r.servers.values_mut() {
        n.users = tidy_names(n.users.drain(..));
    }
    r.servers.retain(|_, n| !n.users.is_empty());
    r
}

/// Writes `r`; nothing left removes the file. Only usernames, never a password.
fn write_remembered(dir: &Path, r: &Remembered) -> std::io::Result<()> {
    let path = dir.join(LOGIN_FILE);
    if r.servers.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    std::fs::create_dir_all(dir)?;
    let body = toml::to_string(r).unwrap_or_default();
    std::fs::write(
        path,
        format!(
            "# Written by amanece: usernames that had \"remember username\" ticked at login,\n\
             # per server host, most recently used first. Never a password. Untick the box\n\
             # at login to drop one.\n{body}"
        ),
    )
}

/// The usernames saved for the server at `host` by the login dialog's "remember username"
/// box, most recently used first.
pub fn remembered_users(dir: &Path, host: &str) -> Vec<String> {
    read_remembered(dir).servers.remove(host).map(|n| n.users).unwrap_or_default()
}

/// Puts `user` first in the server's list (`keep`) or takes it out, and returns the
/// server's new list.
pub fn remember_user(dir: &Path, host: &str, user: &str, keep: bool) -> std::io::Result<Vec<String>> {
    let user = user.trim();
    let mut r = read_remembered(dir);
    let before = read_remembered(dir);
    let names = r.servers.entry(host.to_string()).or_default();
    names.users.retain(|u| u != user);
    if keep && !user.is_empty() {
        names.users.insert(0, user.to_string());
        names.users.truncate(MAX_REMEMBERED);
    }
    r.servers.retain(|_, n| !n.users.is_empty());
    if r != before {
        write_remembered(dir, &r)?;
    }
    Ok(remembered_users(dir, host))
}

const FOLDER_FILE: &str = "local_folder.txt";

/// The Local panel's folder when amanece last closed, if it still exists.
pub fn last_local_folder(dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dir.join(FOLDER_FILE)).ok()?;
    let folder = PathBuf::from(text.trim_end_matches(['\r', '\n']));
    folder.is_dir().then_some(folder)
}

/// Remembers the Local panel's folder for the next start. Only the path is written.
pub fn save_local_folder(dir: &Path, folder: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(FOLDER_FILE), folder.to_string_lossy().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_local_folder() {
        let dir = std::env::temp_dir().join(format!("amanece_folder_{}", std::process::id()));
        let folder = dir.join("proposal");
        std::fs::create_dir_all(&folder).unwrap();
        assert!(last_local_folder(&dir).is_none());
        save_local_folder(&dir, &folder).unwrap();
        assert_eq!(last_local_folder(&dir), Some(folder.clone()));
        // a folder that no longer exists is not restored
        std::fs::remove_dir_all(&folder).unwrap();
        assert!(last_local_folder(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remembers_and_forgets_usernames() {
        let dir = std::env::temp_dir().join(format!("amanece_login_{}", std::process::id()));
        const H: &str = "alba.example";
        assert!(remembered_users(&dir, H).is_empty());
        remember_user(&dir, H, "u2025\"x", true).unwrap();
        remember_user(&dir, H, "ub", true).unwrap();
        assert_eq!(remembered_users(&dir, H), ["ub", "u2025\"x"]);
        // used again: back to the front, not twice
        assert_eq!(remember_user(&dir, H, "u2025\"x", true).unwrap(), ["u2025\"x", "ub"]);
        assert_eq!(remember_user(&dir, H, "ub", false).unwrap(), ["u2025\"x"]);
        assert_eq!(remember_user(&dir, H, "never-saved", false).unwrap(), ["u2025\"x"]);
        remember_user(&dir, H, "u2025\"x", false).unwrap();
        assert!(!dir.join(LOGIN_FILE).exists());
        remember_user(&dir, H, "u2025\"x", false).unwrap();
        // the oldest name drops off past the limit
        for i in 0..MAX_REMEMBERED + 2 {
            remember_user(&dir, H, &format!("u{i}"), true).unwrap();
        }
        let users = remembered_users(&dir, H);
        assert_eq!(users.len(), MAX_REMEMBERED);
        assert_eq!(users[0], format!("u{}", MAX_REMEMBERED + 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usernames_are_kept_per_server() {
        let dir = std::env::temp_dir().join(format!("amanece_login_servers_{}", std::process::id()));
        let (alba, nas) = ("userdata.example", "10.0.0.7");
        remember_user(&dir, alba, "u2024", true).unwrap();
        remember_user(&dir, nas, "amanece", true).unwrap();
        assert_eq!(remembered_users(&dir, alba), ["u2024"]);
        assert_eq!(remembered_users(&dir, nas), ["amanece"]);
        assert!(remembered_users(&dir, "elsewhere").is_empty());
        // forgetting on one server leaves the other alone
        remember_user(&dir, nas, "u2024", false).unwrap();
        assert_eq!(remembered_users(&dir, alba), ["u2024"]);

        // a file of a version before 0.5 (one shared list) reads as empty
        std::fs::write(dir.join(LOGIN_FILE), "users = [\"u2025\"]\n").unwrap();
        assert!(remembered_users(&dir, alba).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn template_parses_to_defaults() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(cfg.port, 443);
        assert_eq!(cfg.poll_mode, PollMode::Manual);
        assert_eq!(cfg.external_window, vec!["xprep".to_string()]);
        assert_eq!(cfg.checkcif, crate::checkcif::Limits::default());
        assert_eq!(cfg.cell_match_pct, Config::default().cell_match_pct);
        assert_eq!(cfg.cell_match_deg, Config::default().cell_match_deg);
        assert!(cfg.check_updates);
        // older config files without the key keep checking
        assert!(toml::from_str::<Config>("port = 443").unwrap().check_updates);
    }

    #[test]
    fn poll_floor_and_request_rate() {
        let cfg = Config {
            fast_poll_minutes: 0.01,
            max_requests_per_second: 50.0,
            ..Config::default()
        };
        assert_eq!(cfg.poll_interval(PollMode::Fast), Some(MIN_POLL));
        assert_eq!(cfg.poll_interval(PollMode::Every30s), Some(Duration::from_secs(30)));
        assert_eq!(cfg.poll_interval(PollMode::Every5s), Some(Duration::from_secs(5)));
        assert_eq!(interval_label(Duration::from_secs(600)), "10 min");
        assert_eq!(interval_label(Duration::from_secs(5)), "5 s");
        let mut m = PollMode::Manual;
        let mut seen = vec![];
        for _ in 0..5 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(seen.last(), Some(&PollMode::Manual));
        assert!(seen.contains(&PollMode::Every5s));
        let parsed: Config = toml::from_str("poll_mode = \"5s\"").unwrap();
        assert_eq!(parsed.poll_mode, PollMode::Every5s);
        assert_eq!(cfg.poll_interval(PollMode::Manual), None);
        // no upper cap: a power user may go as fast as the server lets them; 0 = no limit
        assert_eq!(cfg.request_rate(), 50.0);
        assert!(Config { max_requests_per_second: 0.0, ..Config::default() }.request_rate().is_infinite());
        assert_eq!(Config { max_requests_per_second: 0.01, ..Config::default() }.request_rate(), MIN_REQUESTS_PER_SECOND);
        assert_eq!(Config { max_requests_per_second: -1.0, ..Config::default() }.request_rate(), 3.0);
        assert_eq!(Config::default().background_gap(), Duration::from_secs(3));
        assert_eq!(Config { background_gap_seconds: 0.0, ..Config::default() }.background_gap(), Duration::ZERO);
        assert_eq!(Config { background_gap_seconds: -2.0, ..Config::default() }.background_gap(), Duration::ZERO);
    }

    #[test]
    fn external_matching() {
        let cfg = Config::default();
        assert!(cfg.is_external("XPREP.exe"));
        assert!(!cfg.is_external("shelxt"));
    }

    #[test]
    fn alba_is_built_in_and_servers_come_after_it() {
        // nothing configured: ALBA alone, at its default address
        let list = Config::default().server_list();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].name.as_str(), list[0].host.as_str(), list[0].port), ("ALBA", "userdata.cells.es", 443));
        assert_eq!(list[0].kind, ServerKind::Alba);

        let text = r#"
            [[servers]]
            name = "xray-nas"
            host = "nas.example.org"
            port = 22222
            root = "/data/XRay"
            [[servers]]
            name = "old-style"
            host = "h"
            type = "nas"
            remote_root = "/x"
            [[servers]]
            name = "alba-2"
            host = "other.example.org"
            type = "alba"
        "#;
        let cfg: Config = toml::from_str(text).unwrap();
        let list = cfg.server_list();
        assert_eq!(
            list.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["ALBA", "xray-nas", "old-style", "alba-2"]
        );
        // no type: plain folders; port 22 unless set
        assert_eq!((list[1].kind, list[1].port, list[1].root.as_deref()), (ServerKind::Folders, 22222, Some("/data/XRay")));
        assert_eq!((list[2].kind, list[2].port, list[2].root.as_deref()), (ServerKind::Folders, 22, Some("/x")));
        assert_eq!(list[3].kind, ServerKind::Alba);
        assert!(Config::check_servers(&list).is_ok());

        // unknown keys in a server are an error, not silently ignored
        assert!(toml::from_str::<Config>("[[servers]]\nname = \"a\"\nhost = \"h\"\ncolour = true\n").is_err());
    }

    #[test]
    fn apply_makes_a_server_the_active_one() {
        let mut cfg = Config::default();
        let alba = cfg.cache_host();
        cfg.apply(&ServerProfile {
            name: "xray-nas".into(),
            host: "nas.example.org".into(),
            port: 22222,
            kind: ServerKind::Folders,
            root: Some("/data/XRay".into()),
        });
        assert_eq!((cfg.host.as_str(), cfg.port, cfg.kind), ("nas.example.org", 22222, ServerKind::Folders));
        assert_eq!(cfg.remote_root.as_deref(), Some("/data/XRay"));
        assert!(cfg.cache_host().starts_with("nas:") && cfg.cache_host() != alba);
    }

    #[test]
    fn server_names_must_be_unique_and_present() {
        let p = |name: &str, host: &str| ServerProfile {
            name: name.into(),
            host: host.into(),
            port: 22,
            kind: ServerKind::Folders,
            root: None,
        };
        assert!(Config::check_servers(&[p("a", "h"), p("a", "h2")]).is_err());
        assert!(Config::check_servers(&Config::default().server_list().into_iter().chain([p("ALBA", "h")]).collect::<Vec<_>>()).is_err());
        assert!(Config::check_servers(&[p("", "h")]).is_err());
        assert!(Config::check_servers(&[p("a", " ")]).is_err());
        // a folders server's root is optional, but absolute when set
        let rooted = |root: &str| ServerProfile { root: Some(root.into()), ..p("x", "h") };
        assert!(Config::check_servers(&[p("x", "h")]).is_ok());
        assert!(Config::check_servers(&[rooted("data")]).is_err());
        assert!(Config::check_servers(&[rooted("/data/XRay")]).is_ok());
    }

    #[test]
    fn bruker_servers_parse_validate_and_keep_their_own_cache() {
        let cfg: Config = toml::from_str(
            "[[servers]]
name = \"bruker\"
host = \"b.example.org\"
type = \"bruker\"
             [[servers]]
name = \"b2\"
host = \"b.example.org\"
type = \"bruker\"
root = \"/xray/data/user/guest\"
",
        )
        .unwrap();
        let list = cfg.server_list();
        assert_eq!((list[1].kind, list[1].port, list[1].root.as_deref()), (ServerKind::Bruker, 22, None));
        assert!(Config::check_servers(&list).is_ok());
        let mut bad = list.clone();
        bad[2].root = Some("xray".into());
        assert!(Config::check_servers(&bad).is_err(), "a Bruker root must be absolute like a folders root");

        // the default root and an explicit one are different caches; neither is ALBA's or a NAS's
        let (mut a, mut b) = (Config::default(), Config::default());
        a.apply(&list[1]);
        b.apply(&list[2]);
        assert_eq!(a.cache_host(), "bruker:v1:b.example.org:22:/xray/data/user");
        assert_ne!(a.cache_host(), b.cache_host());
        let mut nas = a.clone();
        nas.kind = ServerKind::Folders;
        assert_ne!(a.cache_host(), nas.cache_host());
        assert_ne!(a.cache_host(), Config::default().cache_host());
    }

    #[test]
    fn server_kinds_say_what_they_browse() {
        use ServerKind::{Alba, Bruker, Folders};
        // (source ids, trailing-slash folders, test names hidden)
        let caps = |k: ServerKind| (k.source_ids(), k.browses_folders(), k.hides_test_names());
        assert_eq!(caps(Alba), (false, false, true));
        assert_eq!(caps(Folders), (true, true, false));
        assert_eq!(caps(Bruker), (true, false, true));
    }
}
