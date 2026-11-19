//! Data shared between the SFTP worker, the cache and the UI.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One autoprocessing result ("solve") for one run of a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SolveKind {
    Archive,
    Xia2Chem,
    EdnaProc,
    AutoProcNoAnom,
    AutoProcAnom,
    /// fast_dp (XAIRA)
    FastDp,
    Xia2,
    /// Bruker (docs/BRUKER.md §4.2b): a dataset is one `.hkl` of `work/` with the cell of its
    /// own files, XPREP's output (it has an `.ins`/`.res`) or SADABS's (`<E>_0m.hkl`). The
    /// sample's one cell, from the highest-authority file, is a row of its own. Never ranked.
    BrukerXprep,
    BrukerSadabs,
    /// SAINT's reflections that were never scaled: the merged `.raw`/`.mul`, to run SADABS/TWINABS on
    BrukerSaint,
    BrukerCell,
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
            SolveKind::Archive => "archive",
            SolveKind::BrukerXprep => "XPREP",
            SolveKind::BrukerSadabs => "SADABS",
            SolveKind::BrukerSaint => "SAINT",
            SolveKind::BrukerCell => "cell",
        }
    }

    /// Bruker rows: a cell and files, no statistics, no quality, no ★.
    #[allow(dead_code)] // used by the UI and tests
    pub fn is_bruker(self) -> bool {
        matches!(self, SolveKind::BrukerXprep | SolveKind::BrukerSadabs | SolveKind::BrukerSaint | SolveKind::BrukerCell)
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
            SolveKind::Archive => "archive",
            // one folder per sample, like the server's, so Olex2 finds every file next to the .ins
            SolveKind::BrukerXprep | SolveKind::BrukerSadabs | SolveKind::BrukerSaint | SolveKind::BrukerCell => "work",
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
    /// Bruker: XPREP's statistics per shell (`.prp`) or SAINT's coverage (`_0m._ls`), as lines
    /// for the Info panel's Shells subtab.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shell_table: Vec<String>,
    /// Bruker: SADABS's error model (`.abs`) or SAINT's integration figures, for the Scaling
    /// subtab.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scaling: Vec<String>,
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
    /// Bruker cell files: the file's time (unix seconds), so an unchanged file is not read again.
    #[serde(default)]
    pub mtime: Option<u32>,
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
            mtime: None,
        }
    }

    /// Unique key of a solve (an autoPROC dir holds both anom and noanom).
    pub fn key(&self) -> String {
        format!("{}#{}", self.dir, self.kind.folder())
    }
}

/// What a run is. ALBA has only data runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    #[default]
    Data,
    /// Bruker unit-cell run (`matrix_<run>_<frame>.sfrm`): frames to download, nothing to process.
    Matrix,
    /// Bruker `work/` folder: not frames, it holds the cell-file rows.
    Work,
}

/// The frames of a Bruker run, from the experiment folder's listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameSet {
    pub first: u32,
    pub last: u32,
    pub count: u32,
    pub bytes: u64,
}

impl FrameSet {
    /// Fewer files than the frame numbers span: cut short, still collecting, or lost.
    #[allow(dead_code)] // the run rows use it from milestone 4
    pub fn has_gaps(&self) -> bool {
        u64::from(self.last - self.first) + 1 != u64::from(self.count)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// MXCuBE file prefix (e.g. `manually-mounted`, or the sample name in old layouts).
    pub prefix: String,
    pub run: u32,
    /// Test shots only (EDNA characterisation / XAIRA testxtal), no data collection.
    /// A run with neither this flag nor solves is a dataset nobody processed.
    pub characterisation: bool,
    pub solves: Vec<Solve>,
    #[serde(default)]
    pub kind: RunKind,
    /// Bruker: what the folder listing shows of this run's frames.
    #[serde(default)]
    pub frames: Option<FrameSet>,
    /// Something to tell about the run as a whole (Bruker: cell files that were not read).
    #[serde(default)]
    pub note: Option<String>,
    /// Bruker `work/`: what its files looked like when they were read (`bruker::work_stamp`).
    #[serde(default)]
    pub stamp: Option<String>,
}

impl Run {
    /// A run of frames that is collected data (not matrix, not the `work/` row).
    pub fn is_data(&self) -> bool {
        self.kind == RunKind::Data
    }

