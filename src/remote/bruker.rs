//! The group's Bruker diffractometer server (docs/BRUKER.md). Pure functions only: frame
//! names → runs, which files of `work/` may hold a cell, and the cell files' text → a cell.
//! Nothing here touches the network, so every rule is tested on real files from
//! `docs/server_samples/`; listing and scanning build on it (milestone 3).

// `list_frames` and a few helpers wait for milestones 4-5.
#![allow(dead_code)]

use super::bruker_quality as quality;
use super::nas;
use super::scan::Experiments;
use super::sftp::{Entry, ReadOnlySftp};
use crate::model::{FrameSet, RemoteFile, Run, RunKind, SampleInfo, Solve, SolveKind, Stats, Status, now_secs};
use anyhow::{Result, bail, ensure};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;

/// An experiment's characterisation folder: the `.p4p` and other processing files.
pub const WORK_DIR: &str = "work";
/// How many cell files of one experiment are read per scan, newest first (the others are
/// only named): one `work/` folder had 32 CELL_NOW files.
pub const MAX_CELL_FILES: usize = 10;

// ---------------------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------------------

/// `<prefix>_<run>_<frame>.sfrm`: `3286AS264_B_01_0001.sfrm`, or `matrix_01_0001.sfrm` for
/// the unit-cell runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameName {
    pub prefix: String,
    pub run: u32,
    pub frame: u32,
}

impl FrameName {
    pub fn is_matrix(&self) -> bool {
        self.prefix.eq_ignore_ascii_case("matrix")
    }

    /// SAINT's own frames, not data (SAINT manual §3.4.1): `*_am_` active-pixel mask, `*_ib_`
    /// initial background, `*_le_` long exposure, `*_xa_` x-ray aperture, and `bg_snap_*`.
    pub fn is_helper(&self) -> bool {
        let p = self.prefix.to_ascii_lowercase();
        ["_am", "_ib", "_le", "_xa"].iter().any(|s| p.ends_with(s)) || p == "bg_snap"
    }
}

/// Splits a frame's file name. The run and frame numbers are the last two `_` groups, so the
/// prefix may hold underscores and digits (`sample_2_01_0001.sfrm` → `sample_2`, run 1).
pub fn frame_of(file: &str) -> Option<FrameName> {
    let (stem, ext) = file.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("sfrm") {
        return None;
    }
    let (rest, frame) = stem.rsplit_once('_')?;
    let (prefix, run) = rest.rsplit_once('_')?;
    let digits = |s: &str, len: std::ops::RangeInclusive<usize>| {
        len.contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
    };
    if prefix.is_empty() || !digits(run, 1..=3) || !digits(frame, 3..=5) {
        return None;
    }
    Some(FrameName { prefix: prefix.to_string(), run: run.parse().ok()?, frame: frame.parse().ok()? })
}

/// The frames of one run, as the experiment folder's listing shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameRun {
    pub prefix: String,
    pub run: u32,
    pub matrix: bool,
    pub count: u32,
    pub first: u32,
    pub last: u32,
    pub bytes: u64,
    /// Oldest and newest file time (unix seconds), when the listing gave them.
    pub oldest: Option<u32>,
    pub newest: Option<u32>,
}

impl FrameRun {
    /// Frames numbered `first..=last` but fewer files than that: a run cut short or
    /// still being collected, or files missing.
    pub fn has_gaps(&self) -> bool {
        u64::from(self.last - self.first) + 1 != u64::from(self.count)
    }
}

/// Groups an experiment folder's files into runs: data runs by prefix and number, then the
/// matrix runs. Folders, links and files that are not frames are ignored.
pub fn group_frames(entries: &[Entry]) -> Vec<FrameRun> {
    let mut runs: BTreeMap<(bool, String, u32), FrameRun> = BTreeMap::new();
    for e in entries.iter().filter(|e| !e.is_dir && !e.is_symlink) {
        let Some(f) = frame_of(&e.name).filter(|f| !f.is_helper()) else { continue };
        let matrix = f.is_matrix();
        // "Matrix" and "matrix" are one run
        let prefix = if matrix { "matrix".to_string() } else { f.prefix.clone() };
        let run = runs.entry((matrix, prefix.clone(), f.run)).or_insert_with(|| FrameRun {
            prefix,
            run: f.run,
            matrix,
            count: 0,
            first: f.frame,
            last: f.frame,
            bytes: 0,
            oldest: None,
            newest: None,
        });
        run.count += 1;
        run.first = run.first.min(f.frame);
        run.last = run.last.max(f.frame);
        run.bytes += e.size.unwrap_or(0);
        if let Some(t) = e.mtime {
            run.oldest = Some(run.oldest.map_or(t, |o| o.min(t)));
            run.newest = Some(run.newest.map_or(t, |n| n.max(t)));
        }
    }
    runs.into_values().collect()
}

/// Does the folder have a `work/` subfolder?
pub fn has_work(entries: &[Entry]) -> bool {
    entries.iter().any(|e| e.is_dir && !e.is_symlink && e.name.eq_ignore_ascii_case(WORK_DIR))
}

// ---------------------------------------------------------------------------------------
// Which files of work/ to read
// ---------------------------------------------------------------------------------------

/// What kind of file a name is, by extension, never by the experiment's name (the stems
/// differ: `3286AS264_B_0m.p4p` next to `3486S264_A.ins`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// `*.p4p`: whatever wrote it says what it is (`FILEID`, see `parse_p4p`)
    P4p,
    /// `*.p4p_cn`: CELL_NOW's output
    P4pCn,
    /// `*.ins`, `*.res`: SHELX, cell and lattice centring
    Shelx,
}

impl FileKind {
    /// Bytes to read from the start: the cell sits in the first lines of each kind.
    pub fn read_limit(self) -> u64 {
        match self {
            FileKind::P4p | FileKind::P4pCn | FileKind::Shelx => 4 * 1024,
        }
    }
}

/// By extension only: a `.p4p`'s name says nothing reliable (APEX exports get random names like
/// `5f9dbq.p4p`), its `FILEID` line does (docs/BRUKER.md §1c).
pub fn file_kind(name: &str) -> Option<FileKind> {
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "p4p" => Some(FileKind::P4p),
        "p4p_cn" => Some(FileKind::P4pCn),
        "ins" | "res" => Some(FileKind::Shelx),
        _ => None,
    }
}

/// The files of `work/` to read, newest first, and the names left out because of the cap.
#[derive(Debug, Default, PartialEq)]
pub struct Candidates {
    pub read: Vec<(String, FileKind)>,
    pub skipped: Vec<String>,
}

pub fn select_candidates(entries: &[Entry], cap: usize) -> Candidates {
    let mut all: Vec<(&Entry, FileKind)> = entries
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink && nas::visible(&e.name))
        .filter_map(|e| Some((e, file_kind(&e.name)?)))
        .collect();
    // newest first; files without a time last; the name settles ties
    all.sort_by(|(a, _), (b, _)| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    let mut out = Candidates::default();
    for (i, (e, kind)) in all.into_iter().enumerate() {
        if i < cap {
            out.read.push((e.name.clone(), kind));
        } else {
            out.skipped.push(e.name.clone());
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Cell files
// ---------------------------------------------------------------------------------------

/// Where a cell came from, in the order the rows are listed (source first, then newest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CellSource {
    /// "matrix": the cell APEX held when it wrote the file (`FILEID APEX2`, `APEX3` …, the
    /// `nac.p4p` of `FILEID Seattle`, a `CELL_NOW` export with a real cell): the cell of the
    /// matrix runs, usually after the Bravais choice
    Matrix,
    /// "integrated": SAINT's `_0m.p4p` / `_0u.p4p` (and `_01m`, twins), refined on all the data
    SaintP4p,
    /// "final": XPREP's `.ins` and the `.res`, in the setting XPREP chose
    Shelx,
    /// A `.p4p` of some other program
    OtherP4p,
    /// CELL_NOW's indexing of the matrix spots
    CellNow,
}

impl CellSource {
    pub fn label(self) -> &'static str {
        match self {
            CellSource::Matrix => "matrix",
            CellSource::SaintP4p => "integrated",
            CellSource::Shelx => "final",
            CellSource::OtherP4p => "p4p",
            CellSource::CellNow => "CELL_NOW",
        }
    }
}

/// A file that is known not to hold a cell (CELL_NOW's input, a placeholder, a log from before
/// the integration started): no row for it, and not a failure either.
#[derive(Debug)]
pub struct NotACell(pub String);

impl std::fmt::Display for NotACell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotACell {}

/// A cell read from one file of `work/`.
#[derive(Debug, Clone, PartialEq)]
pub struct CellFile {
    pub source: CellSource,
    pub cell: [f64; 6],
    /// Standard deviations of the cell (`CELLSD`), when the file has real ones.
    pub esd: Option<[f64; 6]>,
    /// P A B C I F R when the file says so (SAINT's `BRAVAIS`, SHELX's `LATT`).
    pub centring: Option<char>,
    /// `Triclinic P`
    pub bravais: Option<String>,
    /// `SAINT V8.34A`
    pub program: Option<String>,
    /// `2025-07-25 09:06:55`, when the file says when it was written.
    pub date: Option<String>,
    pub wavelength: Option<f64>,
    pub title: Option<String>,
}

impl CellFile {
    /// Same convention as the NAS reader: a letter without a space group says "centring
    /// only"; no centring is no space group (the search then treats the cell as primitive).
    pub fn space_group(&self) -> Option<String> {
        self.centring.map(|c| format!("{c} (centring only)"))
    }

    pub fn stats(&self) -> Stats {
        Stats { cell: Some(self.cell), space_group: self.space_group(), ..Stats::default() }
    }

    /// One line for the row: what it is and what it lacks.
    pub fn describe(&self) -> String {
        let mut parts = vec![self.source.label().to_string()];
        if let Some(p) = &self.program {
            parts.push(p.clone());
        }
        if let Some(d) = &self.date {
            parts.push(d.clone());
        }
        if let Some(w) = self.wavelength {
            parts.push(format!("λ {w}"));
        }
        if let Some(t) = &self.title {
            parts.push(t.clone());
        }
        parts.push(
            match self.centring {
                Some(c) => format!("centring {c}"),
                None => "centring unknown".to_string(),
            },
        );
        parts.push("cell only, no processing quality".into());
        parts.join(" · ")
    }
}

/// Reads one cell file. Errors say why a file gives no cell (the name goes in a note, never a
/// guessed cell): CELL_NOW's input, a placeholder cell, an impossible cell.
pub fn parse_cell_file(name: &str, text: &str) -> Result<CellFile> {
    match file_kind(name) {
        Some(FileKind::P4p | FileKind::P4pCn) => parse_p4p(text),
        Some(FileKind::Shelx) => parse_shelx(name, text),
        None => bail!("{name} is not a cell file"),
    }
}

/// The cell APEX writes when it has nothing: CELL_NOW's spot lists carry it.
fn is_placeholder(c: [f64; 6]) -> bool {
    c.iter().zip([10.0, 10.0, 10.0, 90.0, 90.0, 90.0]).all(|(a, b)| (a - b).abs() < 1e-3)
}

fn floats<'a>(values: impl Iterator<Item = &'a str>, n: usize) -> Option<Vec<f64>> {
    let v: Vec<f64> = values.take(n).map(str::parse).collect::<std::result::Result<_, _>>().ok()?;
    (v.len() == n).then_some(v)
}

