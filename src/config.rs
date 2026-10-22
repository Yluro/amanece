//! `config.toml` in the user config dir. Never holds credentials.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Polls never run more often than this, whatever the config says.
pub const MIN_POLL: Duration = Duration::from_secs(60);
/// Upper bound for the SFTP request rate, whatever the config says.
pub const MAX_REQUESTS_PER_SECOND: f64 = 5.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PollMode {
    Manual,
    Slow,
    Fast,
}

impl PollMode {
    pub fn next(self) -> PollMode {
        match self {
            PollMode::Manual => PollMode::Slow,
            PollMode::Slow => PollMode::Fast,
            PollMode::Fast => PollMode::Manual,
        }
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
    /// Programs that open in their own console window instead of the console panel.
    pub external_window: Vec<String>,
    pub max_requests_per_second: f64,
    pub burst: u32,
    /// Beamline folder used to find the proposal folder (`/bl13/projects/cycle*/...`).
    pub beamline: String,
    /// Proposal folder on the server, if automatic detection doesn't find it.
    pub remote_root: Option<String>,
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
            beamline: "bl13".into(),
            remote_root: None,
        }
    }
}

const TEMPLATE: &str = r#"# alba_watch configuration. Credentials are never stored here.
host = "userdata.cells.es"
port = 443

# "manual", "slow" or "fast" at startup (cycle with `p` in the app)
poll_mode = "manual"
slow_poll_minutes = 10.0
fast_poll_minutes = 2.0   # never below 1 minute

# beep when a new named sample gets processing results
sound = true
# also download logs (summary.html, xia2.txt, CORRECT.LP, ...) with the HKL files
include_logs = false
# show samples named "test..." and characterisation-only samples
show_test = false

# programs that open in their own console window (interactive)
external_window = ["xprep"]

# SFTP politeness: sustained requests per second (max 5) and burst size
max_requests_per_second = 3.0
burst = 6

# Where the beamtime folders (YYYYMMDD) live. Normally found automatically:
# either the login folder itself, or /<beamline>/projects/cycle*/<account digits>-<pi>
beamline = "bl13"
# remote_root = "/bl13/projects/cycle2026-I/<account digits>-<pi>"
"#;

impl Config {
    pub fn dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "alba_watch").map(|d| d.config_dir().to_path_buf())
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
        };
        let d = Duration::from_secs_f64((minutes * 60.0).max(0.0));
        Some(d.max(MIN_POLL))
    }

    pub fn request_rate(&self) -> f64 {
        self.max_requests_per_second.clamp(0.2, MAX_REQUESTS_PER_SECOND)
    }

    pub fn is_external(&self, program: &str) -> bool {
        let p = program.to_ascii_lowercase();
        let p = p.strip_suffix(".exe").unwrap_or(&p);
        self.external_window
            .iter()
            .any(|e| e.to_ascii_lowercase().trim_end_matches(".exe") == p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        let cfg: Config = toml::from_str(TEMPLATE).unwrap();
        assert_eq!(cfg.port, 443);
        assert_eq!(cfg.poll_mode, PollMode::Manual);
        assert_eq!(cfg.external_window, vec!["xprep".to_string()]);
    }

    #[test]
    fn poll_floor_and_rate_cap() {
        let cfg = Config {
            fast_poll_minutes: 0.1,
            max_requests_per_second: 50.0,
            ..Config::default()
        };
        assert_eq!(cfg.poll_interval(PollMode::Fast), Some(MIN_POLL));
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
