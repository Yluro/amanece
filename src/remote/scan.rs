//! Walks one sample's results and fills in its solves: XALOC's EDNA layout
//! (DESIGN.md §2.2–2.4) and XAIRA's EDNA2 folders with STATUS + all_files (§2.9).

use super::sftp::{Entry, ReadOnlySftp};
use crate::ispyb;
use crate::layout::{self, DirKind, Pipeline};
use crate::model::{RemoteFile, Run, SampleInfo, Solve, SolveKind, Stats, Status, now_secs};
use anyhow::Result;

/// ISPyB XMLs are 5–15 KB; anything far bigger is not what we expect.
const MAX_XML: u64 = 512 * 1024;
/// A solve with no result this long after its last change is treated as failed.
const STALE_SECS: u64 = 3 * 3600;

pub struct Experiments {
    /// Beamtime ids: `20251211`, or `<proposal folder>/20260711` (see `layout::beamtime_date`).
    pub ids: Vec<String>,
    /// What the root contains (for the "nothing found" message).
    pub top: Vec<String>,
}

pub async fn list_experiments(sftp: &ReadOnlySftp) -> Result<Experiments> {
    let mut ids = Vec::new();
    let mut top_names = Vec::new();
    for p in sftp.proposals().to_vec() {
        let dir = if p.is_empty() { "/".to_string() } else { format!("/{p}") };
        let top = sftp.list(&dir).await?;
        ids.extend(
            top.iter()
                .filter(|e| e.is_dir && layout::is_date_dir(&e.name))
                .map(|e| if p.is_empty() { e.name.clone() } else { format!("{p}/{}", e.name) }),
        );
        top_names.extend(top.into_iter().map(|e| e.name));
    }
    Ok(Experiments { ids, top: top_names })
}

/// Sample folders of a beamtime, and whether it uses the flat 2026 layout.
pub async fn list_samples(sftp: &ReadOnlySftp, date: &str) -> Result<(Vec<String>, bool)> {
    let dirs = |v: Vec<Entry>| -> Vec<String> {
        v.into_iter()
            .filter(|e| e.is_dir && e.name != "RESULTS")
            .map(|e| e.name)
            .collect()
    };
    match sftp.list(&layout::results_dir(date)).await {
        Ok(v) => Ok((dirs(v), false)),
        Err(first) => match sftp.list(&layout::processed_dir(date)).await {
            Ok(v) => Ok((dirs(v), true)),
            Err(_) => Err(first),
        },
    }
}