fn cell6<'a>(values: impl Iterator<Item = &'a str>) -> Option<[f64; 6]> {
    floats(values, 6)?.try_into().ok()
}

/// `07/25/25` or `07/25/2025`, with an optional `09:06:55` → `2025-07-25 09:06:55`.
fn us_date(date: &str, time: Option<&str>) -> Option<String> {
    let mut p = date.split('/');
    let (m, d, y): (u32, u32, u32) = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    if p.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if y < 100 { 2000 + y } else { y };
    let mut out = format!("{y:04}-{m:02}-{d:02}");
    if let Some(t) = time.filter(|t| t.len() == 8 && t.as_bytes()[2] == b':') {
        out.push(' ');
        out.push_str(t);
    }
    Some(out)
}

/// A `.p4p` or `.p4p_cn`. Which program wrote it (`FILEID`) decides what it is.
pub fn parse_p4p(text: &str) -> Result<CellFile> {
    // APEX leaves empty `.p4p` files next to SAINT's job files (`cmy9z85_.p4p`, `.ini`, `.slm`)
    if text.trim().is_empty() {
        return Err(NotACell("empty file".into()).into());
    }
    let mut source = CellSource::OtherP4p;
    let (mut program, mut date, mut title, mut wavelength) = (None, None, None, None);
    let (mut cell, mut esd, mut centring, mut bravais) = (None, None, None, None);
    let mut cell_now_export = false;
    let mut more_components = 0;
    for line in text.lines() {
        let mut f = line.split_whitespace();
        match f.next() {
            Some("FILEID") => {
                let t: Vec<&str> = f.collect();
                let prog = t.first().copied().unwrap_or("?");
                let upper = prog.to_ascii_uppercase();
                source = match upper.as_str() {
                    "SAINT" => CellSource::SaintP4p,
                    "C_NOW" => CellSource::CellNow,
                    // APEX's export for CELL_NOW: the spot list, with the cell APEX had then
                    "CELL_NOW" => {
                        cell_now_export = true;
                        CellSource::Matrix
                    }
                    "SEATTLE" => CellSource::Matrix,
                    _ if upper.starts_with("APEX") => CellSource::Matrix,
                    _ => CellSource::OtherP4p,
                };
                let version = t.get(1).copied().filter(|v| *v != "?");
                program = Some(match version {
                    Some(v) => format!("{prog} {v}"),
                    None => prog.to_string(),
                });
                date = t.get(3).and_then(|d| us_date(d, t.get(4).copied()));
            }
            Some("TITLE") => {
                let rest = line["TITLE".len()..].trim();
                title = (!rest.is_empty() && rest != "?").then(|| rest.to_string());
            }
            Some("CELL") => cell = cell6(f),
            // a second or third component (twin domains, a second crystal): the row shows the first
            Some(k) if k.len() == 5 && k.starts_with("CELL") && k[4..].bytes().all(|b| b.is_ascii_digit()) => {
                more_components += 1
            }
            // zeros are what a program writes when it has no standard deviations
            Some("CELLSD") => esd = cell6(f).filter(|s| s.iter().any(|v| *v != 0.0)),
            Some("SOURCE") => wavelength = f.nth(1).and_then(|w| w.parse().ok()),
            Some("BRAVAIS") => {
                let t: Vec<&str> = f.collect();
                let letter = t.last().filter(|l| l.len() == 1).and_then(|l| l.chars().next());
                match letter.map(|c| c.to_ascii_uppercase()).filter(|c| "PABCIFR".contains(*c)) {
                    Some(c) if t.len() >= 2 => {
                        centring = Some(c);
                        bravais = Some(t.join(" "));
                    }
                    _ => bail!("unknown lattice centring in the BRAVAIS line: {}", t.join(" ")),
                }
            }
            // the reflection list starts: everything we need is above it
            Some(k) if k.starts_with("REF") => break,
            _ => {}
        }
    }
    let Some(cell) = cell.filter(|c| !is_placeholder(*c)) else {
        let what = if cell_now_export {
            "an export for CELL_NOW made before any cell was found (its spot list, with the 10 10 10 90 90 90 placeholder)"
        } else if cell.is_some() {
            "placeholder cell 10 10 10 90 90 90, not a measured one"
        } else {
            bail!("no CELL line");
        };
        return Err(NotACell(what.into()).into());
    };
    ensure!(nas::valid_cell(cell), "not a valid unit cell: {cell:?}");
    if more_components > 0 {
        let more = format!("{} more component(s) in the file", more_components);
        title = Some(match title {
            Some(t) => format!("{t}; {more}"),
            None => more,
        });
    }
    Ok(CellFile { source, cell, esd, centring, bravais, program, date, wavelength, title })
}

/// A SHELX `.ins` or `.res`: `CELL` with the lattice centring from `LATT`.
pub fn parse_shelx(name: &str, text: &str) -> Result<CellFile> {
    let stats = nas::parse_header(name, text)?;
    let centring = stats.space_group.as_deref().and_then(|s| s.chars().next());
    let title = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("TITL"))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let wavelength = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("CELL"))
        .and_then(|l| l.split_whitespace().next()?.parse().ok());
    Ok(CellFile {
        source: CellSource::Shelx,
        cell: stats.cell.expect("parse_header only succeeds with a cell"),
        esd: None,
        centring,
        bravais: None,
        program: None,
        date: None,
        wavelength,
        title,
    })
}

// ---------------------------------------------------------------------------------------
// Listing and scanning
// ---------------------------------------------------------------------------------------

/// What the scan needs of the server: a listing and a bounded read. The SFTP session, or a
/// fake in the tests.
pub trait Source {
    fn list(&self, path: &str) -> impl Future<Output = Result<Vec<Entry>>> + Send;
    fn read_prefix(&self, path: &str, max_bytes: u64) -> impl Future<Output = Result<Vec<u8>>> + Send;
}

impl Source for ReadOnlySftp {
    async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        ReadOnlySftp::list(self, path).await
    }

    async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
        ReadOnlySftp::read_prefix(self, path, max_bytes).await
    }
}

fn is_folder(e: &Entry) -> bool {
    e.is_dir && !e.is_symlink && nas::visible(&e.name)
}

/// One request: the person folders under the root. Each is a "beamtime" (`bruker-<hash>/<person>`).
pub async fn list_experiments(sftp: &ReadOnlySftp) -> Result<Experiments> {
    let id = sftp.nas_id.as_deref().ok_or_else(|| anyhow::anyhow!("not a Bruker session"))?;
    let top = sftp.list("/").await?;
    let mut ids: Vec<String> = top.iter().filter(|e| is_folder(e)).map(|e| format!("{id}/{}", e.name)).collect();
    ids.sort_by_key(|i| i.to_lowercase());
    Ok(Experiments { ids, top: top.into_iter().map(|e| e.name).collect() })
}

/// One request: the experiment folders of a person. Every folder is a sample, as on ALBA.
pub async fn list_samples(sftp: &ReadOnlySftp, date: &str) -> Result<(Vec<String>, bool)> {
    let base = nas::group_path(sftp, date)?;
    ensure!(!base.is_empty(), "not a person folder: {date}");
    let mut names: Vec<String> = sftp.list(&base).await?.iter().filter(|e| is_folder(e)).map(|e| e.name.clone()).collect();
    names.sort_by_key(|n| n.to_lowercase());
    Ok((names, false))
}

pub async fn scan_sample(
    sftp: &ReadOnlySftp,
    date: &str,
    sample: &str,
    known: Option<&SampleInfo>,
    progress: impl FnMut(&SampleInfo),
) -> Result<SampleInfo> {
    ensure!(nas::visible(sample), "invalid experiment folder name: {sample}");
    let base = nas::group_path(sftp, date)?;
    scan_dir(sftp, &format!("{base}/{sample}"), sample, known, progress).await
}

/// Frames of the given runs (`("matrix", 1)` is a matrix run): one listing of the experiment folder.
pub async fn list_frames(sftp: &ReadOnlySftp, date: &str, sample: &str, runs: &[(String, u32)]) -> Result<Vec<RemoteFile>> {
    ensure!(nas::visible(sample), "invalid experiment folder name: {sample}");
    let dir = format!("{}/{sample}", nas::group_path(sftp, date)?);
    frames_of(sftp, &dir, runs).await
}

