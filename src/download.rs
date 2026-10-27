//! Downloads land in `<sample>/<program>/` inside the Local panel's current folder,
//! like the old notebook (`JSG285b/xia2chem_r3/shelxt.hkl`, `JSG285b/frames_r3/…`),
//! keeping the server's file names. A small `.alba_source.json` in each program folder
//! records where each file came from, so a second download of the same file is
//! recognised and unrelated files are never overwritten.

use crate::local::SOURCE_FILE;
use crate::model::{Run, Solve};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub date: String,
    pub sample: String,
    pub remote: String,
    pub what: String,
    pub run: u32,
    pub downloaded_at: u64,
}

/// File name → origin, for the files amanece downloaded into one folder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sources {
    pub files: BTreeMap<String, Source>,
}

pub fn read_sources(dir: &Path) -> Sources {
    std::fs::read_to_string(dir.join(SOURCE_FILE))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_sources(dir: &Path, s: &Sources) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(
        dir.join(SOURCE_FILE),
        serde_json::to_vec_pretty(s).unwrap_or_default(),
    )
}

/// Drops `name` from `dir/.alba_source.json` (after the file was deleted).
pub fn forget(dir: &Path, name: &str) {
    let mut s = read_sources(dir);
    if s.files.remove(name).is_none() {
        return;
    }
    if s.files.is_empty() {
        let _ = std::fs::remove_file(dir.join(SOURCE_FILE));
    } else {
        let _ = write_sources(dir, &s);
    }
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

/// Short tag naming a solve, e.g. `autoproc_anom_r3`; used to rename clashing files.
pub fn solve_tag(solve: &Solve, run: &Run, multi_prefix: bool) -> String {
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

pub fn frames_tag(run: &Run, multi_prefix: bool) -> String {
    if multi_prefix {
        format!("frames_{}_r{}", sanitize(&run.prefix), run.run)
    } else {
        format!("frames_r{}", run.run)
    }
}

/// The sample's folder: `cwd/<sample>`, or `cwd` itself when you are already in a
/// folder of that name.
pub fn sample_dir(cwd: &Path, sample: &str) -> std::path::PathBuf {
    let name = sanitize(sample);
    if cwd
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&name))
    {
        cwd.to_path_buf()
    } else {
        cwd.join(name)
    }
}

/// Folder for one solve inside the sample folder: `<tag>`, unless that folder already
/// holds files from another beamtime or sample (or someone else's); then
/// `<tag>_<date>`, then numbered.
pub fn solve_dir(parent: &Path, tag: &str, date: &str, sample: &str) -> std::path::PathBuf {
    let usable = |dir: &Path| -> bool {
        if !dir.exists() {
            return true;
        }
        let empty = std::fs::read_dir(dir).is_ok_and(|mut r| r.next().is_none());
        let src = read_sources(dir);
        empty
            || (!src.files.is_empty()
                && src.files.values().all(|s| s.date == date && s.sample == sample))
    };
    let day = crate::layout::beamtime_date(date);
    [tag.to_string(), format!("{tag}_{day}")]
        .into_iter()
        .chain((2..).map(|i| format!("{tag}_{day}_{i}")))
        .map(|name| parent.join(name))
        .find(|d| usable(d))
        .expect("some numbered folder is usable")
}

