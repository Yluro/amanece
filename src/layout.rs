//! Knowledge about the ALBA directory layout (see DESIGN.md §2). Pure functions only.

use regex::Regex;
use std::sync::LazyLock;

static SOLVE_DIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(autoproc|ednaproc|xia2chem|xia2)_(.+)_(\d+)_(\d+)$").expect("valid regex")
});
static CHAR_DIR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^characterisation_(.+)_run(\d+)_\d+$").expect("valid regex"));
/// XAIRA (EDNA2/ewoks): `<prefix>_<pipeline>_<noanom|anom>_run<N>`.
static PROCESSED_DIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(.+)_(autoproc_staraniso|autoproc|fastdp|xia2dials|xia2chem|ednaproc)_(noanom|anom)_run(\d+)$",
    )
    .expect("valid regex")
});
/// XAIRA characterisation: `ref-<prefix>_testxtal_run<N>`.
static PROCESSED_CHAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(ref-.+)_testxtal_run(\d+)$").expect("valid regex"));
static DATE_DIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{8}$").expect("valid regex"));
static EDNA_CELL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d{1,3})_([\d.]+)_([\d.]+)_([\d.]+)_([\d.]+)_([\d.]+)_([\d.]+)$")
        .expect("valid regex")
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pipeline {
    AutoProc,
    EdnaProc,
    Xia2,
    Xia2Chem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirKind {
    Solve {
        pipeline: Pipeline,
        prefix: String,
        run: u32,
        n: u32,
    },
    /// One solve per folder, with a STATUS file and `all_files/` (XAIRA).
    Processed {
        kind: crate::model::SolveKind,
        prefix: String,
        run: u32,
    },
    Characterisation {
        prefix: String,
        run: u32,
    },
    Other,
}

pub fn classify_dir(name: &str) -> DirKind {
    if let Some(c) = SOLVE_DIR.captures(name) {
        let pipeline = match &c[1] {
            "autoproc" => Pipeline::AutoProc,
            "ednaproc" => Pipeline::EdnaProc,
            "xia2chem" => Pipeline::Xia2Chem,
            _ => Pipeline::Xia2,
        };
        return DirKind::Solve {
            pipeline,
            prefix: c[2].to_string(),
            run: c[3].parse().unwrap_or(0),
            n: c[4].parse().unwrap_or(0),
        };
    }
    if let Some(c) = PROCESSED_DIR.captures(name) {
        use crate::model::SolveKind;
        let anom = &c[3] == "anom";
        let kind = match &c[2] {
            "autoproc" if anom => SolveKind::AutoProcAnom,
            "autoproc" => SolveKind::AutoProcNoAnom,
            "fastdp" => SolveKind::FastDp,
            "xia2dials" => SolveKind::Xia2,
            "xia2chem" => SolveKind::Xia2Chem,
            "ednaproc" => SolveKind::EdnaProc,
            // STARANISO (anisotropic cut-off) is for proteins: not listed
            _ => return DirKind::Other,
        };
        return DirKind::Processed {
            kind,
            prefix: c[1].to_string(),
            run: c[4].parse().unwrap_or(0),
        };
    }
    if let Some(c) = PROCESSED_CHAR.captures(name) {
        return DirKind::Characterisation {
            prefix: c[1].to_string(),
            run: c[2].parse().unwrap_or(0),
        };
    }
    if let Some(c) = CHAR_DIR.captures(name) {
        return DirKind::Characterisation {
            prefix: c[1].to_string(),
            run: c[2].parse().unwrap_or(0),
        };
    }
    DirKind::Other
}

pub fn is_date_dir(name: &str) -> bool {
    DATE_DIR.is_match(name)
}

/// Samples whose name says they are test shots.
pub fn is_test_name(name: &str) -> bool {
    name.to_ascii_lowercase().contains("test")
}

pub fn results_dir(date: &str) -> String {
    format!("/{date}/PROCESSED_DATA/RESULTS")
}

pub fn processed_dir(date: &str) -> String {
    format!("/{date}/PROCESSED_DATA")
}

/// Where a sample's solve dirs live: `RESULTS/<sample>` (≤ 2025), or directly
/// `PROCESSED_DATA/<sample>` when the beamtime has no RESULTS folder (2026 layout).
pub fn sample_results_dir(date: &str, sample: &str, flat: bool) -> String {
    if flat {
        format!("/{date}/PROCESSED_DATA/{sample}")
    } else {
        format!("/{date}/PROCESSED_DATA/RESULTS/{sample}")
    }
}

pub fn raw_sample_dir(date: &str, sample: &str) -> String {
    format!("/{date}/RAW_DATA/{sample}")
}

/// Does `file` look like a frame of `<prefix>_<run>_NNNN.cbf`?
pub fn is_frame_of(file: &str, prefix: &str, run: u32) -> bool {
    let Some(rest) = file.strip_prefix(prefix) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(&format!("_{run}_")) else {
        return false;
    };
    let Some(num) = rest.strip_suffix(".cbf") else {
        return false;
    };
    !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
}

/// Does the raw file at `path` belong to run `run` of `prefix`? CBF frames carry both in
/// their name; XAIRA's Eiger HDF5 files sit in a folder named after the prefix.
pub fn frame_belongs(path: &str, prefix: &str, run: u32) -> bool {
    let mut parts = path.rsplit('/');
    let name = parts.next().unwrap_or(path);
    let parent = parts.next().unwrap_or("");
    is_frame_of(name, prefix, run) || (parent == prefix && name.ends_with(".h5"))
}

/// Beamtime ids are paths relative to the SFTP root: `20251211` for proposal accounts,
/// `bl06/projects/cycle2026-I/<proposal>/20260711` for beamline-tree accounts.
pub fn beamtime_date(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// `XALOC`, `XAIRA` … from the id's beamline folder, if it has one.
pub fn beamline_name(id: &str) -> Option<String> {
    let first = id.split('/').next()?;
    if first == id || !first.starts_with("bl") {
        return None;
    }
    Some(match first {
        "bl13" => "XALOC".to_string(),
        "bl06" => "XAIRA".to_string(),
        other => other.to_uppercase(),
    })
}

/// `XAIRA 20260711`, or just the date.
pub fn beamtime_label(id: &str) -> String {
    match beamline_name(id) {
        Some(bl) => format!("{bl} {}", beamtime_date(id)),
        None => beamtime_date(id).to_string(),
    }
}

/// ednaproc leaves a zero-byte file named `<SG number>_<a>_<b>_<c>_<al>_<be>_<ga>`.
pub fn parse_edna_cell_marker(name: &str) -> Option<(u32, [f64; 6])> {
    let c = EDNA_CELL.captures(name)?;
    let sg = c[1].parse().ok()?;
    let mut cell = [0.0; 6];
    for (i, v) in cell.iter_mut().enumerate() {
        *v = c[i + 2].parse().ok()?;
    }
    Some((sg, cell))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_new_layout_dirs() {
        assert_eq!(
            classify_dir("xia2chem_manually-mounted_3_1"),
            DirKind::Solve {
                pipeline: Pipeline::Xia2Chem,
                prefix: "manually-mounted".into(),
                run: 3,
                n: 1
            }
        );
        assert_eq!(
            classify_dir("characterisation_manually-mounted_run2_140127929074064"),
            DirKind::Characterisation {
                prefix: "manually-mounted".into(),
                run: 2
            }
        );
        assert_eq!(classify_dir("manually-mounted"), DirKind::Other);
    }

    #[test]
    fn classifies_old_layout_dirs() {
        assert_eq!(
            classify_dir("xia2_AS268_kappa0_2_1"),
            DirKind::Solve {
                pipeline: Pipeline::Xia2,
                prefix: "AS268_kappa0".into(),
                run: 2,
                n: 1
            }
        );
        assert_eq!(
            classify_dir("autoproc_JFN84_2_kappa0_a_3_1"),
            DirKind::Solve {
                pipeline: Pipeline::AutoProc,
                prefix: "JFN84_2_kappa0_a".into(),
                run: 3,
                n: 1
            }
        );
    }

    #[test]
    fn sample_dirs() {
        assert_eq!(sample_results_dir("20251211", "JSG285b", false), "/20251211/PROCESSED_DATA/RESULTS/JSG285b");
        assert_eq!(sample_results_dir("20260307", "JSG300kappa0", true), "/20260307/PROCESSED_DATA/JSG300kappa0");
        // 2026: solve dirs named after the sample, plus new pipelines we ignore
        assert!(matches!(classify_dir("xia2chem_JSG300kappa0_1_1"), DirKind::Solve { pipeline: Pipeline::Xia2Chem, run: 1, .. }));
        assert_eq!(classify_dir("mosflm_JSG300kappa0_1_1"), DirKind::Other);
        assert_eq!(classify_dir("JSG300kappa0_run1_1"), DirKind::Other);
    }

    #[test]
    fn xaira_dirs() {
        use crate::model::SolveKind;
        assert_eq!(
            classify_dir("BSample-19X01_w1_01_autoproc_noanom_run1"),
            DirKind::Processed {
                kind: SolveKind::AutoProcNoAnom,
                prefix: "BSample-19X01_w1_01".into(),
                run: 1
            }
        );
        assert!(matches!(
            classify_dir("BSample-19X01_w1_01_fastdp_noanom_run2"),
            DirKind::Processed { kind: SolveKind::FastDp, run: 2, .. }
        ));
        assert!(matches!(
            classify_dir("BSample-19X01_w1_01_xia2dials_noanom_run1"),
            DirKind::Processed { kind: SolveKind::Xia2, .. }
        ));
        assert_eq!(classify_dir("BSample-19X01_w1_01_autoproc_staraniso_noanom_run1"), DirKind::Other);
        assert_eq!(classify_dir("BSample-19X01_w1_01_dozorplot_run1"), DirKind::Other);
        assert_eq!(
            classify_dir("ref-BSample-19X01_01_testxtal_run1"),
            DirKind::Characterisation {
                prefix: "ref-BSample-19X01_01".into(),
                run: 1
            }
        );
    }

    #[test]
    fn beamtime_ids() {
        let id = "bl06/projects/cycle2026-I/2025000000-pi/20260711";
        assert_eq!(beamtime_date(id), "20260711");
        assert_eq!(beamtime_label(id), "XAIRA 20260711");
        assert_eq!(beamtime_label("bl13/projects/c/p/20260307"), "XALOC 20260307");
        assert_eq!(beamtime_label("20251211"), "20251211");
        assert_eq!(results_dir(id), "/bl06/projects/cycle2026-I/2025000000-pi/20260711/PROCESSED_DATA/RESULTS");
    }

    #[test]
    fn hdf5_frames_belong_to_their_dataset() {
        let p = "/x/RAW_DATA/Sample-19-01/BSample-19X01_w1_01/BSample-19X01_w1_01__data_000001.h5";
        assert!(frame_belongs(p, "BSample-19X01_w1_01", 1));
        assert!(!frame_belongs(p, "ref-BSample-19X01_01", 1));
        assert!(frame_belongs("/x/manually-mounted/manually-mounted_3_0001.cbf", "manually-mounted", 3));
    }

    #[test]
    fn test_names() {
        assert!(is_test_name("AS322test1"));
        assert!(is_test_name("JSGtest2"));
        assert!(!is_test_name("JSG285b"));
    }

    #[test]
    fn frames() {
        assert!(is_frame_of("manually-mounted_3_0001.cbf", "manually-mounted", 3));
        assert!(!is_frame_of("manually-mounted_3_0001.log", "manually-mounted", 3));
        assert!(!is_frame_of("manually-mounted_2_0001.cbf", "manually-mounted", 3));
        assert!(!is_frame_of("manually-mounted_33_0001.cbf", "manually-mounted", 3));
    }

    #[test]
    fn edna_marker() {
        let (sg, cell) = parse_edna_cell_marker("75_28.387_28.387_14.359_90.0_90.0_90.0").unwrap();
        assert_eq!(sg, 75);
        assert_eq!(cell[2], 14.359);
        assert!(parse_edna_cell_marker("ep_XDS_ASCII.HKL").is_none());
    }

}