/// Frames of the given runs, looking in `RAW_DATA/<sample>` and its prefix subfolder.
pub async fn list_frames(
    sftp: &ReadOnlySftp,
    date: &str,
    sample: &str,
    runs: &[(String, u32)],
) -> Result<Vec<RemoteFile>> {
    let base = layout::raw_sample_dir(date, sample);
    let top = sftp.list(&base).await?;
    let mut dirs = vec![(base.clone(), top.clone())];
    let mut prefixes: Vec<&str> = runs.iter().map(|(p, _)| p.as_str()).collect();
    prefixes.dedup();
    for p in prefixes {
        if top.iter().any(|e| e.is_dir && e.name == p) {
            let sub = format!("{base}/{p}");
            let list = sftp.list(&sub).await?;
            dirs.push((sub, list));
        }
    }
    let mut out = Vec::new();
    for (dir, entries) in dirs {
        for e in entries.iter().filter(|e| !e.is_dir) {
            let path = format!("{dir}/{}", e.name);
            if runs.iter().any(|(p, r)| layout::frame_belongs(&path, p, *r)) {
                out.push(RemoteFile::new(path, e.size));
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    Ok(out)
}

#[derive(Clone, Copy)]
enum Style {
    /// XALOC/EDNA: `proc_<id>/` inside, one or two solves per folder.
    Classic(Pipeline),
    /// XAIRA/EDNA2: one solve per folder, `STATUS` and `all_files/`.
    Processed(SolveKind),
}

struct SolveDir {
    path: String,
    style: Style,
    prefix: String,
    run: u32,
    n: u32,
    /// Last change of the solve dir itself (from the parent listing).
    mtime: u64,
}

#[derive(Default)]
struct Found {
    solve_dirs: Vec<SolveDir>,
    char_runs: Vec<(String, u32)>,
}

impl Found {
    /// Sorts a sample subfolder into solve dirs, characterisation runs, or `other`.
    fn classify(&mut self, parent: &str, e: &Entry, other: &mut Vec<String>) {
        match layout::classify_dir(&e.name) {
            DirKind::Solve { pipeline, prefix, run, n } => self.solve_dirs.push(SolveDir {
                path: format!("{parent}/{}", e.name),
                style: Style::Classic(pipeline),
                prefix,
                run,
                n,
                mtime: e.mtime.map(u64::from).unwrap_or(0),
            }),
            DirKind::Processed { kind, prefix, run } => self.solve_dirs.push(SolveDir {
                path: format!("{parent}/{}", e.name),
                style: Style::Processed(kind),
                prefix,
                run,
                n: 1,
                mtime: e.mtime.map(u64::from).unwrap_or(0),
            }),
            DirKind::Characterisation { prefix, run } => self.char_runs.push((prefix, run)),
            DirKind::Other => other.push(format!("{parent}/{}", e.name)),
        }
    }
}

/// Scans a sample. Finished solves present in `known` are reused without touching
/// the server. `progress` is called after each solve dir so the UI fills in gradually.
pub async fn scan_sample(
    sftp: &ReadOnlySftp,
    date: &str,
    sample: &str,
    flat: bool,
    known: Option<&SampleInfo>,
    mut progress: impl FnMut(&SampleInfo),
) -> Result<SampleInfo> {
    let base = layout::sample_results_dir(date, sample, flat);
    let mut found = Found::default();

    // Old and 2026 layouts: solve dirs directly under the sample. Late 2025: one prefix
    // dir level in between. Only descend when the top level has no solve dirs.
    let mut prefix_dirs = Vec::new();
    for e in sftp.list(&base).await?.iter().filter(|e| e.is_dir) {
        found.classify(&base, e, &mut prefix_dirs);
    }
    if !found.solve_dirs.is_empty() || !found.char_runs.is_empty() {
        prefix_dirs.clear();
    }
    for dir in prefix_dirs {
        let mut ignored = Vec::new();
        for e in sftp.list(&dir).await?.iter().filter(|e| e.is_dir) {
            found.classify(&dir, e, &mut ignored);
        }
    }
    let Found { solve_dirs, char_runs } = found;

    // Skeleton: every run with placeholder solves, so the UI shows rows right away.
    let mut info = SampleInfo {
        name: sample.to_string(),
        runs: Vec::new(),
        scanned_at: now_secs(),
    };
    for d in &solve_dirs {
        let known_here: Vec<Solve> = known
            .map(|k| k.solves().filter(|s| s.dir == d.path).cloned().collect())
            .unwrap_or_default();
        let run = run_mut(&mut info, &d.prefix, d.run);
        if !known_here.is_empty() && known_here.iter().all(|s| s.status != Status::Running) {
            run.solves.extend(known_here);
        } else {
            for kind in kinds_of(d.style) {
                run.solves.push(Solve::new(kind, d.path.clone(), d.n));
            }
        }
    }
    for (prefix, r) in &char_runs {
        run_mut(&mut info, prefix, *r);
    }
    normalise(&mut info);
    progress(&info);

    for d in &solve_dirs {
        let needs_scan = info
            .solves()
            .filter(|s| s.dir == d.path)
            .any(|s| s.status == Status::Running);
        if !needs_scan {
            continue;
        }
        let fresh = match d.style {
            Style::Classic(Pipeline::AutoProc) => scan_autoproc(sftp, &d.path, d.n, d.mtime).await?,
            Style::Classic(Pipeline::EdnaProc) => vec![scan_ednaproc(sftp, &d.path, d.n, d.mtime).await?],
            Style::Classic(Pipeline::Xia2) => vec![scan_xia2(sftp, &d.path, d.n, d.mtime, false).await?],
            Style::Classic(Pipeline::Xia2Chem) => vec![scan_xia2(sftp, &d.path, d.n, d.mtime, true).await?],
            Style::Processed(kind) => vec![scan_processed(sftp, &d.path, kind, d.mtime).await],
        };
        let run = run_mut(&mut info, &d.prefix, d.run);
        run.solves.retain(|s| s.dir != d.path);
        run.solves.extend(fresh);
        normalise(&mut info);
        progress(&info);
    }
    Ok(info)
}

fn kinds_of(style: Style) -> Vec<SolveKind> {
    match style {
        Style::Classic(Pipeline::AutoProc) => vec![SolveKind::AutoProcNoAnom, SolveKind::AutoProcAnom],
        Style::Classic(Pipeline::EdnaProc) => vec![SolveKind::EdnaProc],
        Style::Classic(Pipeline::Xia2) => vec![SolveKind::Xia2],
        Style::Classic(Pipeline::Xia2Chem) => vec![SolveKind::Xia2Chem],
        Style::Processed(kind) => vec![kind],
    }
}

/// Files in a XAIRA solve's `all_files/`: (stats XML, HKL files, logs). Addressed by name
/// because autoPROC's `all_files/` holds ~800 files and is never listed.
fn processed_files(kind: SolveKind) -> (Option<&'static str>, &'static [&'static str], &'static [&'static str]) {
    match kind {
        SolveKind::AutoProcNoAnom | SolveKind::AutoProcAnom => {
            (Some("autoPROC.xml"), &["XDS_ASCII.HKL"], &["autoPROC.xml", "aimless.log"])
        }
        SolveKind::FastDp => (
            Some("fast_dp.xml"),
            &["XDS_ASCII.HKL"],
            &["fast_dp.xml", "fast_dp.log", "CORRECT.LP", "aimless.log"],
        ),
        // not seen succeed on XAIRA yet: status only
        SolveKind::Xia2 | SolveKind::Xia2Chem | SolveKind::EdnaProc => (None, &[], &[]),
    }
}

/// XAIRA solve: `STATUS` says SUCCESS / FAIL (absent while running); results in `all_files/`.
/// Two small reads: STATUS and the stats XML.
async fn scan_processed(sftp: &ReadOnlySftp, dir: &str, kind: SolveKind, dir_mtime: u64) -> Solve {
    let mut solve = Solve::new(kind, dir, 1);
    let status = sftp
        .read_small(&format!("{dir}/STATUS"), 256)
        .await
        .ok()
        .map(|b| String::from_utf8_lossy(&b).trim().to_ascii_uppercase());
    match status.as_deref() {
        Some(s) if s.starts_with("SUCCESS") => {
            let all = format!("{dir}/all_files");
            let (stats, hkl, logs) = processed_files(kind);
            if let Some(name) = stats {
                solve.stats = read_stats(sftp, &format!("{all}/{name}"), true).await;
            }
            solve.hkl = hkl.iter().map(|n| RemoteFile::new(format!("{all}/{n}"), None)).collect();
            solve.logs = logs.iter().map(|n| RemoteFile::new(format!("{all}/{n}"), None)).collect();
            solve.status = Status::Done;
            if hkl.is_empty() {
                solve.note = Some("finished; this pipeline's XAIRA output isn't read yet".into());
            } else if solve.stats.is_none() {
                solve.note = Some("finished, but its statistics file could not be read".into());
            }
        }
        Some(s) if s.starts_with("FAIL") => {
            solve.status = Status::Failed;
            solve.note = Some("the pipeline reported FAIL".into());
        }
        _ => {
            if dir_mtime > 0 && now_secs().saturating_sub(dir_mtime) > STALE_SECS {
                solve.status = Status::Failed;
                solve.note = Some("no result after 3 h".into());
            }
        }
    }
    solve
}

fn run_mut<'a>(info: &'a mut SampleInfo, prefix: &str, run: u32) -> &'a mut Run {
    if let Some(i) = info
        .runs
        .iter()
        .position(|r| r.prefix == prefix && r.run == run)
    {
        return &mut info.runs[i];
    }
    info.runs.push(Run {
        prefix: prefix.to_string(),
        run,
        characterisation: false,
        solves: Vec::new(),
    });
    info.runs.last_mut().expect("just pushed")
}

fn normalise(info: &mut SampleInfo) {
    info.runs
        .sort_by(|a, b| a.prefix.cmp(&b.prefix).then(a.run.cmp(&b.run)));
    for r in &mut info.runs {
        r.characterisation = r.solves.is_empty();
        r.solves.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.n.cmp(&b.n)));
    }
}

fn find<'a>(entries: &'a [Entry], name: &str) -> Option<&'a Entry> {
    entries.iter().find(|e| e.name == name)
}

