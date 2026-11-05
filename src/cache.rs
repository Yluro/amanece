//! `cache-<account>.json` next to `config.toml`: listings and stats already fetched,
//! so a restart doesn't scrape the server again. Finished solves never change on the
//! server. One file per proposal account, named by a hash of `user@host` so the
//! username itself is not written to disk.

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
    /// `account_key` of the account the listing belongs to; `None` until the first
    /// login (an empty cache, or one carried over from an old `.amanece/` folder).
    #[serde(default)]
    pub account: Option<String>,
    /// Experiment dates seen at the server root, newest first.
    pub dates: Vec<String>,
    pub experiments: BTreeMap<String, Experiment>,
}

/// Stable 64-bit FNV-1a of `user@host`, as hex: names the account's cache file.
pub fn account_key(user: &str, host: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{user}@{host}").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn file(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("cache-{key}.json"))
}

impl Cache {
    fn fresh(host: &str) -> Cache {
        Cache {
            version: VERSION,
            host: host.to_string(),
            ..Cache::default()
        }
    }

    fn read(path: &Path, host: &str) -> Option<Cache> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<Cache>(&text)
            .ok()
            .filter(|c| c.version == VERSION && c.host == host)
    }

    /// The cache of one account, or an empty one owned by it.
    pub fn load_account(dir: Option<&Path>, host: &str, key: &str) -> Cache {
        dir.and_then(|dir| Self::read(&file(dir, key), host))
            .filter(|c| c.account.as_deref() == Some(key))
            .unwrap_or_else(|| Cache {
                account: Some(key.to_string()),
                ..Self::fresh(host)
            })
    }

    /// What to show before logging in: the cache of `user` when it is known, else the
    /// most recently saved one, else one an old version left in `<launch>/.amanece/`
    /// (only read, never written; it is adopted by the first login).
    pub fn load_startup(dir: Option<&Path>, host: &str, user: Option<&str>, launch: &Path) -> Cache {
        let own = dir.and_then(|dir| match user {
            Some(u) => Self::read(&file(dir, &account_key(u, host)), host),
            None => {
                let mut files: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
                    .ok()?
                    .flatten()
                    .filter(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        n.starts_with("cache-") && n.ends_with(".json")
                    })
                    .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
                    .collect();
                files.sort_by_key(|f| std::cmp::Reverse(f.0));
                files.iter().find_map(|(_, p)| Self::read(p, host))
            }
        });
        own.or_else(|| {
            [".amanece", ".alba_watch"]
                .iter()
                .find_map(|old| Self::read(&launch.join(old).join("cache.json"), host))
                .map(|c| Cache { account: None, ..c })
        })
        .unwrap_or_else(|| Self::fresh(host))
    }

    /// Writes the cache into `dir`; a cache no login has claimed yet is not saved.
    pub fn save(&self, dir: &Path) -> Result<()> {
        let Some(key) = &self.account else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)?;
        // per process: two amanece windows may save at the same moment
        let tmp = dir.join(format!("cache-{key}.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, file(dir, key))?;
        Ok(())
    }

    /// Replace the date list from a fresh root listing; forget experiments that vanished
    /// (e.g. a cache carried over from an old version that belonged to another account).
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
    fn key_is_stable_and_hides_the_name() {
        assert_eq!(account_key("u2025", "h"), account_key("u2025", "h"));
        assert_ne!(account_key("u2025", "h"), account_key("u2024", "h"));
        assert_eq!(account_key("a", "b").len(), 16);
        assert!(!account_key("u2025", "h").contains("u2025"));
    }

    #[test]
    fn roundtrip_per_account() {
        let root = std::env::temp_dir().join(format!("amanece_cache_test_{}", std::process::id()));
        let dir = root.join("config");
        let launch = root.join("launch");
        std::fs::create_dir_all(&launch).unwrap();

        // nothing claimed yet: not saved
        let mut c = Cache::load_startup(Some(&dir), "h", None, &launch);
        c.set_dates(vec!["20251211".into()]);
        c.save(&dir).unwrap();
        assert!(!dir.exists());

        let a = account_key("ua", "h");
        let mut c = Cache::load_account(Some(&dir), "h", &a);
        c.set_dates(vec!["20251211".into()]);
        c.save(&dir).unwrap();
        assert_eq!(Cache::load_account(Some(&dir), "h", &a).dates, vec!["20251211"]);
        assert!(Cache::load_account(Some(&dir), "other-host", &a).dates.is_empty());
        assert!(Cache::load_account(Some(&dir), "h", &account_key("ub", "h")).dates.is_empty());
        // before login: the named account's, else the latest one
        assert_eq!(Cache::load_startup(Some(&dir), "h", Some("ua"), &launch).dates, vec!["20251211"]);
        assert_eq!(Cache::load_startup(Some(&dir), "h", None, &launch).account, Some(a));
        // nothing written into the launch folder
        assert_eq!(std::fs::read_dir(&launch).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn old_launch_folder_cache_is_read_not_moved() {
        let root = std::env::temp_dir().join(format!("amanece_cache_old_{}", std::process::id()));
        let launch = root.join("launch");
        let old = launch.join(".amanece").join("cache.json");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        let mut c = Cache::fresh("h");
        c.set_dates(vec!["20250704".into()]);
        std::fs::write(&old, serde_json::to_vec(&c).unwrap()).unwrap();
        let back = Cache::load_startup(Some(&root.join("config")), "h", Some("ua"), &launch);
        assert_eq!(back.dates, vec!["20250704"]);
        assert_eq!(back.account, None);
        assert!(old.exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
