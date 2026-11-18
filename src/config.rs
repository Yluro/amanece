//! `config.toml` in the user config dir. Never holds credentials. The same folder
//! holds amanece's other files: `known_hosts`, the caches, `amanece.log` and, only
//! when "remember username" is ticked at login, `login.toml` with the usernames.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Polls never run more often than this, whatever the config says.
pub const MIN_POLL: Duration = Duration::from_secs(5);
/// Below this interval a poll only re-lists the open/newest beamtime; the beamtime
/// list and sample rescans happen at most this often (see `App::poll_now`).
pub const HEAVY_POLL_GAP: Duration = Duration::from_secs(60);
/// Upper bound for the SFTP request rate, whatever the config says.
pub const MAX_REQUESTS_PER_SECOND: f64 = 5.0;

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

/// The layout of a server's folders: a facility amanece knows (`type = "alba"`), or
/// plain folders (no `type`, or `type = "nas"`). A new facility is a new variant plus
/// its scan code; every `match` on the type then asks for it.
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
}

impl ServerKind {
    pub fn is_folders(self) -> bool {
        self == ServerKind::Folders
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
    /// ALBA's address (old configs may still set it; new ones leave it out). After
    /// startup: the active server's.
    pub host: String,
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
    pub max_requests_per_second: f64,
    pub burst: u32,
    /// Frames downloaded at a time over the one connection (1–4). HKL/log downloads
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
    /// How the first NAS patch described a NAS server; refused, see `Config::load`.
    #[serde(skip_serializing)]
    pub nas: bool,
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
            parallel_downloads: 3,
            beamlines: vec!["bl13".into(), "bl06".into()],
            remote_root: None,
            kind: ServerKind::Alba,
            nas: false,
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

# SFTP politeness: sustained requests per second (max 5) and burst size
max_requests_per_second = 3.0
burst = 6
# frames downloaded at a time over the one connection (1-4); HKL/log downloads
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
#   type: a facility amanece knows ("alba"), or leave it out (or "nas") to browse
#         plain folders: any SFTP server, cells read from SHELX and XDS files
#   root: where browsing starts (absolute SFTP path); default the login folder
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
    /// facility adds its entry here. Old configs' top-level `host`/`port`/`remote_root`
    /// still point ALBA elsewhere.
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
                (ServerKind::Folders, Some(root)) => crate::remote::nas::validate_root(root)
                    .with_context(|| format!("server \"{}\"", s.name))?,
                (ServerKind::Folders, None) | (ServerKind::Alba, _) => {}
            }
        }
        Ok(())
    }

    /// Caches of folder servers are kept apart per host, port and root.
    pub fn cache_host(&self) -> String {
        match self.kind {
            ServerKind::Alba => self.host.clone(),
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
        // The app used to be called alba_watch: carry its settings and pinned host key over.
        if !dir.exists() {
            if let Some(old) = directories::ProjectDirs::from("", "", "alba_watch")
                .map(|d| d.config_dir().to_path_buf())
                .filter(|o| o.exists())
            {
                if let Some(parent) = dir.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::rename(old, &dir);
            }
        }
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
        anyhow::ensure!(
            !cfg.nas,
            "{}: `nas = true` is gone; describe the NAS as a [[servers]] entry instead (see the end of the file)",
            path.display()
        );
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

    pub fn request_rate(&self) -> f64 {
        self.max_requests_per_second.clamp(0.2, MAX_REQUESTS_PER_SECOND)
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

#[derive(Serialize, Deserialize, Default)]
struct Remembered {
    /// Most recently used first.
    #[serde(default)]
    users: Vec<String>,
    /// 0.2.0 kept a single name here; still read, never written.
    #[serde(default, skip_serializing)]
    user: Option<String>,
}

/// The usernames saved by the login dialog's "remember username" box, most recently
/// used first.
pub fn remembered_users(dir: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(dir.join(LOGIN_FILE)) else {
        return Vec::new();
    };
    let r: Remembered = toml::from_str(&text).unwrap_or_default();
    let mut users: Vec<String> = Vec::new();
    for u in r.users.into_iter().chain(r.user) {
        let u = u.trim().to_string();
        if !u.is_empty() && !users.contains(&u) {
            users.push(u);
        }
    }
    users.truncate(MAX_REMEMBERED);
    users
}

/// Puts `user` first in the saved list (`keep`) or takes it out, and returns the new
/// list. Only usernames are written, never a password; an empty list removes the file.
pub fn remember_user(dir: &Path, user: &str, keep: bool) -> std::io::Result<Vec<String>> {
    let user = user.trim();
    let before = remembered_users(dir);
    let mut users = before.clone();
    users.retain(|u| u != user);
    if keep && !user.is_empty() {
        users.insert(0, user.to_string());
        users.truncate(MAX_REMEMBERED);
    }
    if users == before {
        return Ok(users);
    }
    let path = dir.join(LOGIN_FILE);
    if users.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(users),
        };
    }
    std::fs::create_dir_all(dir)?;
    let body = toml::to_string(&Remembered {
        users: users.clone(),
        user: None,
    })
    .unwrap_or_default();
    std::fs::write(
        path,
        format!(
            "# Written by amanece: usernames that had \"remember username\" ticked at login,\n\
             # most recently used first. Never a password. Untick the box at login to drop one.\n{body}"
        ),
    )?;
    Ok(users)
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
        assert!(remembered_users(&dir).is_empty());
        remember_user(&dir, "u2025\"x", true).unwrap();
        remember_user(&dir, "ub", true).unwrap();
        assert_eq!(remembered_users(&dir), ["ub", "u2025\"x"]);
        // used again: back to the front, not twice
        assert_eq!(remember_user(&dir, "u2025\"x", true).unwrap(), ["u2025\"x", "ub"]);
        assert_eq!(remember_user(&dir, "ub", false).unwrap(), ["u2025\"x"]);
        assert_eq!(remember_user(&dir, "never-saved", false).unwrap(), ["u2025\"x"]);
        remember_user(&dir, "u2025\"x", false).unwrap();
        assert!(!dir.join(LOGIN_FILE).exists());
        remember_user(&dir, "u2025\"x", false).unwrap();
        // the oldest name drops off past the limit
        for i in 0..MAX_REMEMBERED + 2 {
            remember_user(&dir, &format!("u{i}"), true).unwrap();
        }
        let users = remembered_users(&dir);
        assert_eq!(users.len(), MAX_REMEMBERED);
        assert_eq!(users[0], format!("u{}", MAX_REMEMBERED + 1));
        // the one-name file written by 0.2.0 still reads
        std::fs::write(dir.join(LOGIN_FILE), "user = \"ua\"\n").unwrap();
        assert_eq!(remembered_users(&dir), ["ua"]);
        assert_eq!(remember_user(&dir, "ub", true).unwrap(), ["ub", "ua"]);
        assert!(!std::fs::read_to_string(dir.join(LOGIN_FILE)).unwrap().contains("user ="));
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
    fn poll_floor_and_rate_cap() {
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
        assert_eq!(cfg.request_rate(), MAX_REQUESTS_PER_SECOND);
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

        // an old top-level host still moves the built-in ALBA
        let cfg: Config = toml::from_str("host = \"h\"\nport = 2222\n").unwrap();
        assert_eq!((cfg.server_list()[0].host.as_str(), cfg.server_list()[0].port), ("h", 2222));
        // unknown keys in a server are an error, not silently ignored
        assert!(toml::from_str::<Config>("[[servers]]\nname = \"a\"\nhost = \"h\"\nnas = true\n").is_err());
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
}