fn file(entries: &[Entry], dir: &str, name: &str) -> Option<RemoteFile> {
    find(entries, name)
        .filter(|e| !e.is_dir)
        .map(|e| RemoteFile::new(format!("{dir}/{name}"), e.size))
}

enum ProcDir {
    /// ≤ 2025: results live in `proc_<id>/`.
    Found(String),
    /// 2026: no `proc_<id>`; the solve dir itself (usually only job files).
    Flat(Vec<Entry>),
}

/// Locate `proc_<id>` inside a solve dir.
async fn proc_dir(sftp: &ReadOnlySftp, dir: &str) -> Result<ProcDir> {
    let entries = sftp.list(dir).await?;
    Ok(
        match entries
            .iter()
            .find(|e| e.is_dir && e.name.starts_with("proc_"))
        {
            Some(e) => ProcDir::Found(format!("{dir}/{}", e.name)),
            None => ProcDir::Flat(entries),
        },
    )
}

/// 2026 layout: the solve dir normally holds only the job files (Slurm script,
/// input.yml, an empty *Output_*.xml) because results stayed on the cluster.
/// Picks up result files anyway in case they show up there later.
async fn flat_solve(
    sftp: &ReadOnlySftp,
    kind: SolveKind,
    dir: &str,
    n: u32,
    dir_mtime: u64,
    entries: &[Entry],
) -> Solve {
    let mut solve = Solve::new(kind, dir, n);
    for name in ["shelxt.hkl", "shelxt.ins", "ep_XDS_ASCII.HKL", "XDS_ASCII.HKL"] {
        if let Some(f) = file(entries, dir, name) {
            solve.hkl.push(f);
        }
    }
    for name in ["ispyb.xml", "ISPyBStoreAutoProcv1_4_dataInput.xml"] {
        if find(entries, name).is_some() {
            solve.stats = read_stats(sftp, &format!("{dir}/{name}"), name == "ispyb.xml").await;
            break;
        }
    }
    let ended = entries
        .iter()
        .any(|e| e.name.contains("Output_") && e.name.ends_with(".xml"));
    if solve.hkl.is_empty() && solve.stats.is_none() && ended {
        solve.status = Status::Failed;
        solve.note = Some("job ran, but its results were not copied back to the server (only job files here)".into());
    } else {
        settle(&mut solve, ended, latest_mtime(entries).max(dir_mtime));
    }
    solve
}