async fn frames_of<S: Source>(src: &S, dir: &str, runs: &[(String, u32)]) -> Result<Vec<RemoteFile>> {
    let mut out: Vec<RemoteFile> = src
        .list(dir)
        .await?
        .into_iter()
        .filter(|e| !e.is_dir && !e.is_symlink)
        .filter(|e| {
            frame_of(&e.name).filter(|f| !f.is_helper()).is_some_and(|f| {
                runs.iter().any(|(p, r)| *r == f.run && (f.prefix == *p || (f.is_matrix() && p.eq_ignore_ascii_case("matrix"))))
            })
        })
        .map(|e| RemoteFile::new(format!("{dir}/{}", e.name), e.size))
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// `2024-05-21` from a unix time (UTC), for the "older than the frames" note.
fn day(t: u32) -> String {
    let days = i64::from(t / 86_400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

// ---------------------------------------------------------------------------------------
// The results: one dataset per .hkl, one cell for the sample (docs/BRUKER.md §4.2b)
// ---------------------------------------------------------------------------------------

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

fn stem_of(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// SAINT's per-run output ends in the run number (`3286AS264_B_02`); the merged file does not
/// (`3286AS264_B_0m`, `x_01m`, a name of your own).
fn is_run_file(stem: &str) -> bool {
    stem.rsplit_once('_').is_some_and(|(_, n)| n.len() == 2 && n.bytes().all(|b| b.is_ascii_digit()))
}

fn is_ini(e: &Entry) -> bool {
    !e.is_dir && (e.name.eq_ignore_ascii_case("saint.ini") || e.name.eq_ignore_ascii_case("smart.ini"))
}

/// XPREP's space-group line in its `.pcf`: `_symmetry_space_group_name_H-M   'C2/c'`.
pub fn pcf_space_group(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let l = line.trim();
        ["_symmetry_space_group_name_H-M", "_space_group_name_H-M_alt"].iter().find_map(|key| {
            let v = l.strip_prefix(key)?.trim().trim_matches(['\'', '"']).trim();
            (!v.is_empty() && v != "?").then(|| v.to_string())
        })
    })
}

/// XPREP names the space group in the title it writes: `TITL 3486S264_A in C2/c`.
pub fn title_space_group(title: &str) -> Option<String> {
    let sg = title.rsplit_once(" in ")?.1.trim();
    let first = sg.chars().next()?.to_ascii_uppercase();
    ("PABCIFRH".contains(first) && sg.len() <= 20 && !sg.contains(char::is_whitespace)).then(|| sg.to_string())
}

/// What the scan read in `work/`, so no file is read twice in one scan.
struct Reader<'a, S> {
    src: &'a S,
    work: &'a str,
    texts: HashMap<String, String>,
}

impl<S: Source> Reader<'_, S> {
    async fn text(&mut self, name: &str, limit: u64) -> Result<String> {
        if let Some(t) = self.texts.get(name) {
            return Ok(t.clone());
        }
        let bytes = self.src.read_prefix(&format!("{}/{name}", self.work), limit).await?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.texts.insert(name.to_string(), text.clone());
        Ok(text)
    }

    /// A program's listing, for the data quality: one that can't be read is left out, not an
    /// error of the scan.
    async fn listing(&mut self, name: &str, limit: u64) -> Option<String> {
        match self.text(name, limit).await {
            Ok(t) => Some(t),
            Err(_) => {
                // asked once per scan, even when two datasets share it
                self.texts.insert(name.to_string(), String::new());
                None
            }
        }
    }

    /// The outer error is the connection; the inner one why this file gives no cell.
    async fn cell(&mut self, name: &str) -> Result<std::result::Result<CellFile, String>> {
        let Some(kind) = file_kind(name) else {
            return Ok(Err(format!("{name} is not a cell file")));
        };
        let text = self.text(name, kind.read_limit()).await?;
        Ok(parse_cell_file(name, &text).map_err(|e| e.to_string()))
    }
}

/// The plain files of `work/` with this extension, newest first.
fn newest<'a>(listing: &'a [Entry], ext: &str) -> Vec<&'a Entry> {
    let mut v: Vec<&Entry> = listing
        .iter()
        .filter(|e| !e.is_dir && !e.is_symlink && nas::visible(&e.name) && ext_of(&e.name) == ext)
        .collect();
    v.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    v
}

fn named<'a>(listing: &'a [Entry], name: &str) -> Option<&'a Entry> {
    listing.iter().find(|e| !e.is_dir && !e.is_symlink && e.name.eq_ignore_ascii_case(name))
}

/// The cell of the whole sample: the highest-authority file wins and everything below it is
/// ignored: final (`.res`, then `.ins`) > integrated (SAINT's `_0m.p4p`, then `_0u`, …) >
/// matrix (APEX's exports) > CELL_NOW. Returns the cell, its file, and the files the read cap
/// left out.
async fn sample_cell<S: Source>(r: &mut Reader<'_, S>, listing: &[Entry]) -> Result<(Option<(CellFile, String)>, Vec<String>)> {
    for e in newest(listing, "res").into_iter().chain(newest(listing, "ins")) {
        if let Ok(c) = r.cell(&e.name).await? {
            return Ok((Some((c, e.name.clone())), Vec::new()));
        }
    }
    let rank = |e: &&Entry| {
        let l = e.name.to_ascii_lowercase();
        if l.ends_with("_0m.p4p") {
            0
        } else if l.ends_with("_0u.p4p") {
            1
        } else {
            2
        }
    };
    let mut p4p = newest(listing, "p4p");
    p4p.sort_by_key(rank);
    let mut budget = MAX_CELL_FILES;
    let mut skipped = Vec::new();
    // the newest of each lower tier, while looking for SAINT's cell
    let (mut matrix, mut other, mut cell_now) = (None, None, None);
    for e in p4p {
        if budget == 0 {
            skipped.push(e.name.clone());
            continue;
        }
        budget -= 1;
        let Ok(c) = r.cell(&e.name).await? else { continue };
        let slot = match c.source {
            CellSource::SaintP4p => return Ok((Some((c, e.name.clone())), skipped)),
            CellSource::Matrix => &mut matrix,
            CellSource::CellNow => &mut cell_now,
            CellSource::OtherP4p | CellSource::Shelx => &mut other,
        };
        if slot.is_none() {
            *slot = Some((c, e.name.clone()));
        }
    }
    if let Some(m) = matrix.or(other) {
        return Ok((Some(m), skipped));
    }
    // CELL_NOW's results get a cap of their own: a work/ full of spot lists (RDTtest: 32) must
    // not use up the reads before them
    let mut budget = MAX_CELL_FILES;
    for e in newest(listing, "p4p_cn") {
        if cell_now.is_some() {
            break;
        }
        if budget == 0 {
            skipped.push(e.name.clone());
            continue;
        }
        budget -= 1;
        if let Ok(c) = r.cell(&e.name).await? {
            cell_now = Some((c, e.name.clone()));
        }
    }
    Ok((cell_now, skipped))
}

/// Count, newest time and size of the files the results come from: a rescan of an unchanged
/// `work/` reads nothing.
fn work_stamp(listing: &[Entry], experiment: &[Entry]) -> String {
    let relevant = |e: &&Entry| {
        !e.is_dir
            && matches!(
                ext_of(&e.name).as_str(),
                "hkl" | "ins" | "res" | "pcf" | "p4p" | "p4p_cn" | "abs" | "_ls" | "prp" | "ini" | "raw" | "mul"
            )
    };
    let files: Vec<&Entry> = listing.iter().filter(relevant).chain(experiment.iter().filter(|e| is_ini(e))).collect();
    let newest = files.iter().filter_map(|e| e.mtime).max().unwrap_or(0);
    let bytes: u64 = files.iter().filter_map(|e| e.size).sum();
    // `q2`: what the scan reads and keeps (data quality since 2026-11-19); a new version rescans
    format!("q2:{}:{newest}:{bytes}", files.len())
}

