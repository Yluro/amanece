//! Data shared between the SFTP worker, the cache and the UI.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One autoprocessing result ("solve") for one run of a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SolveKind {
    Xia2Chem,
    EdnaProc,
    AutoProcNoAnom,
    AutoProcAnom,
    /// fast_dp (XAIRA)
    FastDp,
    Xia2,
}

impl SolveKind {
    pub fn label(self) -> &'static str {
        match self {
            SolveKind::Xia2Chem => "xia2chem",
            SolveKind::EdnaProc => "ednaproc",
            SolveKind::AutoProcNoAnom => "autoPROC noanom",
            SolveKind::AutoProcAnom => "autoPROC anom",
            SolveKind::FastDp => "fast_dp",
            SolveKind::Xia2 => "xia2",
        }
    }

    /// Local folder stem, e.g. `autoproc_noanom` → `autoproc_noanom_r3/`.
    pub fn folder(self) -> &'static str {
        match self {
            SolveKind::Xia2Chem => "xia2chem",
            SolveKind::EdnaProc => "ednaproc",
            SolveKind::AutoProcNoAnom => "autoproc_noanom",
            SolveKind::AutoProcAnom => "autoproc_anom",
            SolveKind::FastDp => "fastdp",
            SolveKind::Xia2 => "xia2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Running,
    Done,
    Failed,
}

/// Statistics for one resolution shell. Percentages are already normalised.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Shell {
    pub d_low: Option<f64>,
    pub d_high: Option<f64>,
    pub completeness: Option<f64>,
    pub multiplicity: Option<f64>,
    pub i_sig: Option<f64>,
    pub rmerge_pct: Option<f64>,
    pub rmeas_pct: Option<f64>,
    pub rpim_pct: Option<f64>,
    pub cc_half: Option<f64>,
    pub n_obs: Option<u64>,
    pub n_unique: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub space_group: Option<String>,
    pub cell: Option<[f64; 6]>,
    pub overall: Option<Shell>,
    pub inner: Option<Shell>,
    pub outer: Option<Shell>,
    pub images: Option<(u32, u32)>,
    pub anomalous: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFile {
    pub path: String,
    pub size: Option<u64>,
}

impl RemoteFile {
    pub fn new(path: impl Into<String>, size: Option<u64>) -> Self {
        RemoteFile { path: path.into(), size }
    }

    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Solve {
    pub kind: SolveKind,
    /// Remote solve directory, e.g. `…/xia2chem_manually-mounted_3_1`.
    pub dir: String,
    /// Trailing index of the solve dir name (`_3_1` → 1).
    pub n: u32,
    pub status: Status,
    pub stats: Option<Stats>,
    pub hkl: Vec<RemoteFile>,
    pub logs: Vec<RemoteFile>,
    pub note: Option<String>,
}

impl Solve {
    pub fn new(kind: SolveKind, dir: impl Into<String>, n: u32) -> Self {
        Solve {
            kind,
            dir: dir.into(),
            n,
            status: Status::Running,
            stats: None,
            hkl: Vec::new(),
            logs: Vec::new(),
            note: None,
        }
    }

    /// Unique key of a solve (an autoPROC dir holds both anom and noanom).
    pub fn key(&self) -> String {
        format!("{}#{}", self.dir, self.kind.folder())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// MXCuBE file prefix (e.g. `manually-mounted`, or the sample name in old layouts).
    pub prefix: String,
    pub run: u32,
    /// True when the run only has an EDNA characterisation (test shots), no solves.
    pub characterisation: bool,
    pub solves: Vec<Solve>,
}

impl Run {
    pub fn key(&self) -> String {
        format!("{}_{}", self.prefix, self.run)
    }

    /// Image range taken from any solve's stats.
    pub fn images(&self) -> Option<(u32, u32)> {
        self.solves
            .iter()
            .filter_map(|s| s.stats.as_ref().and_then(|st| st.images))
            .next()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleInfo {
    pub name: String,
    pub runs: Vec<Run>,
    /// Unix seconds of the last scan.
    pub scanned_at: u64,
}

impl SampleInfo {
    pub fn solves(&self) -> impl Iterator<Item = &Solve> {
        self.runs.iter().flat_map(|r| r.solves.iter())
    }

    pub fn has_data(&self) -> bool {
        self.solves().next().is_some()
    }

    pub fn has_running(&self) -> bool {
        self.solves().any(|s| s.status == Status::Running)
    }

    /// Distinct prefixes among runs with solves (characterisation prefixes don't count).
    pub fn distinct_prefixes(&self) -> usize {
        let mut p: Vec<&str> = self
            .runs
            .iter()
            .filter(|r| !r.characterisation)
            .map(|r| r.prefix.as_str())
            .collect();
        p.sort();
        p.dedup();
        p.len()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Experiment {
    pub date: String,
    /// Sample folder names under RESULTS; `None` until listed.
    pub samples: Option<Vec<String>>,
    /// No RESULTS folder: samples sit directly in PROCESSED_DATA (2026 layout).
    #[serde(default)]
    pub flat: bool,
    pub infos: BTreeMap<String, SampleInfo>,
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formats() {
        assert_eq!(human_bytes(514), "514 B");
        assert_eq!(human_bytes(5_599_465), "5.6 MB");
        assert_eq!(human_bytes(4_500_000_000), "4.5 GB");
    }

    #[test]
    fn remote_file_name() {
        let f = RemoteFile::new("/a/b/shelxt.hkl", Some(1));
        assert_eq!(f.name(), "shelxt.hkl");
    }
}