fn latest_mtime(entries: &[Entry]) -> u64 {
    entries
        .iter()
        .filter_map(|e| e.mtime)
        .max()
        .map(u64::from)
        .unwrap_or(0)
}

/// Pipeline ended (ControlXXX_dataOutput.xml written) or went quiet for hours.
fn settle(solve: &mut Solve, ended: bool, latest: u64) {
    if solve.stats.is_some() && (ended || !solve.hkl.is_empty()) {
        solve.status = Status::Done;
    } else if ended {
        solve.status = Status::Failed;
        solve.note.get_or_insert_with(|| "pipeline ended without statistics".into());
    } else if latest > 0 && now_secs().saturating_sub(latest) > STALE_SECS {
        solve.status = Status::Failed;
        solve.note = Some("no result after 3 h".into());
    } else {
        solve.status = Status::Running;
    }
}

/// `r_fraction`: see `ispyb::parse` (EDNA's ISPyB files use percent, the rest fractions).
async fn read_stats(sftp: &ReadOnlySftp, path: &str, r_fraction: bool) -> Option<Stats> {
    let bytes = sftp.read_small(path, MAX_XML).await.ok()?;
    ispyb::parse(&String::from_utf8_lossy(&bytes), r_fraction).ok()
}

async fn scan_autoproc(sftp: &ReadOnlySftp, dir: &str, n: u32, dir_mtime: u64) -> Result<Vec<Solve>> {
    let proc = match proc_dir(sftp, dir).await? {
        ProcDir::Found(p) => p,
        ProcDir::Flat(entries) => {
            return Ok(vec![
                flat_solve(sftp, SolveKind::AutoProcNoAnom, dir, n, dir_mtime, &entries).await,
            ]);
        }
    };
    let top = sftp.list(&proc).await?;
    let ended = top
        .iter()
        .any(|e| e.name.starts_with("ControlAutoPROC") && e.name.ends_with("_dataOutput.xml"));
    let latest = latest_mtime(&top);
    let mut out = Vec::new();
    for (kind, suffix) in [
        (SolveKind::AutoProcNoAnom, "noanom"),
        (SolveKind::AutoProcAnom, "anom"),
    ] {
        let ap_name = format!("AutoPROCv1_0_{suffix}");
        let isp_name = format!("ISPyBStoreAutoProcv1_4_{suffix}");
        if find(&top, &ap_name).is_none() && find(&top, &isp_name).is_none() {
            continue;
        }
        let mut solve = Solve::new(kind, dir, n);
        if find(&top, &isp_name).is_some() {
            let isp = format!("{proc}/{isp_name}");
            let files = sftp.list(&isp).await?;
            if find(&files, "ISPyBStoreAutoProcv1_4_dataInput.xml").is_some() {
                solve.stats =
                    read_stats(sftp, &format!("{isp}/ISPyBStoreAutoProcv1_4_dataInput.xml"), false).await;
            }
        }
        if find(&top, &ap_name).is_some() {
            let ap = format!("{proc}/{ap_name}");
            let files = sftp.list(&ap).await?;
            // The numeric dir is autoPROC's huge working dir: never list it, address the HKL directly.
            if let Some(id) = files.iter().find(|e| {
                e.is_dir
                    && e.name.chars().all(|c| c.is_ascii_digit())
                    && find(&files, &format!("{}.dat", e.name)).is_some()
            }) {
                solve.hkl.push(RemoteFile::new(format!("{ap}/{}/XDS_ASCII.HKL", id.name), None));
            }
            for name in ["summary.html", "report.pdf", "autoPROC.xml"] {
                if let Some(f) = file(&files, &ap, name) {
                    solve.logs.push(f);
                }
            }
        }
        settle(&mut solve, ended, latest);
        if find(&top, &format!("ISPyBStoreAutoProcv1_4_{suffix}_failed")).is_some() && solve.stats.is_none() {
            solve.status = Status::Failed;
            solve.note = Some("autoPROC reported a failure".into());
            solve.hkl.clear();
        }
        out.push(solve);
    }
    if out.is_empty() {
        let mut s = Solve::new(SolveKind::AutoProcNoAnom, dir, n);
        settle(&mut s, ended, latest);
        out.push(s);
    }
    Ok(out)
}

