//! `<base>/.amanece/cache.json`: listings and stats already fetched, so a restart
//! doesn't scrape the server again. Finished solves never change on the server.

use crate::model::Experiment;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Bumped when scan results change meaning, so old caches are rebuilt.
const VERSION: u32 = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub host: String,
    /// Experiment dates seen at the server root, newest first.
    pub dates: Vec<String>,
    pub experiments: BTreeMap<String, Experiment>,
}

pub fn state_dir(base: &Path) -> PathBuf {
    let dir = base.join(".amanece");
    // The app used to be called alba_watch: keep its cache instead of refetching everything.
    let old = base.join(".alba_watch");
    if !dir.exists() && old.exists() {
        let _ = std::fs::rename(old, &dir);
    }
    dir
}

fn path(base: &Path) -> PathBuf {
    state_dir(base).join("cache.json")
}

impl Cache {
    pub fn load(base: &Path, host: &str) -> Cache {
        let fresh = Cache {
            version: VERSION,
            host: host.to_string(),
            ..Cache::default()
        };
        let Ok(text) = std::fs::read_to_string(path(base)) else {
            return fresh;
        };
        match serde_json::from_str::<Cache>(&text) {
            Ok(c) if c.version == VERSION && c.host == host => c,
            _ => fresh,
        }
    }

    pub fn save(&self, base: &Path) -> Result<()> {
        let dir = state_dir(base);
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("cache.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, path(base))?;
        Ok(())
    }

    /// Replace the date list from a fresh root listing; forget experiments that vanished
    /// (e.g. after logging in with another proposal account in the same folder).
    pub fn set_dates(&mut self, mut dates: Vec<String>) {
        let day = |s: &String| crate::layout::beamtime_date(s).to_string();
        dates.sort_by(|a, b| day(b).cmp(&day(a)).then(b.cmp(a)));
        self.experiments.retain(|d, _| dates.contains(d));
        for d in &dates {
            self.experiments.entry(d.clone()).or_insert_with(|| Experiment {
                date: d.clone(),
                ..Experiment::default()
            });
        }
        self.dates = dates;
    }

    pub fn experiment_mut(&mut self, date: &str) -> &mut Experiment {
        self.experiments
            .entry(date.to_string())
            .or_insert_with(|| Experiment {
                date: date.to_string(),
                ..Experiment::default()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_dates_sorts_and_prunes() {
        let mut c = Cache::default();
        c.set_dates(vec!["20250704".into(), "20251211".into()]);
        assert_eq!(c.dates, vec!["20251211", "20250704"]);
        c.set_dates(vec!["20251211".into()]);
        assert!(!c.experiments.contains_key("20250704"));
    }

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("amanece_cache_test_{}", std::process::id()));
        let mut c = Cache::load(&dir, "h");
        c.set_dates(vec!["20251211".into()]);
        c.save(&dir).unwrap();
        let back = Cache::load(&dir, "h");
        assert_eq!(back.dates, vec!["20251211"]);
        assert!(Cache::load(&dir, "other-host").dates.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