/// The datasets of `work/` (one per `.hkl`, newest first, with the cell of its own files and the
/// files Olex2 needs next to it), then the sample's cell.
async fn scan_work<S: Source>(
    src: &S,
    dir: &str,
    work: &str,
    listing: &[Entry],
    experiment: &[Entry],
    first_frame: Option<u32>,
) -> Result<(Vec<Solve>, Option<String>)> {
    let mut r = Reader { src, work, texts: HashMap::new() };
    let file = |e: &Entry| RemoteFile::new(format!("{work}/{}", e.name), e.size);

    // the files every dataset brings along, for Olex2's CIF (it takes the newest of each kind)
    let pcf_any = newest(listing, "pcf").into_iter().next();
    let abs = newest(listing, "abs").into_iter().next();
    let integrated = newest(listing, "p4p").into_iter().find(|e| e.name.to_ascii_lowercase().ends_with("_0m.p4p"));
    let ls = {
        let all = newest(listing, "_ls");
        all.iter().find(|e| e.name.to_ascii_lowercase().ends_with("_0m._ls")).or(all.first()).copied()
    };
    let prp_0m = newest(listing, "prp").into_iter().find(|e| e.name.to_ascii_lowercase().ends_with("_0m.prp"));
    let mut inis: Vec<RemoteFile> = listing.iter().filter(|e| is_ini(e)).map(file).collect();
    inis.extend(
        experiment
            .iter()
            .filter(|e| is_ini(e) && !listing.iter().any(|w| w.name.eq_ignore_ascii_case(&e.name)))
            .map(|e| RemoteFile::new(format!("{dir}/{}", e.name), e.size)),
    );

    let mut solves = Vec::new();
    for (i, hkl) in newest(listing, "hkl").into_iter().enumerate() {
        let stem = stem_of(&hkl.name);
        let own = |ext: &str| named(listing, &format!("{stem}.{ext}"));
        let (res, ins, pcf, p4p) = (own("res"), own("ins"), own("pcf"), own("p4p"));
        let mut problems = Vec::new();
        let mut cell = None;
        for e in [res, ins, p4p].into_iter().flatten() {
            match r.cell(&e.name).await? {
                Ok(c) => {
                    cell = Some((c, e.name.clone()));
                    break;
                }
                Err(why) => problems.push(format!("{}: {why}", e.name)),
            }
        }
        let mut sg = None;
        if let Some(p) = pcf {
            sg = pcf_space_group(&r.text(&p.name, 4096).await?).map(|g| (g, p.name.clone()));
        }
        if sg.is_none()
            && let Some((c, from)) = &cell
        {
            sg = c.title.as_deref().and_then(title_space_group).map(|g| (g, from.clone()));
        }

        let kind = if res.is_some() || ins.is_some() { SolveKind::BrukerXprep } else { SolveKind::BrukerSadabs };
        let mut solve = Solve::new(kind, format!("{work}/{}", hkl.name), i as u32 + 1);
        solve.status = Status::Done;
        solve.hkl = [Some(hkl), ins, res].into_iter().flatten().map(file).collect();
        let mut logs: Vec<RemoteFile> = Vec::new();
        for e in [pcf.or(pcf_any), abs, integrated.or(p4p), ls, own("prp").or(prp_0m)].into_iter().flatten() {
            logs.push(file(e));
        }
        logs.extend(inis.iter().cloned());
        logs.retain(|f| !solve.hkl.iter().any(|h| h.path == f.path));
        logs.dedup_by(|a, b| a.path == b.path);
        solve.logs = logs;
        let mut note = Vec::new();
        match &cell {
            Some((c, from)) => {
                let mut st = c.stats();
                if let Some((g, _)) = &sg {
                    st.space_group = Some(g.clone());
                }
                solve.stats = Some(st);
                note.push(format!("cell from {from} ({})", c.source.label()));
            }
            None => note.push("no cell file for this data".into()),
        }
        if let Some((g, from)) = &sg {
            note.push(format!("space group {g} from {from}"));
        }
        note.extend(problems);
        solve.note = Some(note.join(" · "));
        // quality: XPREP's statistics (its .prp is named after the .hkl it read: the dataset's
        // own, else _0m's) and SADABS's error model
        if let Some(p) = own("prp").or(prp_0m)
            && let Some(text) = r.listing(&p.name, quality::LISTING_LIMIT).await
            && let Some(q) = quality::parse_prp(&p.name, &text)
        {
            let st = solve.stats.get_or_insert_with(Stats::default);
            (st.overall, st.inner, st.outer) = (q.overall, q.inner, q.outer);
            st.shell_table = q.table;
        }
        if let Some(a) = abs
            && let Some(text) = r.listing(&a.name, quality::LISTING_LIMIT).await
            && let Some(lines) = quality::parse_abs(&a.name, &text)
        {
            solve.stats.get_or_insert_with(Stats::default).scaling = lines;
        }
        solve.mtime = solve.hkl.iter().chain(&solve.logs).filter_map(|f| named(listing, f.name())).filter_map(|e| e.mtime).max();
        solves.push(solve);
    }

    // SAINT's integrated reflections, to run SADABS (the merged `*m.raw`, or every run's `.raw`) or
    // TWINABS (the `.mul` files) yourself (SADABS manual): listed too, scaled or not.
    let hkl_stems: Vec<String> = newest(listing, "hkl").iter().map(|e| stem_of(&e.name).to_ascii_lowercase()).collect();
    let saint: Vec<&Entry> = newest(listing, "raw").into_iter().chain(newest(listing, "mul")).collect();
    let per_run = |e: &&Entry| is_run_file(stem_of(&e.name));
    let mut groups: Vec<Vec<&Entry>> = saint
        .iter()
        .filter(|e| !per_run(e) && !stem_of(&e.name).to_ascii_lowercase().ends_with("_uf"))
        .map(|e| vec![*e])
        .collect();
    if groups.is_empty() {
        // no merged file (one run, or merging switched off): every run's file together
        let mut runs: Vec<&Entry> = saint.iter().copied().filter(|e| per_run(e)).collect();
        runs.sort_by(|a, b| a.name.cmp(&b.name));
        if !runs.is_empty() {
            groups.push(runs);
        }
    }
    for files in groups {
        let first = files[0];
        let stem = stem_of(&first.name);
        let own = |ext: &str| named(listing, &format!("{stem}.{ext}"));
        // the cell SAINT refined for these reflections: the p4p of the same name, else _0m's
        let p4p = own("p4p").or(integrated);
        let mut solve = Solve::new(SolveKind::BrukerSaint, format!("{work}/{}", first.name), solves.len() as u32 + 1);
        solve.status = Status::Done;
        solve.hkl = files.iter().map(|e| file(e)).collect();
        let mut logs: Vec<RemoteFile> = Vec::new();
        for e in [p4p, named(listing, &format!("{stem}_a.p4p")), own("_ls").or(ls), abs].into_iter().flatten() {
            logs.push(file(e));
        }
        logs.extend(inis.iter().cloned());
        logs.dedup_by(|a, b| a.path == b.path);
        solve.logs = logs;
        let twin = ext_of(&first.name) == "mul";
        let program = if twin { "TWINABS" } else { "SADABS" };
        let what = if files.len() > 1 { "these runs' files" } else { "this file" };
        let mut note = vec![if hkl_stems.contains(&stem.to_ascii_lowercase()) {
            format!("SAINT's integrated reflections, already scaled into {stem}.hkl; {program} reads {what}")
        } else {
            format!("integrated, not scaled: run {program} on {what}")
        }];
        if let Some(e) = p4p {
            match r.cell(&e.name).await? {
                Ok(c) => {
                    solve.stats = Some(c.stats());
                    note.push(format!("cell from {} ({})", e.name, c.source.label()));
                }
                Err(why) => note.push(format!("{}: {why}", e.name)),
            }
        }
        // quality: SAINT's merged listing (a per-run one has its tables at the end, up to 1 MB in:
        // not read)
        if let Some(e) = own("_ls").or(ls).filter(|e| !is_run_file(stem_of(&e.name)))
            && let Some(text) = r.listing(&e.name, quality::SAINT_LS_LIMIT).await
            && let Some(q) = quality::parse_saint_ls(&e.name, &text)
        {
            let st = solve.stats.get_or_insert_with(Stats::default);
            (st.overall, st.inner, st.outer) = (q.overall, q.inner, q.outer);
            (st.shell_table, st.scaling) = (q.table, q.summary);
        }
        solve.note = Some(note.join(" · "));
        solve.mtime = solve.hkl.iter().chain(&solve.logs).filter_map(|f| named(listing, f.name())).filter_map(|e| e.mtime).max();
        solves.push(solve);
    }

    let (cell, skipped) = sample_cell(&mut r, listing).await?;
    if let Some((c, from)) = cell {
        let entry = named(listing, &from);
        let mut solve = Solve::new(SolveKind::BrukerCell, format!("{work}/{from}"), 1);
        solve.status = Status::Done;
        let mut st = c.stats();
        // a final cell takes XPREP's space group, like its dataset
        if c.source == CellSource::Shelx {
            let pcf = named(listing, &format!("{}.pcf", stem_of(&from)));
            let from_pcf = match pcf {
                Some(p) => pcf_space_group(&r.text(&p.name, 4096).await?),
                None => None,
            };
            if let Some(g) = from_pcf.or_else(|| c.title.as_deref().and_then(title_space_group)) {
                st.space_group = Some(g);
            }
        }
        solve.stats = Some(st);
        let mut note = format!("{}: {from} · {}", c.source.label(), c.describe());
        if let (Some(t), Some(first)) = (entry.and_then(|e| e.mtime), first_frame)
            && t < first
        {
            note.push_str(&format!(" · dated {}, before the frames ({})", day(t), day(first)));
        }
        solve.note = Some(note);
        solve.mtime = entry.and_then(|e| e.mtime);
        solves.push(solve);
    }
    let note = (!skipped.is_empty())
        .then(|| format!("{} older cell file(s) not read: {}", skipped.len(), skipped.join(", ")));
    // shown with the cell, the row it concerns
    if let (Some(n), Some(cell)) = (&note, solves.last_mut().filter(|s| s.kind == SolveKind::BrukerCell)) {
        let text = cell.note.take().unwrap_or_default();
        cell.note = Some(format!("{text} · {n}"));
    }
    Ok((solves, note))
}