async fn scan_ednaproc(sftp: &ReadOnlySftp, dir: &str, n: u32, dir_mtime: u64) -> Result<Solve> {
    let mut solve = Solve::new(SolveKind::EdnaProc, dir, n);
    let proc = match proc_dir(sftp, dir).await? {
        ProcDir::Found(p) => p,
        ProcDir::Flat(entries) => {
            return Ok(flat_solve(sftp, SolveKind::EdnaProc, dir, n, dir_mtime, &entries).await);
        }
    };
    let top = sftp.list(&proc).await?;
    let mut ended = top
        .iter()
        .any(|e| e.name.starts_with("ControlEDNAproc") && e.name.ends_with("_dataOutput.xml"));
    let latest = latest_mtime(&top);
    let mut marker = None;

    if find(&top, "results").is_some() {
        let res = format!("{proc}/results");
        let files = sftp.list(&res).await?;
        ended |= find(&files, ".finished").is_some();
        if let Some(f) = file(&files, &res, "ep_XDS_ASCII.HKL") {
            solve.hkl.push(f);
        }
        for e in files.iter().filter(|e| !e.is_dir) {
            if e.name.ends_with("_CORRECT.LP") || (e.name.contains("_aimless_") && e.name.ends_with(".log")) {
                solve.logs.push(RemoteFile::new(format!("{res}/{}", e.name), e.size));
            }
            if marker.is_none() {
                marker = layout::parse_edna_cell_marker(&e.name);
            }
        }
    }

    // Up to two ISPyB stores (anom / noanom); prefer the non-anomalous statistics.
    let mut isp: Vec<&Entry> = top
        .iter()
        .filter(|e| e.is_dir && e.name.starts_with("ISPyBStoreAutoProcv1_4"))
        .collect();
    isp.sort_by(|a, b| a.name.cmp(&b.name));
    for e in isp.into_iter().take(2) {
        let path = format!("{proc}/{}/ISPyBStoreAutoProcv1_4_dataInput.xml", e.name);
        if let Some(st) = read_stats(sftp, &path, false).await {
            let noanom = st.anomalous == Some(false);
            if solve.stats.is_none() || noanom {
                solve.stats = Some(st);
            }
            if noanom {
                break;
            }
        }
    }
    if ended && solve.hkl.is_empty() {
        solve.note = Some("ednaproc produced no HKL (see its CORRECT.LP / logs)".into());
    }
    if solve.stats.is_none()
        && let Some((sg, cell)) = marker
    {
        solve.stats = Some(Stats {
            space_group: Some(format!("#{sg}")),
            cell: Some(cell),
            ..Stats::default()
        });
    }
    settle(&mut solve, ended, latest);
    Ok(solve)
}