/// Local name for `remote` in `dir`: the server's file name when it's free (or
/// already holds this very file), otherwise `<stem>_<tag>.<ext>`, then numbered.
pub fn local_name(dir: &Path, sources: &Sources, taken: &HashSet<String>, remote: &str, tag: &str) -> String {
    let file = sanitize(remote.rsplit('/').next().unwrap_or(remote));
    let free = |name: &str| {
        !taken.contains(name)
            && match sources.files.get(name) {
                Some(src) => src.remote == remote,
                None => !dir.join(name).exists(),
            }
    };
    if free(&file) {
        return file;
    }
    let (stem, ext) = match file.rfind('.') {
        Some(i) if i > 0 => (&file[..i], &file[i..]),
        _ => (file.as_str(), ""),
    };
    let tagged = format!("{stem}_{tag}{ext}");
    if free(&tagged) {
        return tagged;
    }
    (2..)
        .map(|i| format!("{stem}_{tag}_{i}{ext}"))
        .find(|n| free(n))
        .expect("some numbered name is free")
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

    fn src(remote: &str) -> Source {
        Source {
            date: "20260307".into(),
            sample: "S".into(),
            remote: remote.into(),
            what: "x".into(),
            run: 1,
            downloaded_at: 0,
        }
    }

    #[test]
    fn tags() {
        let s = Solve::new(SolveKind::AutoProcAnom, "/x", 1);
        assert_eq!(solve_tag(&s, &run("manually-mounted", 3), false), "autoproc_anom_r3");
        assert_eq!(solve_tag(&s, &run("mm", 3), true), "autoproc_anom_mm_r3");
        let s2 = Solve::new(SolveKind::Xia2Chem, "/x", 2);
        assert_eq!(solve_tag(&s2, &run("p", 1), false), "xia2chem_r1_n2");
    }

    #[test]
    fn names_never_overwrite_unrelated_files() {
        let dir = std::env::temp_dir().join(format!("amanece_names_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut sources = Sources::default();
        let mut taken = HashSet::new();

        // free name: keep the server's name
        let a = local_name(&dir, &sources, &taken, "/r/autoproc/noanom/1/XDS_ASCII.HKL", "autoproc_noanom_r1");
        assert_eq!(a, "XDS_ASCII.HKL");
        taken.insert(a);
        // same name in the same batch: tagged
        let b = local_name(&dir, &sources, &taken, "/r/autoproc/anom/1/XDS_ASCII.HKL", "autoproc_anom_r1");
        assert_eq!(b, "XDS_ASCII_autoproc_anom_r1.HKL");

        // an unrelated local file with that name: tagged, not overwritten
        std::fs::write(dir.join("shelxt.hkl"), b"mine").unwrap();
        let c = local_name(&dir, &sources, &HashSet::new(), "/r/xia2chem/shelxt.hkl", "xia2chem_r1");
        assert_eq!(c, "shelxt_xia2chem_r1.hkl");

        // the same remote file downloaded before: same name again (the worker skips it)
        sources.files.insert("shelxt.hkl".into(), src("/r/xia2chem/shelxt.hkl"));
        let d = local_name(&dir, &sources, &HashSet::new(), "/r/xia2chem/shelxt.hkl", "xia2chem_r1");
        assert_eq!(d, "shelxt.hkl");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn one_folder_per_solve() {
        let cwd = std::env::temp_dir().join(format!("amanece_dirs_{}", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let first = solve_dir(&cwd, "xia2chem_r3", "20251211", "JSG285b");
        assert_eq!(first, cwd.join("xia2chem_r3"));
        let mut s = Sources::default();
        let mut origin = src("/r/shelxt.hkl");
        origin.date = "20251211".into();
        origin.sample = "JSG285b".into();
        s.files.insert("shelxt.hkl".into(), origin);
        write_sources(&first, &s).unwrap();
        // same sample again: same folder
        assert_eq!(solve_dir(&cwd, "xia2chem_r3", "20251211", "JSG285b"), first);
        // same sample name from another beamtime: dated folder
        assert_eq!(
            solve_dir(&cwd, "xia2chem_r3", "20260307", "JSG285b"),
            cwd.join("xia2chem_r3_20260307")
        );
        // a folder of the user's with that name (not ours, not empty): left alone
        std::fs::create_dir_all(cwd.join("ednaproc_r1")).unwrap();
        std::fs::write(cwd.join("ednaproc_r1").join("notes.txt"), b"mine").unwrap();
        assert_eq!(solve_dir(&cwd, "ednaproc_r1", "d", "S"), cwd.join("ednaproc_r1_d"));
        // sample folder: created inside cwd, not nested when already inside it
        assert_eq!(sample_dir(&cwd, "JSG285b"), cwd.join("JSG285b"));
        assert_eq!(sample_dir(&cwd.join("JSG285b"), "JSG285b"), cwd.join("JSG285b"));
        assert_eq!(frames_tag(&run("p", 3), false), "frames_r3");
        std::fs::remove_dir_all(&cwd).ok();
    }

    #[test]
    fn forget_drops_entries() {
        let dir = std::env::temp_dir().join(format!("amanece_forget_{}", std::process::id()));
        let mut s = Sources::default();
        s.files.insert("a.hkl".into(), src("/r/a.hkl"));
        s.files.insert("b.ins".into(), src("/r/b.ins"));
        write_sources(&dir, &s).unwrap();
        forget(&dir, "a.hkl");
        assert!(!read_sources(&dir).files.contains_key("a.hkl"));
        forget(&dir, "b.ins");
        assert!(!dir.join(SOURCE_FILE).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sources_roundtrip() {
        let dir = std::env::temp_dir().join(format!("amanece_src_{}", std::process::id()));
        let mut s = Sources::default();
        s.files.insert("a.hkl".into(), src("/r/a.hkl"));
        write_sources(&dir, &s).unwrap();
        assert_eq!(read_sources(&dir).files.get("a.hkl"), Some(&src("/r/a.hkl")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sanitizes() {
        assert_eq!(sanitize("a:b/c"), "a_b_c");
    }
}