/// One experiment: 1 listing for its frames, 1 for `work/`, and bounded reads of the files that
/// give the cells (none at all when `work/` is unchanged since `known`). The server is only
/// listed and read.
async fn scan_dir<S: Source>(
    src: &S,
    dir: &str,
    name: &str,
    known: Option<&SampleInfo>,
    mut progress: impl FnMut(&SampleInfo),
) -> Result<SampleInfo> {
    let entries = src.list(dir).await?;
    let frame_runs = group_frames(&entries);
    let mut info = SampleInfo { name: name.to_string(), runs: Vec::new(), scanned_at: now_secs() };
    for r in &frame_runs {
        info.runs.push(Run {
            prefix: r.prefix.clone(),
            run: r.run,
            characterisation: false,
            solves: Vec::new(),
            kind: if r.matrix { RunKind::Matrix } else { RunKind::Data },
            frames: Some(FrameSet { first: r.first, last: r.last, count: r.count, bytes: r.bytes }),
            note: None,
            stamp: None,
        });
    }
    progress(&info);

    let first_frame = frame_runs.iter().filter_map(|r| r.oldest).min();
    let work_name = entries
        .iter()
        .find(|e| e.is_dir && !e.is_symlink && e.name.eq_ignore_ascii_case(WORK_DIR))
        .map(|e| e.name.clone());
    if let Some(work_name) = work_name {
        let work = format!("{dir}/{work_name}");
        let listing = src.list(&work).await?;
        let stamp = work_stamp(&listing, &entries);
        let reused = known
            .and_then(|k| k.runs.iter().find(|r| r.kind == RunKind::Work))
            .filter(|r| r.stamp.as_deref() == Some(stamp.as_str()))
            .cloned();
        let run = match reused {
            Some(r) => Some(r),
            None => {
                let (solves, note) = scan_work(src, dir, &work, &listing, &entries, first_frame).await?;
                (!solves.is_empty() || note.is_some()).then(|| Run {
                    prefix: WORK_DIR.to_string(),
                    run: 0,
                    characterisation: false,
                    solves,
                    kind: RunKind::Work,
                    frames: None,
                    note,
                    stamp: Some(stamp),
                })
            }
        };
        info.runs.extend(run);
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAINT_P4P: &str = include_str!("../../docs/server_samples/bruker_saint.p4p");
    const CELLNOW_INPUT: &str = include_str!("../../docs/server_samples/bruker_cellnow_input.p4p");
    const CELLNOW_OUTPUT: &str = include_str!("../../docs/server_samples/bruker_cellnow_output.p4p_cn");
    const XPREP_INS: &str = include_str!("../../docs/server_samples/bruker_xprep.ins");
    const APEX2_EXPORT: &str = include_str!("../../docs/server_samples/bruker_apex2_export.p4p");
    const SEATTLE_NAC: &str = include_str!("../../docs/server_samples/bruker_seattle_nac.p4p");

    fn file(name: &str, size: u64, mtime: Option<u32>) -> Entry {
        Entry { name: name.into(), is_dir: false, is_symlink: false, size: Some(size), mtime }
    }

    fn dir(name: &str) -> Entry {
        Entry { name: name.into(), is_dir: true, is_symlink: false, size: None, mtime: None }
    }

    #[test]
    fn frame_names_split_from_the_right() {
        let f = |n: &str| frame_of(n).map(|f| (f.prefix, f.run, f.frame));
        assert_eq!(f("3286AS264_B_01_0001.sfrm"), Some(("3286AS264_B".into(), 1, 1)));
        assert_eq!(f("matrix_02_0012.sfrm"), Some(("matrix".into(), 2, 12)));
        // digits and underscores in the prefix stay in the prefix
        assert_eq!(f("sample_2_01_0001.sfrm"), Some(("sample_2".into(), 1, 1)));
        assert_eq!(f("JSG285b_a_12_0360.sfrm"), Some(("JSG285b_a".into(), 12, 360)));
        // wide run and frame numbers, upper-case extension
        assert_eq!(f("x_123_00001.SFRM"), Some(("x".into(), 123, 1)));
        // not frames
        for n in [
            "x_01_0001.sfr",
            "x_01_0001.p4p",
            "x_0001.sfrm",
            "_01_0001.sfrm",
            "x_01_01.sfrm",
            "x_1234_0001.sfrm",
            "x_ab_0001.sfrm",
            "x_01_000a.sfrm",
            "x.sfrm",
            "sfrm",
        ] {
            assert_eq!(f(n), None, "{n}");
        }
        assert!(frame_of("MATRIX_01_0001.sfrm").unwrap().is_matrix());
        for n in ["3570JSG276_am_01_0001.sfrm", "3570JSG276_ib_02_0001.sfrm", "x_le_01_0001.sfrm", "x_xa_01_0001.sfrm", "bg_snap_01_0001.sfrm"] {
            assert!(frame_of(n).unwrap().is_helper(), "{n}");
        }
        assert!(!frame_of("3570JSG276_01_0001.sfrm").unwrap().is_helper());
        assert!(!frame_of("ham_01_0001.sfrm").unwrap().is_helper(), "a name ending in am is a name");
        assert!(!frame_of("matrix2_01_0001.sfrm").unwrap().is_matrix());
    }

    #[test]
    fn frames_group_into_runs_and_matrix_runs() {
        let mut v = vec![dir("work"), dir("matrix_09_0001.sfrm"), file("notes.txt", 5, None), file("x_01_0001.p4p", 5, None)];
        for n in 1..=3 {
            v.push(file(&format!("JSG1_01_{n:04}.sfrm"), 270_000, Some(100 + n)));
        }
        // run 2 is missing its frame 2
        v.push(file("JSG1_02_0001.sfrm", 270_000, Some(200)));
        v.push(file("JSG1_02_0003.sfrm", 270_001, Some(210)));
        for run in 1..=2 {
            v.push(file(&format!("matrix_{run:02}_0001.sfrm"), 1000, None));
        }
        v.push(file("Matrix_01_0002.sfrm", 1000, None));
        // SAINT's mask and background frames left next to the data are no runs
        v.push(file("JSG1_am_01_0001.sfrm", 269_824, None));
        v.push(file("JSG1_ib_01_0001.sfrm", 273_696, None));
        let runs = group_frames(&v);
        let summary: Vec<_> = runs.iter().map(|r| (r.prefix.as_str(), r.run, r.matrix, r.count, r.first, r.last, r.bytes)).collect();
        assert_eq!(
            summary,
            vec![
                ("JSG1", 1, false, 3, 1, 3, 810_000),
                ("JSG1", 2, false, 2, 1, 3, 540_001),
                ("matrix", 1, true, 2, 1, 2, 2000),
                ("matrix", 2, true, 1, 1, 1, 1000),
            ]
        );
        assert!(!runs[0].has_gaps() && runs[1].has_gaps());
        assert_eq!((runs[0].oldest, runs[0].newest), (Some(101), Some(103)));
        assert_eq!(runs[2].oldest, None);
        assert!(has_work(&v));
        assert!(!has_work(&[file("work", 1, None), dir("worker")]));
    }

    #[test]
    fn cell_files_are_found_by_extension_not_by_name() {
        let k = |n: &str| file_kind(n);
        assert_eq!(k("3286AS264_B_0m.p4p"), Some(FileKind::P4p));
        assert_eq!(k("0egsl6.p4p_cn"), Some(FileKind::P4pCn));
        assert_eq!(k("3486S264_A.ins"), Some(FileKind::Shelx));
        assert_eq!(k("3486S264_A.RES"), Some(FileKind::Shelx));
        assert_eq!(k("3286AS264_B_0u.p4p"), Some(FileKind::P4p));
        assert_eq!(k("X_0M.P4P"), Some(FileKind::P4p));
        // any .p4p is read: APEX's exports have random names, CELL_NOW's spot lists too
        for n in ["5f9dbq.p4p", "nac.p4p", "3286AS264_B.p4p", "cell.p4p", "0egsl6.p4p"] {
            assert_eq!(k(n), Some(FileKind::P4p), "{n}");
        }
        // never read: CELL_NOW's text log, SAINT's other files, reflections
        for n in ["3286AS264_B_0m.p4p.bak", "3286AS264_B_01._ls", "0egsl6._cn", "x_02._ma", "x_02.raw", "x_0m.hkl", "x_0m.prp", "x.abs", "x_01_0001.sfrm", "noextension"] {
            assert_eq!(k(n), None, "{n}");
        }
    }

    #[test]
    fn only_the_newest_cell_files_are_read_and_the_rest_are_named() {
        let mut v = vec![dir("sub.p4p"), file("a.hkl", 1, Some(999))];
        for i in 0..12u32 {
            v.push(file(&format!("f{i:02}.p4p_cn"), 3000, Some(1000 + i)));
        }
        v.push(file("undated.ins", 300, None));
        v.push(Entry { is_symlink: true, ..file("link.p4p", 1, Some(5000)) });
        v.push(file(".hidden.p4p", 1, Some(6000)));
        let c = select_candidates(&v, 10);
        assert_eq!(c.read.len(), 10);
        assert_eq!(c.read[0], ("f11.p4p_cn".to_string(), FileKind::P4pCn));
        assert_eq!(c.read[9].0, "f02.p4p_cn");
        // the oldest dated ones, then the undated, are left out
        assert_eq!(c.skipped, vec!["f01.p4p_cn", "f00.p4p_cn", "undated.ins"]);
        assert_eq!(select_candidates(&v, 100).skipped, Vec::<String>::new());
        assert_eq!(select_candidates(&[], 10), Candidates::default());
    }

    #[test]
    fn saint_p4p_gives_the_refined_cell_with_its_centring() {
        let c = parse_cell_file("3286AS264_B_0m.p4p", SAINT_P4P).unwrap();
        assert_eq!(c.source, CellSource::SaintP4p);
        assert_eq!(c.cell, [18.2973, 18.2927, 20.5019, 110.0182, 110.0735, 97.7397]);
        assert_eq!(c.esd, Some([0.0075, 0.0069, 0.0084, 0.0068, 0.0064, 0.0070]));
        assert_eq!((c.centring, c.bravais.as_deref()), (Some('P'), Some("Triclinic P")));
        assert_eq!(c.program.as_deref(), Some("SAINT V8.34A"));
        assert_eq!(c.date.as_deref(), Some("2025-07-25 09:06:55"));
        assert_eq!(c.wavelength, Some(0.71073));
        assert_eq!(c.title.as_deref(), Some("Integration of 3286AS264_B"));
        let st = c.stats();
        assert_eq!(st.space_group.as_deref(), Some("P (centring only)"));
        assert_eq!(crate::cells::centring(st.space_group.as_deref().unwrap()), 'P');
        assert!(c.describe().starts_with("integrated · SAINT V8.34A · 2025-07-25 09:06:55 · λ 0.71073 · Integration of 3286AS264_B · centring P"));
        // Windows line ends change nothing
        assert_eq!(parse_p4p(&SAINT_P4P.replace('\n', "\r\n")).unwrap(), parse_p4p(SAINT_P4P).unwrap());
    }

    #[test]
    fn cell_now_output_is_a_cell_and_its_input_is_not() {
        let c = parse_cell_file("0egsl6.p4p_cn", CELLNOW_OUTPUT).unwrap();
        assert_eq!(c.source, CellSource::CellNow);
        assert_eq!(c.cell, [9.8380, 4.6617, 11.9019, 90.0159, 96.8362, 90.1710]);
        assert_eq!(c.esd.map(|e| e[0]), Some(0.0020));
        // no BRAVAIS line: the centring is unknown, so the cell is searched as primitive
        assert_eq!((c.centring, c.space_group()), (None, None));
        assert_eq!(c.date.as_deref(), Some("2024-05-21 10:52:29"));
        assert!(c.describe().contains("centring unknown"));

        // the matching .p4p is the spot list CELL_NOW reads, with a placeholder cell
        let err = parse_p4p(CELLNOW_INPUT).unwrap_err();
        assert!(err.is::<NotACell>() && err.to_string().contains("spot list"), "{err}");
        // such a file is read like any .p4p, and gives no row
        assert_eq!(file_kind("0egsl6.p4p"), Some(FileKind::P4p));
    }

    #[test]
    fn placeholder_and_impossible_cells_give_no_row() {
        let with_cell = |line: &str| {
            let text: Vec<String> = SAINT_P4P
                .lines()
                .map(|l| if l.starts_with("CELL ") { line.to_string() } else { l.to_string() })
                .collect();
            parse_p4p(&text.join("\n"))
        };
        let placeholder = with_cell("CELL 10.0000 10.0000 10.0000 90.0000 90.0000 90.0000 1000.000").unwrap_err().to_string();
        assert!(placeholder.contains("placeholder"), "{placeholder}");
        assert!(with_cell("CELL 10 20 30 1 1 179 1").is_err());
        assert!(with_cell("CELL 0 20 30 90 90 90 1").is_err());
        assert!(with_cell("CELL 10 20 30 90 90").is_err());
        assert!(with_cell("CELL").is_err());
        let no_cell: String = SAINT_P4P.lines().filter(|l| !l.starts_with("CELL")).collect::<Vec<_>>().join("\n");
        assert!(parse_p4p(&no_cell).unwrap_err().to_string().contains("no CELL"));
        assert!(parse_p4p("").unwrap_err().is::<NotACell>(), "an empty .p4p is no row, not a failure");
        assert!(parse_p4p("\r\n \n").unwrap_err().is::<NotACell>());
        // a BRAVAIS line with a letter that is no centring is an error, never a guess
        let bad = SAINT_P4P.replace("Triclinic             P", "Triclinic             Q");
        assert!(parse_p4p(&bad).unwrap_err().to_string().contains("BRAVAIS"));
        // 2 KiB cut anywhere before the reflections still reads (the header is all we need)
        assert!(parse_p4p(&SAINT_P4P[..SAINT_P4P.find("SAINOV").unwrap()]).is_ok());
    }

    #[test]
    fn apex_exports_are_the_matrix_cell_with_its_bravais_lattice() {
        // a random-named export next to the integration (1743_rdc204etoh on the NAS)
        let c = parse_cell_file("ezu8f7.p4p", APEX2_EXPORT).unwrap();
        assert_eq!(c.source, CellSource::Matrix);
        assert_eq!(c.cell, [12.3946, 12.3515, 48.9538, 90.0, 91.3092, 90.0]);
        assert_eq!((c.centring, c.bravais.as_deref()), (Some('C'), Some("Monoclinic(b-unique) C")));
        assert_eq!((c.program.as_deref(), c.date.as_deref()), (Some("APEX2 2011.8-0"), Some("2019-06-04 10:56:26")));
        assert_eq!(c.stats().space_group.as_deref(), Some("C (centring only)"));
        assert!(c.describe().starts_with("matrix · APEX2 2011.8-0"));
        // other APEX versions too
        let apex4 = APEX2_EXPORT.replace("FILEID APEX2        2011.8-0", "FILEID APEX4        2021.10-0");
        assert_eq!(parse_p4p(&apex4).unwrap().source, CellSource::Matrix);
    }

    #[test]
    fn seattle_files_are_matrix_cells_and_name_their_other_components() {
        let c = parse_cell_file("nac.p4p", SEATTLE_NAC).unwrap();
        assert_eq!(c.source, CellSource::Matrix);
        // the first component is the row; CELL2 is only mentioned
        assert_eq!(c.cell, [19.98, 22.8913, 27.3187, 90.0, 100.5207, 90.0]);
        assert_eq!(c.centring, Some('C'));
        assert_eq!(c.title.as_deref(), Some("AZO1 + Mn(2+) Sara Barbero TFG; 1 more component(s) in the file"));
    }

    #[test]
    fn a_cell_now_export_with_a_real_cell_is_a_matrix_cell() {
        // exported after the cell was found (3570JSG276.p4p on this PC): the cell is real
        let found = CELLNOW_INPUT.replace(
            "CELL     10.0000   10.0000   10.0000   90.0000   90.0000   90.0000   1000.000",
            "CELL     13.4209   24.9248   30.5151   91.7505   96.2141  138.7799   6588.508",
        );
        assert_ne!(found, CELLNOW_INPUT, "the fixture has the placeholder line");
        let c = parse_p4p(&found).unwrap();
        assert_eq!((c.source, c.cell[0]), (CellSource::Matrix, 13.4209));
        // exported before: the placeholder, no row
        let e = parse_p4p(CELLNOW_INPUT).unwrap_err();
        assert!(e.is::<NotACell>() && e.to_string().contains("before any cell was found"), "{e}");
    }

    #[test]
    fn other_programs_p4p_is_accepted_under_its_own_name() {
        let text = SAINT_P4P.replace("FILEID SAINT        V8.34A", "FILEID XPREP        ?");
        let c = parse_p4p(&text).unwrap();
        assert_eq!((c.source, c.program.as_deref()), (CellSource::OtherP4p, Some("XPREP")));
    }

    #[test]
    fn shelx_ins_gives_the_final_cell_and_the_centring() {
        let c = parse_cell_file("3486S264_A.ins", XPREP_INS).unwrap();
        assert_eq!(c.source, CellSource::Shelx);
        assert_eq!(c.cell, [24.06787, 27.56022, 20.50190, 90.0, 121.4068, 90.0]);
        assert_eq!(c.centring, Some('C'));
        assert_eq!(c.wavelength, Some(0.71073));
        assert_eq!(c.title.as_deref(), Some("3486S264_A in C2/c"));
        assert_eq!(c.stats().space_group.as_deref(), Some("C (centring only)"));
        // no LATT: SHELX's default would be wrong for a centred cell, so no cell
        assert!(parse_cell_file("x.ins", "CELL 0.71 10 20 30 90 90 90\nEND").is_err());
    }

    /// A local copy of a `work/` folder, standing in for the server.
    struct LocalDir(std::path::PathBuf);

    impl Source for LocalDir {
        async fn list(&self, path: &str) -> Result<Vec<Entry>> {
            let dir = if path.ends_with("/work") { self.0.clone() } else { self.0.parent().unwrap().to_path_buf() };
            let mut out = Vec::new();
            for e in std::fs::read_dir(dir)? {
                let e = e?;
                let meta = e.metadata()?;
                let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
                out.push(Entry {
                    name: e.file_name().to_string_lossy().into(),
                    is_dir: meta.is_dir(),
                    is_symlink: false,
                    size: Some(meta.len()),
                    mtime: mtime.map(|d| d.as_secs() as u32),
                });
            }
            Ok(out)
        }

        async fn read_prefix(&self, path: &str, max: u64) -> Result<Vec<u8>> {
            use std::io::Read;
            let mut buf = Vec::new();
            std::fs::File::open(self.0.join(path.rsplit('/').next().unwrap()))?.take(max).read_to_end(&mut buf)?;
            Ok(buf)
        }
    }

    /// Opt-in check on a real `work/` folder copied to disk (its parent folder is read as the
    /// experiment folder, so frames next to it count too):
    /// `BRUKER_WORK_DIR=C:/x/exp/work cargo test bruker_work_dir -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "needs BRUKER_WORK_DIR"]
    async fn bruker_work_dir() {
        let dir = std::path::PathBuf::from(std::env::var("BRUKER_WORK_DIR").expect("set BRUKER_WORK_DIR"));
        let info = scan_dir(&LocalDir(dir), "/exp", "exp", None, |_| {}).await.unwrap();
        for r in &info.runs {
            println!("{:<8} {:<12} run {} frames {:?} {}", format!("{:?}", r.kind), r.prefix, r.run, r.frames.map(|f| f.count), r.note.as_deref().unwrap_or(""));
            for s in &r.solves {
                let c = s.stats.as_ref().and_then(|st| st.cell);
                println!("   {:<11} {:?} {:?} files {:?}", s.kind.label(), s.status, c, s.hkl.iter().map(|f| f.name()).collect::<Vec<_>>());
                for l in s.stats.iter().flat_map(|st| st.shell_table.iter().chain(&st.scaling)) {
                    println!("      | {l}");
                }
            }
        }
    }

    #[test]
    fn sources_are_listed_in_a_fixed_order() {
        use CellSource::*;
        let mut v = vec![CellNow, OtherP4p, Shelx, SaintP4p, Matrix];
        v.sort();
        assert_eq!(v, vec![Matrix, SaintP4p, Shelx, OtherP4p, CellNow]);
        let labels: Vec<_> = v.iter().map(|s| s.label()).collect();
        assert_eq!(labels, ["matrix", "integrated", "final", "p4p", "CELL_NOW"]);
    }

    #[test]
    fn dates_read_two_and_four_digit_years() {
        assert_eq!(us_date("07/25/25", Some("09:06:55")).as_deref(), Some("2025-07-25 09:06:55"));
        assert_eq!(us_date("07/25/2025", None).as_deref(), Some("2025-07-25"));
        assert_eq!(us_date("13/25/25", None), None);
        assert_eq!(us_date("07/25", None), None);
        assert_eq!(us_date("07/25/25/1", None), None);
    }

    // ---- scanning an experiment, against an in-memory server ----

    use std::sync::Mutex;

    const XPREP_PCF: &str = include_str!("../../docs/server_samples/bruker_xprep.pcf");
    const XPREP_PRP: &str = include_str!("../../docs/server_samples/bruker_xprep.prp");
    const SADABS_ABS: &str = include_str!("../../docs/server_samples/bruker_sadabs.abs");
    const SAINT_LS: &str = include_str!("../../docs/server_samples/bruker_saint_0m_ls_excerpt.txt");

    #[derive(Default)]
    struct Fake {
        dirs: HashMap<String, Vec<Entry>>,
        files: HashMap<String, String>,
        lists: Mutex<Vec<String>>,
        reads: Mutex<Vec<String>>,
    }

    impl Fake {
        fn dir(mut self, path: &str, entries: Vec<Entry>) -> Self {
            self.dirs.insert(path.into(), entries);
            self
        }

        fn file(mut self, path: &str, text: &str) -> Self {
            self.files.insert(path.into(), text.into());
            self
        }

        fn requests(&self) -> (usize, usize) {
            (self.lists.lock().unwrap().len(), self.reads.lock().unwrap().len())
        }

        fn read_names(&self) -> Vec<String> {
            self.reads.lock().unwrap().iter().map(|p| p.rsplit('/').next().unwrap().to_string()).collect()
        }
    }

    impl Source for Fake {
        async fn list(&self, path: &str) -> Result<Vec<Entry>> {
            self.lists.lock().unwrap().push(path.to_string());
            self.dirs.get(path).cloned().ok_or_else(|| anyhow::anyhow!("no such folder {path}"))
        }

        async fn read_prefix(&self, path: &str, max: u64) -> Result<Vec<u8>> {
            self.reads.lock().unwrap().push(path.to_string());
            let text = self.files.get(path).ok_or_else(|| anyhow::anyhow!("no such file {path}"))?;
            Ok(text.as_bytes()[..text.len().min(max as usize)].to_vec())
        }
    }

    const E: &str = "/guest/3286AS264_B";

    /// Frames at t = 1500: two data runs and one matrix run (t = 1400), a work/ folder.
    fn frames_listing() -> Vec<Entry> {
        let mut v = vec![dir("work"), dir("gallery"), file("saint.ini", 3758, Some(1400))];
        for run in [1, 2] {
            for n in 1..=3 {
                v.push(file(&format!("3286AS264_B_{run:02}_{n:04}.sfrm"), 270_000, Some(1500)));
            }
        }
        for n in 1..=2 {
            v.push(file(&format!("matrix_01_{n:04}.sfrm"), 270_000, Some(1400)));
        }
        v
    }

    /// The real processed folder: XPREP's output (`3486S264_A`), SADABS's `_0m.hkl` with SAINT's
    /// `_0m`/`_0u` p4p, APEX's export, CELL_NOW's files, logs.
    fn processed() -> Fake {
        let w = |n: &str| format!("{E}/work/{n}");
        Fake::default()
            .dir(E, frames_listing())
            .dir(
                w("").trim_end_matches('/'),
                vec![
                    file("3486S264_A.hkl", 392_280, Some(2003)),
                    file("3486S264_A.ins", 270, Some(2002)),
                    file("3486S264_A.res", 6856, Some(2004)),
                    file("3486S264_A.pcf", 1417, Some(2002)),
                    file("3486S264_A.lst", 77_295, Some(2004)),
                    file("3286AS264_B_0m.hkl", 463_352, Some(1990)),
                    file("3286AS264_B_0m.p4p", 160_127, Some(1990)),
                    file("3286AS264_B_0u.p4p", 160_127, Some(1991)),
                    file("3286AS264_B_0m.prp", 7229, Some(1992)),
                    file("3286AS264_B.abs", 16_674, Some(1995)),
                    file("3286AS264_B_01._ls", 74_849, Some(1980)),
                    file("3286AS264_B_02.raw", 3_503_167, Some(1985)),
                    file("ezu8f7.p4p", 117_211, Some(1450)),
                    file("0egsl6.p4p", 3800, Some(1000)),
                    file("0egsl6.p4p_cn", 3600, Some(1000)),
                ],
            )
            .file(&w("3486S264_A.ins"), XPREP_INS)
            .file(&w("3486S264_A.res"), XPREP_INS)
            .file(&w("3486S264_A.pcf"), XPREP_PCF)
            .file(&w("3286AS264_B_0m.p4p"), SAINT_P4P)
            .file(&w("3286AS264_B_0u.p4p"), SAINT_P4P)
            .file(&w("3286AS264_B_0m.prp"), XPREP_PRP)
            .file(&w("3286AS264_B.abs"), SADABS_ABS)
            .file(&w("ezu8f7.p4p"), APEX2_EXPORT)
            .file(&w("0egsl6.p4p"), CELLNOW_INPUT)
            .file(&w("0egsl6.p4p_cn"), CELLNOW_OUTPUT)
    }

    fn work_run(info: &SampleInfo) -> &Run {
        info.runs.iter().find(|r| r.kind == RunKind::Work).expect("a work run")
    }

    fn names(files: &[RemoteFile]) -> Vec<&str> {
        files.iter().map(RemoteFile::name).collect()
    }

    #[tokio::test]
    async fn every_hkl_is_a_dataset_with_its_own_cell_and_the_files_olex2_wants() {
        let src = processed();
        let mut progress = Vec::new();
        let info = scan_dir(&src, E, "3286AS264_B", None, |i| progress.push(i.runs.len())).await.unwrap();
        let runs: Vec<_> = info.runs.iter().map(|r| (r.prefix.as_str(), r.run, r.kind)).collect();
        assert_eq!(
            runs,
            vec![
                ("3286AS264_B", 1, RunKind::Data),
                ("3286AS264_B", 2, RunKind::Data),
                ("matrix", 1, RunKind::Matrix),
                ("work", 0, RunKind::Work),
            ]
        );
        assert_eq!(progress, vec![3], "the frame runs show up before work/ is read");

        let w = work_run(&info);
        let kinds: Vec<_> = w.solves.iter().map(|s| (s.kind, s.n)).collect();
        assert_eq!(
            kinds,
            vec![(SolveKind::BrukerXprep, 1), (SolveKind::BrukerSadabs, 2), (SolveKind::BrukerSaint, 3), (SolveKind::BrukerCell, 1)]
        );
        // SAINT's reflections (only one run's .raw is left here): listed too, for SADABS
        assert_eq!(names(&w.solves[2].hkl), ["3286AS264_B_02.raw"]);

        // XPREP's data: refined with its .ins/.res, cell from the .res, space group from the .pcf
        let x = &w.solves[0];
        assert!(x.dir.ends_with("/work/3486S264_A.hkl"));
        assert_eq!(names(&x.hkl), ["3486S264_A.hkl", "3486S264_A.ins", "3486S264_A.res"]);
        assert_eq!(
            names(&x.logs),
            ["3486S264_A.pcf", "3286AS264_B.abs", "3286AS264_B_0m.p4p", "3286AS264_B_01._ls", "3286AS264_B_0m.prp", "saint.ini"]
        );
        assert_eq!(x.logs.last().unwrap().path, format!("{E}/saint.ini"), "saint.ini of the experiment folder");
        let st = x.stats.as_ref().unwrap();
        assert_eq!((st.cell.unwrap()[0], st.space_group.as_deref()), (24.06787, Some("C2/c")));
        let note = x.note.as_deref().unwrap();
        assert!(note.contains("cell from 3486S264_A.res (final)") && note.contains("space group C2/c from 3486S264_A.pcf"), "{note}");
        assert_eq!(x.mtime, Some(2004));

        // SADABS's data: its cell is SAINT's _0m.p4p (another setting), centring only
        let s = &w.solves[1];
        assert_eq!(names(&s.hkl), ["3286AS264_B_0m.hkl"]);
        let st = s.stats.as_ref().unwrap();
        assert_eq!((st.cell.unwrap()[0], st.space_group.as_deref()), (18.2973, Some("P (centring only)")));
        assert!(s.note.as_deref().unwrap().starts_with("cell from 3286AS264_B_0m.p4p (integrated)"));
        assert_eq!(names(&s.logs)[0], "3486S264_A.pcf", "no .pcf of its own: the newest one");

        // the sample's one cell: the highest authority, the .res, with XPREP's space group
        let c = &w.solves[3];
        assert!(c.hkl.is_empty() && c.logs.is_empty(), "the cell row downloads nothing");
        let st = c.stats.as_ref().unwrap();
        assert_eq!((st.cell.unwrap(), st.space_group.as_deref()), ([24.06787, 27.56022, 20.5019, 90.0, 121.4068, 90.0], Some("C2/c")));
        assert!(c.note.as_deref().unwrap().starts_with("final: 3486S264_A.res"));
        assert!(crate::rank::best(&info).is_none(), "nothing is ranked");
        assert_eq!(crate::cells::sample_cell(&info).map(|(s, _)| s.kind), Some(SolveKind::BrukerCell));

        // the quality: XPREP's statistics (its _0m.prp) and SADABS's error model, for both datasets
        for d in [x, s] {
            let st = d.stats.as_ref().unwrap();
            assert_eq!(st.shell_table[0], "XPREP (3286AS264_B_0m.prp), merged in C2/c:");
            assert!(st.scaling[0].starts_with("SADABS-2008/1 (3286AS264_B.abs): g = 0.0636"), "{:?}", st.scaling);
            assert_eq!(st.overall.as_ref().unwrap().completeness, Some(94.9));
        }
        // SAINT's only listing here is a run's (its tables are at the end, up to 1 MB in): not read
        let st = w.solves[2].stats.as_ref().unwrap();
        assert!(st.shell_table.is_empty() && st.scaling.is_empty());

        // two listings, and only five reads: the .res, the .pcf, the .prp, the .abs and the
        // _0m.p4p (the sample's cell reuses the .res; APEX's export, CELL_NOW's files, the .ins
        // and the run's ._ls are never read)
        assert_eq!(src.requests().0, 2);
        assert_eq!(
            src.read_names(),
            ["3486S264_A.res", "3486S264_A.pcf", "3286AS264_B_0m.prp", "3286AS264_B.abs", "3286AS264_B_0m.p4p"]
        );
        assert!(info.has_data() && !info.unprocessed() && !info.test_shots_only());
    }

    #[tokio::test]
    async fn an_unchanged_work_folder_is_not_read_again() {
        let src = processed();
        let first = scan_dir(&src, E, "3286AS264_B", None, |_| {}).await.unwrap();
        let reads = src.requests().1;
        let again = scan_dir(&src, E, "3286AS264_B", Some(&first), |_| {}).await.unwrap();
        assert_eq!(src.requests().1, reads, "same files: no read at all");
        assert_eq!(work_run(&again), work_run(&first));

        // a new .res (refined again) is a change: everything is read again
        let mut touched = processed();
        touched.dirs.get_mut(&format!("{E}/work")).unwrap()[2].mtime = Some(2100);
        let third = scan_dir(&touched, E, "3286AS264_B", Some(&first), |_| {}).await.unwrap();
        assert_eq!(touched.requests().1, 5);
        assert_ne!(work_run(&third).stamp, work_run(&first).stamp);
    }

    fn work_only(files: Vec<(&str, &str, u32)>) -> Fake {
        let mut src = Fake::default().dir("/g/S", vec![dir("work"), file("matrix_01_0001.sfrm", 1, Some(5000))]);
        let mut listing = Vec::new();
        for (name, text, t) in files {
            listing.push(file(name, text.len() as u64, Some(t)));
            src = src.file(&format!("/g/S/work/{name}"), text);
        }
        src.dir("/g/S/work", listing)
    }

    async fn sample_cell_of(src: &Fake) -> Option<Solve> {
        let info = scan_dir(src, "/g/S", "S", None, |_| {}).await.unwrap();
        info.solves().find(|s| s.kind == SolveKind::BrukerCell).cloned()
    }

    #[tokio::test]
    async fn the_sample_cell_trickles_down_integrated_matrix_cell_now() {
        // no .ins/.res: SAINT's _0m wins over APEX's export and CELL_NOW, and is read first
        let src = work_only(vec![
            ("S_0m.p4p", SAINT_P4P, 10),
            ("S_0u.p4p", SAINT_P4P, 11),
            ("ezu8f7.p4p", APEX2_EXPORT, 12),
            ("0egsl6.p4p_cn", CELLNOW_OUTPUT, 13),
        ]);
        let c = sample_cell_of(&src).await.unwrap();
        assert!(c.dir.ends_with("S_0m.p4p") && c.note.as_deref().unwrap().starts_with("integrated: S_0m.p4p"));
        assert_eq!(src.read_names(), ["S_0m.p4p"]);

        // no SAINT cell: APEX's export (the matrix cell, monoclinic C) beats Seattle's older one and CELL_NOW
        let src = work_only(vec![
            ("nac.p4p", SEATTLE_NAC, 10),
            ("ezu8f7.p4p", APEX2_EXPORT, 12),
            ("0egsl6.p4p", CELLNOW_INPUT, 13),
            ("0egsl6.p4p_cn", CELLNOW_OUTPUT, 14),
        ]);
        let c = sample_cell_of(&src).await.unwrap();
        let st = c.stats.as_ref().unwrap();
        assert_eq!((st.cell.unwrap()[0], st.space_group.as_deref()), (12.3946, Some("C (centring only)")));
        assert!(c.note.as_deref().unwrap().starts_with("matrix: ezu8f7.p4p"));
        assert!(!src.read_names().contains(&"0egsl6.p4p_cn".to_string()), "CELL_NOW is not needed");

        // only CELL_NOW: its newest result, from before the frames (another crystal?) says so
        let src = work_only(vec![("0egsl6.p4p", CELLNOW_INPUT, 13), ("0egsl6.p4p_cn", CELLNOW_OUTPUT, 14)]);
        let c = sample_cell_of(&src).await.unwrap();
        assert!(c.note.as_deref().unwrap().starts_with("CELL_NOW: 0egsl6.p4p_cn"));
        assert!(c.note.as_deref().unwrap().contains("before the frames"));
        assert_eq!(c.stats.as_ref().unwrap().space_group, None, "CELL_NOW gives no centring");

        // nothing but CELL_NOW's spot lists: no cell row at all
        let src = work_only(vec![("0egsl6.p4p", CELLNOW_INPUT, 13)]);
        assert!(sample_cell_of(&src).await.is_none());
    }

    #[tokio::test]
    async fn the_read_cap_names_what_was_left_out() {
        let mut files: Vec<(String, u32)> = (0..12u32).map(|i| (format!("s{i:02}.p4p"), 100 + i)).collect();
        files.push(("late.p4p_cn".into(), 50));
        let src = work_only(files.iter().map(|(n, t)| (n.as_str(), if n.ends_with("_cn") { CELLNOW_OUTPUT } else { CELLNOW_INPUT }, *t)).collect());
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        // ten spot lists read, two left out; CELL_NOW's result has reads of its own and is found
        assert_eq!(src.requests().1, MAX_CELL_FILES + 1);
        let w = work_run(&info);
        assert_eq!(w.solves.len(), 1);
        assert!(w.solves[0].note.as_deref().unwrap().starts_with("CELL_NOW: late.p4p_cn"));
        assert_eq!(w.note.as_deref(), Some("2 older cell file(s) not read: s01.p4p, s00.p4p"));
    }

    #[tokio::test]
    async fn a_dataset_without_a_cell_file_is_still_listed() {
        let bad = "TITL x in P-1\nCELL 0.71 10 20 30 1 1 179\nLATT 1\nEND\n";
        let src = work_only(vec![("lone.hkl", "1 2 3 4 5", 10), ("X.hkl", "1", 9), ("X.res", bad, 9), ("X.ins", XPREP_INS, 8)]);
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        let w = work_run(&info);
        let lone = &w.solves[0];
        assert_eq!((lone.kind, lone.stats.clone()), (SolveKind::BrukerSadabs, None));
        assert_eq!(lone.note.as_deref(), Some("no cell file for this data"));
        // a broken .res: the .ins of the same name gives the cell, and the problem is noted
        let x = &w.solves[1];
        assert_eq!(x.kind, SolveKind::BrukerXprep);
        assert_eq!(x.stats.as_ref().unwrap().cell.unwrap()[0], 24.06787);
        assert!(x.note.as_deref().unwrap().contains("X.res: no valid unit cell"), "{:?}", x.note);
        // the sample's cell also skips the broken .res
        assert!(w.solves[2].note.as_deref().unwrap().starts_with("final: X.ins"));
    }

    #[tokio::test]
    async fn integrated_but_unscaled_data_can_be_fetched_for_sadabs() {
        // SAINT ran (merged _0m.raw, per-run .raw, _0m.p4p, listing), SADABS did not: no .hkl
        let src = work_only(vec![
            ("S_0m.raw", "raw", 20),
            ("S_01.raw", "raw", 19),
            ("S_02.raw", "raw", 19),
            ("S_0m_uf.raw", "raw", 20),
            ("S_0m.p4p", SAINT_P4P, 20),
            ("S_0m._ls", SAINT_LS, 20),
        ]);
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        let w = work_run(&info);
        let s = &w.solves[0];
        assert_eq!((s.kind, s.kind.label()), (SolveKind::BrukerSaint, "SAINT"));
        assert_eq!(names(&s.hkl), ["S_0m.raw"], "SADABS takes the merged file; the runs are not needed");
        assert_eq!(names(&s.logs), ["S_0m.p4p", "S_0m._ls"]);
        // its quality: SAINT's coverage, from the merged listing
        let st = s.stats.as_ref().unwrap();
        assert!(st.shell_table[0].starts_with("SAINT (S_0m._ls), before scaling, point group -1"), "{:?}", st.shell_table);
        assert_eq!(st.scaling[0], "SAINT (S_0m._ls):");
        assert_eq!(st.overall.as_ref().unwrap().completeness, Some(43.11));
        assert_eq!(s.stats.as_ref().unwrap().cell.unwrap()[0], 18.2973);
        assert!(s.note.as_deref().unwrap().starts_with("integrated, not scaled: run SADABS on this file"));
        assert_eq!(w.solves[1].kind, SolveKind::BrukerCell, "the sample's cell is still listed");

        // after SADABS the .raw is still listed (to scale again), after the .hkl, and says so
        let src = work_only(vec![("S_0m.raw", "raw", 20), ("S_0m.hkl", "hkl", 21), ("S_0m.p4p", SAINT_P4P, 20)]);
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        let w = work_run(&info);
        let kinds: Vec<_> = w.solves.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [SolveKind::BrukerSadabs, SolveKind::BrukerSaint, SolveKind::BrukerCell]);
        assert!(w.solves[1].note.as_deref().unwrap().starts_with("SAINT's integrated reflections, already scaled into S_0m.hkl"));

        // a twin: TWINABS reads the .mul; its p4p has the components
        let src = work_only(vec![("twin4.mul", "mul", 20), ("twin4_01.mul", "mul", 19), ("twin4.p4p", SAINT_P4P, 20)]);
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        let s = &work_run(&info).solves[0];
        assert_eq!(names(&s.hkl), ["twin4.mul"]);
        assert_eq!(names(&s.logs), ["twin4.p4p"]);
        assert!(s.note.as_deref().unwrap().starts_with("integrated, not scaled: run TWINABS"));

        // no merged file: every run's file, in run order
        let src = work_only(vec![("S_02.raw", "raw", 20), ("S_01.raw", "raw", 19)]);
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        let s = &work_run(&info).solves[0];
        assert_eq!(names(&s.hkl), ["S_01.raw", "S_02.raw"]);
        assert!(s.stats.is_none() && s.note.as_deref().unwrap().contains("these runs' files"));
        assert!(is_run_file("3286AS264_B_02") && !is_run_file("3286AS264_B_0m") && !is_run_file("x_01m") && !is_run_file("twin4"));
    }

    #[test]
    fn space_groups_from_pcf_and_titles() {
        assert_eq!(pcf_space_group(XPREP_PCF).as_deref(), Some("C2/c"));
        assert_eq!(pcf_space_group("_space_group_name_H-M_alt 'P 21/c'").as_deref(), Some("P 21/c"));
        assert_eq!(pcf_space_group("_symmetry_space_group_name_H-M ?"), None);
        assert_eq!(title_space_group("3486S264_A in C2/c").as_deref(), Some("C2/c"));
        assert_eq!(title_space_group("3570JSG276_0m_t1 in P-1").as_deref(), Some("P-1"));
        assert_eq!(title_space_group("Integration of 3286AS264_B"), None);
        assert_eq!(title_space_group("crystal grown in water"), None);
    }

    #[tokio::test]
    async fn a_matrix_only_experiment_is_shown_with_its_runs_and_no_cell() {
        let mut v = vec![dir("work")];
        for run in 1..=3 {
            v.push(file(&format!("matrix_{run:02}_0001.sfrm"), 270_000, Some(10)));
        }
        let src = Fake::default().dir("/guest/RDT", v).dir("/guest/RDT/work", vec![]);
        let info = scan_dir(&src, "/guest/RDT", "RDT", None, |_| {}).await.unwrap();
        assert_eq!(info.runs.len(), 3);
        assert!(info.runs.iter().all(|r| r.kind == RunKind::Matrix && r.solves.is_empty()));
        assert!(!info.unprocessed() && !info.test_shots_only() && !info.has_data());
        assert_eq!(src.requests(), (2, 0));

        // no work/ at all: one listing
        let src = Fake::default().dir("/guest/RDT", vec![file("matrix_01_0001.sfrm", 1, None)]);
        let info = scan_dir(&src, "/guest/RDT", "RDT", None, |_| {}).await.unwrap();
        assert_eq!((info.runs.len(), src.requests()), (1, (1, 0)));

        // an empty folder is a sample without runs, not an error
        let src = Fake::default().dir("/guest/new", vec![]);
        assert!(scan_dir(&src, "/guest/new", "new", None, |_| {}).await.unwrap().runs.is_empty());
        // a folder that can't be listed is an error
        assert!(scan_dir(&Fake::default(), "/guest/x", "x", None, |_| {}).await.is_err());
    }

    #[tokio::test]
    async fn data_runs_without_cell_files_are_unprocessed() {
        let src = Fake::default().dir("/g/S", vec![file("S_01_0001.sfrm", 1, None)]);
        let info = scan_dir(&src, "/g/S", "S", None, |_| {}).await.unwrap();
        assert!(info.unprocessed());
    }

    #[tokio::test]
    async fn a_read_that_fails_fails_the_scan() {
        // the connection is probably gone: no half result
        let gone = Fake::default().dir("/g/S", vec![dir("work")]).dir("/g/S/work", vec![file("S.res", 1, None)]);
        assert!(scan_dir(&gone, "/g/S", "S", None, |_| {}).await.is_err());
    }

    #[test]
    fn frames_of_runs_include_matrix_runs() {
        let src = Fake::default().dir(E, frames_listing());
        let names = |runs: &[(&str, u32)]| {
            let runs: Vec<(String, u32)> = runs.iter().map(|(p, r)| (p.to_string(), *r)).collect();
            futures_lite_block(frames_of(&src, E, &runs)).unwrap().into_iter().map(|f| f.name().to_string()).collect::<Vec<_>>()
        };
        assert_eq!(names(&[("3286AS264_B", 2)]), ["3286AS264_B_02_0001.sfrm", "3286AS264_B_02_0002.sfrm", "3286AS264_B_02_0003.sfrm"]);
        assert_eq!(names(&[("matrix", 1)]), ["matrix_01_0001.sfrm", "matrix_01_0002.sfrm"]);
        assert_eq!(names(&[("3286AS264_B", 1), ("matrix", 1)]).len(), 5);
        assert!(names(&[("3286AS264_B", 9)]).is_empty());
    }

    fn futures_lite_block<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(1_716_288_749), "2024-05-21");
        assert_eq!(day(1_765_886_184), "2025-12-16");
    }
}