async fn scan_xia2(sftp: &ReadOnlySftp, dir: &str, n: u32, dir_mtime: u64, chem: bool) -> Result<Solve> {
    let kind = if chem { SolveKind::Xia2Chem } else { SolveKind::Xia2 };
    let mut solve = Solve::new(kind, dir, n);
    if !chem {
        solve.note = Some("stats only: xia2 protein mode writes no SHELX HKL".into());
    }
    let proc = match proc_dir(sftp, dir).await? {
        ProcDir::Found(p) => p,
        ProcDir::Flat(entries) => {
            let mut s = flat_solve(sftp, kind, dir, n, dir_mtime, &entries).await;
            if s.note.is_none() {
                s.note = solve.note;
            }
            return Ok(s);
        }
    };
    let top = sftp.list(&proc).await?;
    let ended = top
        .iter()
        .any(|e| e.name.starts_with("ControlXia2") && e.name.ends_with("_dataOutput.xml"));
    let latest = latest_mtime(&top);
    let xd = find(&top, "Xia2DIALSv1_0_noanom")
        .or_else(|| top.iter().find(|e| e.is_dir && e.name.starts_with("Xia2DIALSv1_0")))
        .map(|e| format!("{proc}/{}", e.name));
    if let Some(xd) = xd {
        let files = sftp.list(&xd).await?;
        if find(&files, "ispyb.xml").is_some() {
            solve.stats = read_stats(sftp, &format!("{xd}/ispyb.xml"), true).await;
        }
        for name in ["xia2.txt", "xia2-summary.dat", "xia2.html", "ispyb.xml", "xia2-error.txt"] {
            if let Some(f) = file(&files, &xd, name) {
                solve.logs.push(f);
            }
        }
        if solve.stats.is_none() && find(&files, "xia2-error.txt").is_some() {
            solve.note = Some("xia2 failed (xia2-error.txt is in the logs)".into());
        }
        if chem && find(&files, "DataFiles").is_some() {
            let df = format!("{xd}/DataFiles");
            let data = sftp.list(&df).await?;
            for name in ["shelxt.hkl", "shelxt.ins"] {
                if let Some(f) = file(&data, &df, name) {
                    solve.hkl.push(f);
                }
            }
        }
    }
    settle(&mut solve, ended, latest);
    Ok(solve)
}
