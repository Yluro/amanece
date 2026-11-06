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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
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
    /// Proposal folder on the server, if automatic detection doesn't find it.
    pub remote_root: Option<String>,
    /// Limits for the checkCIF-style alerts in the Stats panel.
    pub checkcif: crate::checkcif::Limits,
    /// Unit-cell database: two reduced cells match within this many % on each edge…
    pub cell_match_pct: f64,
    /// …and this many degrees on each angle.
    pub cell_match_deg: f64,
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
            include_logs: false,
            show_test: false,
            external_window: vec!["xprep".into()],
            max_requests_per_second: 3.0,
            burst: 6,
            parallel_downloads: 3,
            beamlines: vec!["bl13".into(), "bl06".into()],
            remote_root: None,
            checkcif: crate::checkcif::Limits::default(),
            cell_match_pct: 1.5,
            cell_match_deg: 2.0,
        }
    }
}

const TEMPLATE: &str = r#"# amanece configuration. Credentials are never stored here.
host = "userdata.cells.es"
port = 443

# "manual", "slow", "fast", "30s" or "5s" at startup (cycle with `p` in the app).
# At 30 s / 5 s a poll only re-lists the open beamtime; the beamtime list and
# sample rescans still happen at most once a minute.
poll_mode = "manual"
slow_poll_minutes = 10.0
fast_poll_minutes = 2.0

# beep when a new named sample gets processing results
sound = true
# also download logs (summary.html, xia2.txt, CORRECT.LP, ...) with the HKL files
include_logs = false
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

# Where the beamtime folders (YYYYMMDD) live. Normally found automatically:
# either the login folder itself, or /<beamline>/projects/cycle*/<account digits>-<pi>
# on each of these beamlines (bl13 = XALOC, bl06 = XAIRA)
beamlines = ["bl13", "bl06"]
# remote_root = "/bl13/projects/cycle2026-I/<account digits>-<pi>"

# unit-cell database (cells.toml in this folder): the Stats panel lists the samples
# whose reduced cell matches within this many % on each edge and degrees on each angle
cell_match_pct = 1.5
cell_match_deg = 2.0

# checkCIF-style alerts in the Stats panel. Each list gives the limits for alert
# levels C, B, A; a value past one turns its cell yellow (C), orange (B) or red (A).
[checkcif]
# THETM01: sin(theta_max)/lambda in 1/A, i.e. d_min above 0.85 / 0.87 / 0.91 A
sin_theta_lambda = [0.590, 0.575, 0.550]
# PLAT029: completeness in %, below (checkCIF expects 99 % or more)
completeness = [99.0, 97.0, 95.0]
# RINTA01: Rint in %, above; compared with the overall Rmerge (Rmeas if no Rmerge)
rint = [10.0, 15.0, 20.0]
"#;

impl Config {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
