//! Where downloads go locally: `<base>/<sample>/<pipeline>_r<run>/` (DESIGN.md §4).

use crate::local::SOURCE_FILE;
use crate::model::{Run, Solve};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Written next to downloaded files so we know where they came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub date: String,
    pub sample: String,
    pub remote_dir: String,
    pub what: String,
    pub run: u32,
    pub downloaded_at: u64,
}

pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

pub fn solve_folder(solve: &Solve, run: &Run, multi_prefix: bool) -> String {
    let mut s = if multi_prefix {
        format!("{}_{}_r{}", solve.kind.folder(), sanitize(&run.prefix), run.run)
    } else {
        format!("{}_r{}", solve.kind.folder(), run.run)
    };
    if solve.n > 1 {
        s.push_str(&format!("_n{}", solve.n));
    }
    s
}

pub fn frames_folder(run: &Run, multi_prefix: bool) -> String {
    if multi_prefix {
        format!("frames_{}_r{}", sanitize(&run.prefix), run.run)
    } else {
        format!("frames_r{}", run.run)
    }
}

/// `<base>/<sample>/<folder>`, or `<folder>_<date>` when that folder already holds
/// data from another beamtime with the same sample name.
pub fn target_dir(base: &Path, sample: &str, folder: &str, date: &str) -> PathBuf {
    let dir = base.join(sanitize(sample)).join(folder);
    match read_source(&dir) {
        Some(src) if src.date != date => base.join(sanitize(sample)).join(format!("{folder}_{date}")),
        _ => dir,
    }
}

pub fn read_source(dir: &Path) -> Option<Source> {
    let text = std::fs::read_to_string(dir.join(SOURCE_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_source(dir: &Path, src: &Source) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join(SOURCE_FILE),
        serde_json::to_vec_pretty(src).unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SolveKind;

    fn run(prefix: &str, n: u32) -> Run {
        Run {
            prefix: prefix.into(),
            run: n,
            characterisation: false,
            solves: vec![],
        }
    }

    #[test]
    fn folder_names() {
        let s = Solve::new(SolveKind::AutoProcNoAnom, "/x", 1);
        assert_eq!(solve_folder(&s, &run("manually-mounted", 3), false), "autoproc_noanom_r3");
        assert_eq!(
            solve_folder(&s, &run("manually-mounted", 3), true),
            "autoproc_noanom_manually-mounted_r3"
        );
        let s2 = Solve::new(SolveKind::Xia2Chem, "/x", 2);
        assert_eq!(solve_folder(&s2, &run("p", 1), false), "xia2chem_r1_n2");
        assert_eq!(frames_folder(&run("p", 3), false), "frames_r3");
    }

    #[test]
    fn date_collision_gets_suffix() {
        let base = std::env::temp_dir().join(format!("alba_watch_dl_{}", std::process::id()));
        let dir = target_dir(&base, "MR8", "xia2chem_r1", "20251106");
        assert_eq!(dir, base.join("MR8").join("xia2chem_r1"));
        write_source(
            &dir,
            &Source {
                date: "20251106".into(),
                sample: "MR8".into(),
                remote_dir: "/x".into(),
                what: "xia2chem".into(),
                run: 1,
                downloaded_at: 0,
            },
        )
        .unwrap();
        assert_eq!(target_dir(&base, "MR8", "xia2chem_r1", "20251106"), dir);
        assert_eq!(
            target_dir(&base, "MR8", "xia2chem_r1", "20251211"),
            base.join("MR8").join("xia2chem_r1_20251211")
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("a:b/c"), "a_b_c");
    }
}