    pub fn key(&self) -> String {
        format!("{}_{}", self.prefix, self.run)
    }

    /// The run's image range: the widest the solves report. A pipeline may integrate
    /// fewer images than were collected (autoPROC cut XAIRA's 1–720 to 1–635), so the
    /// first solve's range isn't the run's; the raw data covers at least the union.
    pub fn images(&self) -> Option<(u32, u32)> {
        self.solves
            .iter()
            .filter_map(|s| s.stats.as_ref().and_then(|st| st.images))
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
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

    /// Only test shots: no data collection at all.
    pub fn test_shots_only(&self) -> bool {
        !self.runs.is_empty() && self.runs.iter().all(|r| r.characterisation)
    }

    /// Has a collected dataset that no pipeline processed.
    pub fn unprocessed(&self) -> bool {
        !self.has_data() && self.runs.iter().any(|r| r.is_data() && !r.characterisation && r.solves.is_empty())
    }

    pub fn has_running(&self) -> bool {
        self.solves().any(|s| s.status == Status::Running)
    }

    /// Distinct prefixes among runs with solves (characterisation prefixes don't count).
    pub fn distinct_prefixes(&self) -> usize {
        let mut p: Vec<&str> = self
            .runs
            .iter()
            .filter(|r| !r.characterisation && r.is_data())
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
    #[serde(default)]
    pub listing_error: Option<String>,
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
    fn run_images_are_the_widest_any_solve_reports() {
        let solve = |images| Solve {
            stats: Some(Stats { images, ..Stats::default() }),
            ..Solve::new(SolveKind::AutoProcNoAnom, "d", 1)
        };
        let run = |solves| Run { prefix: "p".into(), run: 1, characterisation: false, solves, kind: RunKind::Data, frames: None, note: None, stamp: None };
        assert_eq!(run(vec![]).images(), None);
        assert_eq!(run(vec![solve(Some((1, 635))), solve(None), solve(Some((1, 720)))]).images(), Some((1, 720)));
        assert_eq!(run(vec![solve(Some((5, 700))), solve(Some((1, 650)))]).images(), Some((1, 700)));
    }

    #[test]
    fn caches_from_before_bruker_still_load() {
        // a sample as the 0.4 cache stored it: no run kind, frames, note or file time
        let old = r#"{"name":"S","scanned_at":1,"runs":[{"prefix":"p","run":1,"characterisation":false,
            "solves":[{"kind":"Xia2Chem","dir":"/d","n":1,"status":"Done","stats":null,"hkl":[],"logs":[],"note":null}]}]}"#;
        let info: SampleInfo = serde_json::from_str(old).unwrap();
        let run = &info.runs[0];
        assert_eq!((run.kind, run.frames, run.note.clone()), (RunKind::Data, None, None));
        assert_eq!(run.solves[0].mtime, None);
        assert!(run.is_data() && info.has_data());

        // the Bruker additions survive a save and a load
        let mut s = Solve::new(SolveKind::BrukerCell, "/g/S/work/a.p4p_cn", 1);
        s.mtime = Some(5);
        let work = Run {
            prefix: "work".into(),
            run: 0,
            characterisation: false,
            solves: vec![s],
            kind: RunKind::Work,
            frames: Some(FrameSet { first: 1, last: 12, count: 12, bytes: 3 }),
            note: Some("n".into()),
            stamp: Some("3:5:7".into()),
        };
        let back: Run = serde_json::from_str(&serde_json::to_string(&work).unwrap()).unwrap();
        assert_eq!(back, work);
        assert!(SolveKind::BrukerCell.is_bruker() && !SolveKind::Xia2Chem.is_bruker());
        assert!(!FrameSet { first: 1, last: 12, count: 12, bytes: 0 }.has_gaps());
        assert!(FrameSet { first: 1, last: 12, count: 11, bytes: 0 }.has_gaps());
    }

    #[test]
    fn remote_file_name() {
        let f = RemoteFile::new("/a/b/shelxt.hkl", Some(1));
        assert_eq!(f.name(), "shelxt.hkl");
    }
}
