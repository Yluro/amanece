//! The unit-cell database, `cells.toml` next to `config.toml`: one entry per sample
//! (its code, beamtime date, space group and the cell of its ★ solve), filled from the
//! scans amanece already has, so it costs nothing on the server. Cells can also be added
//! by hand (starting materials, ligands, common salts). The Stats panel lists every
//! entry whose lattice matches the selected solve's.
//!
//! Cells are compared as lattices. Each one is turned into a primitive cell (from the
//! centring letter of its space group) and Niggli-reduced (Křivý & Gruber 1976, with the
//! tolerances of Grosse-Kunstleve, Sauter & Adams 2004). So the P2₁/c and P2₁/n settings
//! of a crystal match, and so do a C2/c cell and the P-1 cell of the same crystal.
//! Near-equal edges and angles near 90° make the reduced cell ambiguous, so a match also
//! accepts any other basis of the second lattice made of short vectors (coefficients
//! −1, 0 or 1 on the reduced axes).
//!
//! amanece keeps the entries it wrote (those with a `from` line) in step with the scans,
//! and never changes an entry without `from`. Saving re-reads the file and only replaces
//! the entries amanece changed since, so hand edits and a second amanece window are safe.

use crate::cache::Cache;
use crate::layout;
use crate::model::{SampleInfo, Solve, Status};
use crate::rank;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

pub const FILE: &str = "cells.toml";

const HEADER: &str = "\
# amanece unit-cell database. amanece adds one entry per scanned sample: its code,
# the beamtime date, and the space group and cell of its best (★) solve.
# You can add your own cells (starting materials, ligands, salts...), e.g.
#
#   [[cell]]
#   code = \"Pd(OAc)2\"
#   sg = \"P 21/n\"
#   cell = [11.37, 15.62, 13.51, 90.0, 104.6, 90.0]
#   note = \"starting material\"
#
# Entries with a `from` line are kept up to date by amanece; delete that line to keep
# your own values. Comments are not kept when amanece rewrites this file.
";

/// Sample code and date, as an entry is identified: `(date, beamline, code)`.
pub type Key = (String, String, String);

pub fn sample_key(beamtime: &str, code: &str) -> Key {
    (
        layout::beamtime_date(beamtime).to_string(),
        layout::beamline_name(beamtime).unwrap_or_default(),
        code.to_string(),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Sample name, or any label for a cell added by hand.
    pub code: String,
    /// Beamtime date (YYYYMMDD); empty for a cell added by hand.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beamline: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sg: Option<String>,
    /// a b c (Å) α β γ (°)
    pub cell: [f64; 6],
    /// Pipeline of the solve the cell came from. Only entries with it are updated by
    /// amanece; hand-added ones have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Entry {
    fn is(&self, k: &Key) -> bool {
        self.date == k.0 && self.beamline.as_deref().unwrap_or("") == k.1 && self.code == k.2
    }

    fn reduced(&self) -> Option<G6> {
        reduce(self.cell, self.sg.as_deref())
    }
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default)]
    cell: Vec<Entry>,
}

/// How far two reduced cells may differ and still be the same lattice.
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    /// on each edge, relative
    pub length_pct: f64,
    /// on each angle
    pub angle_deg: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Match<'a> {
    pub entry: &'a Entry,
    /// Largest deviation as a fraction of its tolerance (0 = identical, 1 = at the limit).
    pub score: f64,
}

impl Match<'_> {
    /// 100 minus the largest deviation in % of its edge, where an angle off by
    /// `angle_deg` counts as much as an edge off by `length_pct`: 100 for the same cell,
    /// 100 − `length_pct` (98.5 by default) at the match limit.
    pub fn similarity_pct(&self, tol: Tolerance) -> f64 {
        100.0 - self.score * tol.length_pct
    }
}

#[derive(Debug, Default)]
pub struct CellDb {
    entries: Vec<Entry>,
    /// Niggli-reduced primitive cell of each entry, same order.
    reduced: Vec<Option<G6>>,
    /// Entries amanece recorded since the last save: only these overwrite the file.
    changed: BTreeSet<Key>,
    /// `cells.toml` exists but can't be read: it is never written, so no hand edit is lost.
    pub error: Option<String>,
}

impl CellDb {
    pub fn load(dir: Option<&Path>) -> CellDb {
        let mut db = CellDb::default();
        if let Some(dir) = dir {
            match read(&dir.join(FILE)) {
                Ok(entries) => db.set_entries(entries),
                Err(e) => db.error = Some(e),
            }
        }
        db
    }

    fn set_entries(&mut self, entries: Vec<Entry>) {
        self.reduced = entries.iter().map(Entry::reduced).collect();
        self.entries = entries;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Something to save (never while the file is unreadable).
    pub fn dirty(&self) -> bool {
        !self.changed.is_empty() && self.error.is_none()
    }

    /// Adds or updates the entry of one sample from its best solve. Test samples and
    /// samples without a finished cell are skipped; hand-made entries are left alone.
    pub fn record(&mut self, beamtime: &str, info: &SampleInfo) {
        if layout::is_test_name(&info.name) {
            return;
        }
        let Some((solve, cell)) = sample_cell(info) else {
            return;
        };
        let key = sample_key(beamtime, &info.name);
        let mut entry = Entry {
            code: key.2.clone(),
            date: key.0.clone(),
            beamline: Some(key.1.clone()).filter(|b| !b.is_empty()),
            sg: solve
                .stats
                .as_ref()
                .and_then(|st| st.space_group.as_deref())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            cell: tidy(cell),
            from: Some(solve.kind.label().to_string()),
            note: None,
        };
        let reduced = entry.reduced();
        match self.entries.iter().position(|e| e.is(&key)) {
            Some(i) if self.entries[i].from.is_none() => return,
            Some(i) => {
                entry.note = self.entries[i].note.clone();
                if self.entries[i] == entry {
                    return;
                }
                self.entries[i] = entry;
                self.reduced[i] = reduced;
            }
            None => {
                self.entries.push(entry);
                self.reduced.push(reduced);
            }
        }
        self.changed.insert(key);
    }

    /// Records every scanned sample of a cache.
    pub fn record_cache(&mut self, cache: &Cache) {
        for (beamtime, exp) in &cache.experiments {
            for info in exp.infos.values() {
                self.record(beamtime, info);
            }
        }
    }

    /// Entries whose lattice matches `cell`, closest first (newest first on ties),
    /// leaving out the entry `skip` (the sample being looked at).
    pub fn matches(&self, cell: [f64; 6], sg: Option<&str>, tol: Tolerance, skip: Option<&Key>) -> Vec<Match<'_>> {
        let Some(q) = reduce(cell, sg) else {
            return Vec::new();
        };
        let mut found: Vec<Match> = self
            .entries
            .iter()
            .zip(&self.reduced)
            .filter(|(e, _)| skip.is_none_or(|k| !e.is(k)))
            .filter_map(|(e, r)| {
                Some(Match {
                    entry: e,
                    score: same_lattice(&q, r.as_ref()?, tol)?,
                })
            })
            .collect();
        found.sort_by(|a, b| a.score.total_cmp(&b.score).then_with(|| b.entry.date.cmp(&a.entry.date)));
        found
    }

    /// Writes the entries amanece changed into `dir/cells.toml`, on top of what the file
    /// holds now (hand edits, another window's entries).
    pub fn save(&mut self, dir: &Path) -> Result<()> {
        if !self.dirty() {
            return Ok(());
        }
        let path = dir.join(FILE);
        let mut disk = match read(&path) {
            Ok(d) => d,
            Err(e) => {
                self.error = Some(e.clone());
                bail!("{e}; it is left as it is until fixed");
            }
        };
        for key in &self.changed {
            let Some(mine) = self.entries.iter().find(|e| e.is(key)) else {
                continue;
            };
            match disk.iter_mut().find(|e| e.is(key)) {
                // turned into a hand-made entry in the meantime
                Some(d) if d.from.is_none() => {}
                Some(d) => {
                    let note = d.note.take();
                    *d = Entry { note, ..mine.clone() };
                }
                None => disk.push(mine.clone()),
            }
        }
        // hand-made entries first, in their order; then the samples, newest first
        disk.sort_by(|a, b| match (a.from.is_some(), b.from.is_some()) {
            (false, false) => std::cmp::Ordering::Equal,
            (true, true) => b.date.cmp(&a.date).then_with(|| a.code.cmp(&b.code)),
            (x, y) => x.cmp(&y),
        });
        let mut body = toml::to_string(&File { cell: disk.clone() })?;
        body.insert_str(0, &format!("{HEADER}\n"));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &path)?;
        self.set_entries(disk);
        self.changed.clear();
        Ok(())
    }
}

fn read(path: &Path) -> std::result::Result<Vec<Entry>, String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
        Ok(text) => toml::from_str::<File>(&text)
            .map(|f| f.cell)
            .map_err(|e| format!("cannot read {}: {e}", path.display())),
    }
}

/// The solve whose cell stands for the sample: the ★ one, else any finished one with a cell.
pub fn sample_cell(info: &SampleInfo) -> Option<(&Solve, [f64; 6])> {
    let cell = |s: &Solve| s.stats.as_ref().and_then(|st| st.cell);
    let best = rank::best(info).map(|(k, _)| k);
    info.solves()
        .find(|s| best.as_deref() == Some(s.key().as_str()) && cell(s).is_some())
        .or_else(|| info.solves().find(|s| s.status == Status::Done && cell(s).is_some()))
        .and_then(|s| Some((s, cell(s)?)))
}

/// Edges to 0.001 Å and angles to 0.01°, so the file stays readable.
fn tidy(c: [f64; 6]) -> [f64; 6] {
    let r = |x: f64, k: f64| (x * k).round() / k;
    [r(c[0], 1e3), r(c[1], 1e3), r(c[2], 1e3), r(c[3], 1e2), r(c[4], 1e2), r(c[5], 1e2)]
}

// ------------------------------------------------------------------ lattices

/// Metric as a G6 vector: (a·a, b·b, c·c, 2b·c, 2a·c, 2a·b), in Å².
pub type G6 = [f64; 6];

type M3 = [[f64; 3]; 3];

fn metric(c: [f64; 6]) -> M3 {
    let [a, b, cc, al, be, ga] = c;
    let (ca, cb, cg) = (al.to_radians().cos(), be.to_radians().cos(), ga.to_radians().cos());
    [
        [a * a, a * b * cg, a * cc * cb],
        [a * b * cg, b * b, b * cc * ca],
        [a * cc * cb, b * cc * ca, cc * cc],
    ]
}

fn g6(g: &M3) -> G6 {
    [g[0][0], g[1][1], g[2][2], 2.0 * g[1][2], 2.0 * g[0][2], 2.0 * g[0][1]]
}

fn metric_of(p: &G6) -> M3 {
    [
        [p[0], p[5] / 2.0, p[4] / 2.0],
        [p[5] / 2.0, p[1], p[3] / 2.0],
        [p[4] / 2.0, p[3] / 2.0, p[2]],
    ]
}

fn det(g: &M3) -> f64 {
    g[0][0] * (g[1][1] * g[2][2] - g[1][2] * g[2][1]) - g[0][1] * (g[1][0] * g[2][2] - g[1][2] * g[2][0])
        + g[0][2] * (g[1][0] * g[2][1] - g[1][1] * g[2][0])
}

fn volume(p: &G6) -> f64 {
    det(&metric_of(p)).max(0.0).sqrt()
}

/// a b c α β γ of a G6 vector.
pub fn params(p: &G6) -> [f64; 6] {
    let (a, b, c) = (p[0].sqrt(), p[1].sqrt(), p[2].sqrt());
    let ang = |x: f64, l: f64, m: f64| (x / (2.0 * l * m)).clamp(-1.0, 1.0).acos().to_degrees();
    [a, b, c, ang(p[3], b, c), ang(p[4], a, c), ang(p[5], a, b)]
}

/// Lattice centring of a space group: `P 21/c` → P, `C 2/c` → C, `#15` (ednaproc's
/// number) → C. `H` (hexagonal setting of R) counts as R; anything unknown as P.
pub fn centring(sg: &str) -> char {
    let s = sg.trim();
    if let Ok(n) = s.trim_start_matches('#').trim().parse::<u32>() {
        return match n {
            5 | 8 | 9 | 12 | 15 | 20 | 21 | 35..=37 | 63..=68 => 'C',
            38..=41 => 'A',
            22 | 42 | 43 | 69 | 70 | 196 | 202 | 203 | 209 | 210 | 216 | 219 | 225..=228 => 'F',
            23 | 24 | 44..=46 | 71..=74 | 79 | 80 | 82 | 87 | 88 | 97 | 98 | 107..=110 | 119..=122
            | 139..=142 | 197 | 199 | 204 | 206 | 211 | 214 | 217 | 220 | 229 | 230 => 'I',
            146 | 148 | 155 | 160 | 161 | 166 | 167 => 'R',
            _ => 'P',
        };
    }
    match s.chars().next().map(|c| c.to_ascii_uppercase()) {
        Some('H') => 'R',
        Some(c @ ('A' | 'B' | 'C' | 'I' | 'F' | 'R')) => c,
        _ => 'P',
    }
}

/// A primitive basis of the centred lattice, as rows in the conventional cell's
/// fractional coordinates.
fn primitive_basis(centring: char) -> M3 {
    const H: f64 = 0.5;
    const T: f64 = 1.0 / 3.0;
    match centring {
        'A' => [[1.0, 0.0, 0.0], [0.0, H, H], [0.0, -H, H]],
        'B' => [[H, 0.0, H], [0.0, 1.0, 0.0], [-H, 0.0, H]],
        'C' => [[H, H, 0.0], [-H, H, 0.0], [0.0, 0.0, 1.0]],
        'I' => [[-H, H, H], [H, -H, H], [H, H, -H]],
        'F' => [[0.0, H, H], [H, 0.0, H], [H, H, 0.0]],
        // obverse rhombohedral centring of the hexagonal cell
        'R' => [[2.0 * T, T, T], [-T, T, T], [-T, -2.0 * T, T]],
        _ => [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// `vᵢ·G·vⱼ` for the rows of `p`.
fn transform(g: &M3, p: &M3) -> M3 {
    let mut out = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for k in 0..3 {
                for l in 0..3 {
                    out[i][j] += p[i][k] * g[k][l] * p[j][l];
                }
            }
        }
    }
    out
}

/// Niggli-reduced primitive cell of a cell in the setting of space group `sg`
/// (primitive when unknown). `None` for a cell that isn't one.
pub fn reduce(cell: [f64; 6], sg: Option<&str>) -> Option<G6> {
    let [a, b, c, al, be, ga] = cell;
    let edges_ok = [a, b, c].iter().all(|x| x.is_finite() && *x > 0.1);
    let angles_ok = [al, be, ga].iter().all(|x| x.is_finite() && *x > 1.0 && *x < 179.0);
    if !edges_ok || !angles_ok {
        return None;
    }
    let mut z = sg.map(centring).unwrap_or('P');
    // an R group given on rhombohedral axes is already primitive
    if z == 'R' && !((a - b).abs() < 0.01 * a && (ga - 120.0).abs() < 1.0) {
        z = 'P';
    }
    let g = transform(&metric(cell), &primitive_basis(z));
    if det(&g) <= 0.0 {
        return None;
    }
    Some(niggli(g6(&g)))
}

/// Křivý–Gruber reduction of a G6 vector, with the epsilon handling of
/// Grosse-Kunstleve et al. (Acta Cryst. A60, 1–6, 2004).
pub fn niggli(p: G6) -> G6 {
    let [mut a, mut b, mut c, mut xi, mut eta, mut zeta] = p;
    let e = 1e-5 * volume(&p).cbrt().powi(2);
    let sign = |x: f64| {
        if x > e {
            1
        } else if x < -e {
            -1
        } else {
            0
        }
    };
    for _ in 0..1000 {
        // N1
        if a > b + e || ((a - b).abs() <= e && xi.abs() > eta.abs() + e) {
            std::mem::swap(&mut a, &mut b);
            std::mem::swap(&mut xi, &mut eta);
        }
        // N2
        if b > c + e || ((b - c).abs() <= e && eta.abs() > zeta.abs() + e) {
            std::mem::swap(&mut b, &mut c);
            std::mem::swap(&mut eta, &mut zeta);
            continue;
        }
        // N3, N4: all angles acute, or all non-acute
        if sign(xi) * sign(eta) * sign(zeta) == 1 {
            (xi, eta, zeta) = (xi.abs(), eta.abs(), zeta.abs());
        } else {
            (xi, eta, zeta) = (-xi.abs(), -eta.abs(), -zeta.abs());
        }
        // N5
        if xi.abs() > b + e || ((xi - b).abs() <= e && 2.0 * eta < zeta - e) || ((xi + b).abs() <= e && zeta < -e) {
            let s = xi.signum();
            c = b + c - xi * s;
            eta -= zeta * s;
            xi -= 2.0 * b * s;
            continue;
        }
        // N6
        if eta.abs() > a + e || ((eta - a).abs() <= e && 2.0 * xi < zeta - e) || ((eta + a).abs() <= e && zeta < -e) {
            let s = eta.signum();
            c = a + c - eta * s;
            xi -= zeta * s;
            eta -= 2.0 * a * s;
            continue;
        }
        // N7
        if zeta.abs() > a + e || ((zeta - a).abs() <= e && 2.0 * xi < eta - e) || ((zeta + a).abs() <= e && eta < -e) {
            let s = zeta.signum();
            b = a + b - zeta * s;
            xi -= eta * s;
            zeta -= 2.0 * a * s;
            continue;
        }
        // N8
        let sum = xi + eta + zeta + a + b;
        if sum < -e || (sum.abs() <= e && 2.0 * (a + eta) + zeta > e) {
            c += a + b + xi + eta + zeta;
            xi += 2.0 * b + zeta;
            eta += 2.0 * a + zeta;
            continue;
        }
        break;
    }
    [a, b, c, xi, eta, zeta]
}

/// Whether two reduced cells describe the same lattice within `tol`: some basis of `q`
/// made of short vectors has `p`'s edges and angles. Returns the largest deviation as a
/// fraction of its tolerance.
pub fn same_lattice(p: &G6, q: &G6, tol: Tolerance) -> Option<f64> {
    let lt = tol.length_pct / 100.0;
    let (vp, vq) = (volume(p), volume(q));
    if (vp - vq).abs() > (3.0 * lt + 0.02) * vp.max(vq) {
        return None;
    }
    let target = params(p);
    let g = metric_of(q);
    let dot = |u: &[i32; 3], v: &[i32; 3]| {
        let mut s = 0.0;
        for i in 0..3 {
            for j in 0..3 {
                s += f64::from(u[i]) * g[i][j] * f64::from(v[j]);
            }
        }
        s
    };
    // the 13 lattice vectors with coefficients −1..1, one of each ± pair
    let mut short: Vec<([i32; 3], f64)> = Vec::new();
    for i in -1..=1 {
        for j in -1..=1 {
            for k in -1..=1 {
                let v = [i, j, k];
                if v.iter().find(|x| **x != 0).is_some_and(|x| *x > 0) {
                    short.push((v, dot(&v, &v).sqrt()));
                }
            }
        }
    }
    let near = |t: f64| -> Vec<&([i32; 3], f64)> { short.iter().filter(|(_, l)| (l - t).abs() <= lt * t).collect() };
    let (ca, cb, cc) = (near(target[0]), near(target[1]), near(target[2]));
    let mut best: Option<f64> = None;
    for (u, lu) in &ca {
        for (v, lv) in &cb {
            for (w, lw) in &cc {
                let d = u[0] * (v[1] * w[2] - v[2] * w[1]) - u[1] * (v[0] * w[2] - v[2] * w[0])
                    + u[2] * (v[0] * w[1] - v[1] * w[0]);
                if d.abs() != 1 {
                    continue;
                }
                let dl = [(lu, target[0]), (lv, target[1]), (lw, target[2])]
                    .iter()
                    .map(|(l, t)| (**l - t).abs() / t / lt)
                    .fold(0.0, f64::max);
                let cos = |x: &[i32; 3], y: &[i32; 3], lx: f64, ly: f64| dot(x, y) / (lx * ly);
                let (cvw, cuw, cuv) = (cos(v, w, *lv, *lw), cos(u, w, *lu, *lw), cos(u, v, *lu, *lv));
                // flipping one vector flips the two angles it makes
                for (su, sv, sw) in [(1.0, 1.0, 1.0), (-1.0, 1.0, 1.0), (1.0, -1.0, 1.0), (1.0, 1.0, -1.0)] {
                    let ang = |c: f64| c.clamp(-1.0, 1.0).acos().to_degrees();
                    let da = [
                        (ang(cvw * sv * sw) - target[3]).abs(),
                        (ang(cuw * su * sw) - target[4]).abs(),
                        (ang(cuv * su * sv) - target[5]).abs(),
                    ]
                    .iter()
                    .fold(0.0, |m: f64, x| m.max(*x))
                        / tol.angle_deg;
                    let score = dl.max(da);
                    if score <= 1.0 && best.is_none_or(|b| score < b) {
                        best = Some(score);
                    }
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Experiment, RemoteFile, Run, SolveKind, Stats};

    const TOL: Tolerance = Tolerance {
        length_pct: 1.5,
        angle_deg: 2.0,
    };

    /// Cell parameters of the basis given by the rows of `p` (in `cell`'s coordinates).
    fn setting(cell: [f64; 6], p: M3) -> [f64; 6] {
        params(&g6(&transform(&metric(cell), &p)))
    }

    fn close(a: [f64; 6], b: [f64; 6]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-6)
    }

    fn matches(a: [f64; 6], sa: &str, b: [f64; 6], sb: &str) -> bool {
        same_lattice(&reduce(a, Some(sa)).unwrap(), &reduce(b, Some(sb)).unwrap(), TOL).is_some()
    }

    #[test]
    fn similarity_is_100_minus_the_largest_deviation() {
        let p = reduce([10.0, 12.0, 15.0, 90.0, 100.0, 90.0], Some("P 21/c")).unwrap();
        let same = same_lattice(&p, &p, TOL).unwrap();
        // b 0.6 % longer: 99.4 %
        let q = reduce([10.0, 12.072, 15.0, 90.0, 100.0, 90.0], Some("P 21/c")).unwrap();
        let off = same_lattice(&p, &q, TOL).unwrap();
        let e = Entry { code: "x".into(), date: String::new(), beamline: None, sg: None, cell: [0.0; 6], from: None, note: None };
        let sim = |score| Match { entry: &e, score }.similarity_pct(TOL);
        assert!((sim(same) - 100.0).abs() < 1e-9);
        assert!((sim(off) - 99.4).abs() < 0.01, "{}", sim(off));
        assert!((sim(1.0) - 98.5).abs() < 1e-9);
    }

    #[test]
    fn reduced_cell_does_not_depend_on_the_basis() {
        let cell = [7.12, 9.85, 13.4, 82.3, 76.9, 88.1];
        let r = params(&reduce(cell, None).unwrap());
        // a skewed basis of the same lattice: b + a, c - a + b
        let skew = setting(cell, [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [-1.0, 1.0, 1.0]]);
        assert!(!close(skew, cell));
        assert!(close(params(&reduce(skew, None).unwrap()), r), "{r:?}");
        assert!(r[0] <= r[1] && r[1] <= r[2]);
    }

    #[test]
    fn p21c_matches_its_p21n_setting() {
        let c = [8.214, 11.032, 14.876, 90.0, 118.7, 90.0];
        // P21/n: a' = a, c' = a + c
        let n = setting(c, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0]]);
        assert!((n[4] - c[4]).abs() > 10.0, "{n:?}");
        assert!(matches(c, "P 21/c", n, "P 1 21/n 1"));
    }

    #[test]
    fn centred_cells_match_their_primitive_cells() {
        let c2c = [24.31, 6.12, 18.77, 90.0, 109.4, 90.0];
        let p = setting(c2c, primitive_basis('C'));
        assert!(matches(c2c, "C 2/c", p, "P -1"));
        assert!(matches(c2c, "#15", p, "P1"));
        // without the centring it's a different (doubled) cell
        assert!(!matches(c2c, "P 2/c", p, "P -1"));

        let fmmm = [18.0, 21.82, 55.56, 90.0, 90.0, 90.0];
        assert!(matches(fmmm, "F m m m", setting(fmmm, primitive_basis('F')), "P 1"));
        let i = [10.2, 10.2, 21.7, 90.0, 90.0, 90.0];
        assert!(matches(i, "I 41/a", setting(i, primitive_basis('I')), "P 1"));

        // R-3 on hexagonal axes vs. its rhombohedral cell
        let hex = [13.95, 13.95, 9.41, 90.0, 90.0, 120.0];
        let rh = setting(hex, primitive_basis('R'));
        assert!((rh[0] - rh[1]).abs() < 1e-6 && (rh[3] - 90.0).abs() > 5.0, "{rh:?}");
        assert!(matches(hex, "R -3", rh, "R -3"));
        assert!(matches(hex, "H 3", rh, "P 1"));
    }

    #[test]
    fn tolerance_and_near_90_degree_angles() {
        let c = [10.0, 12.0, 15.0, 90.0, 95.0, 90.0];
        assert!(matches(c, "P 21/c", [10.1, 12.05, 14.95, 90.0, 95.6, 90.0], "P 21/c"));
        // angles either side of 90° reduce to different forms, but are the same lattice
        assert!(matches([9.0, 11.0, 13.0, 89.9, 90.2, 89.8], "P1", [9.0, 11.0, 13.0, 90.1, 89.9, 90.1], "P1"));
        // 3 % on one edge, or 4° on β: different
        assert!(!matches(c, "P 21/c", [10.3, 12.0, 15.0, 90.0, 95.0, 90.0], "P 21/c"));
        assert!(!matches(c, "P 21/c", [10.0, 12.0, 15.0, 90.0, 99.0, 90.0], "P 21/c"));
        // all-acute and all-obtuse cells with the same edges and |cos| are not the same lattice
        assert!(!matches([10.0, 10.5, 11.0, 80.0, 80.0, 80.0], "P1", [10.0, 10.5, 11.0, 100.0, 100.0, 100.0], "P1"));
        // nonsense cells are ignored
        assert!(reduce([0.0, 1.0, 1.0, 90.0, 90.0, 90.0], None).is_none());
        assert!(reduce([5.0, 5.0, 5.0, 0.0, 90.0, 90.0], None).is_none());
    }

    #[test]
    fn centring_letters() {
        assert_eq!(centring("P 21/c"), 'P');
        assert_eq!(centring("c 2/c"), 'C');
        assert_eq!(centring("#75"), 'P');
        assert_eq!(centring("#167"), 'R');
        assert_eq!(centring("230"), 'I');
        assert_eq!(centring("H 3"), 'R');
        assert_eq!(centring(""), 'P');
    }

    fn sample(name: &str, cell: [f64; 6], sg: &str) -> SampleInfo {
        let mut s = Solve::new(SolveKind::Xia2Chem, format!("/r/{name}/xia2chem_1"), 1);
        s.status = Status::Done;
        s.stats = Some(Stats {
            space_group: Some(sg.into()),
            cell: Some(cell),
            ..Stats::default()
        });
        s.hkl = vec![RemoteFile::new("/r/shelxt.hkl", Some(1))];
        SampleInfo {
            name: name.into(),
            scanned_at: 0,
            runs: vec![Run {
                prefix: "p".into(),
                run: 1,
                characterisation: false,
                solves: vec![s],
            }],
        }
    }

    #[test]
    fn records_matches_and_keeps_hand_edits() {
        let dir = std::env::temp_dir().join(format!("amanece_cells_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let a = [8.214, 11.032, 14.876, 90.0, 118.7, 90.0];
        let mut db = CellDb::load(Some(&dir));
        assert_eq!(db.len(), 0);

        let mut cache = Cache::default();
        let mut exp = Experiment::default();
        exp.infos.insert("JSG241".into(), sample("JSG241", a, "P 21/c"));
        exp.infos.insert("JSG242".into(), sample("JSG242", [5.1, 6.2, 7.3, 90.0, 90.0, 90.0], "P 21 21 21"));
        exp.infos.insert("test1".into(), sample("test1", a, "P 21/c"));
        cache.experiments.insert("bl13/projects/cycle2025-II/2025000000-pi/20250704".into(), exp);
        db.record_cache(&cache);
        assert_eq!(db.len(), 2, "test samples are left out");
        assert!(db.dirty());

        // the same crystal measured again, reported in P21/n
        let n = setting(a, [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 0.0, 1.0]]);
        db.record("20261023", &sample("JSG301", n, "P 21/n"));
        let skip = sample_key("20261023", "JSG301");
        let found = db.matches(n, Some("P 21/n"), TOL, Some(&skip));
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].entry.code.as_str(), found[0].entry.date.as_str()), ("JSG241", "20250704"));
        assert_eq!(found[0].entry.beamline.as_deref(), Some("XALOC"));
        assert_eq!(db.matches(n, Some("P 21/n"), TOL, None).len(), 2);

        db.save(&dir).unwrap();
        assert!(!db.dirty());
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(text.starts_with("# amanece unit-cell database") && text.contains("[[cell]]"), "{text}");

        // by hand: a note on a recorded entry, JSG242 taken over, and a cell of our own
        let mut entries = read(&dir.join(FILE)).unwrap();
        for e in &mut entries {
            match e.code.as_str() {
                "JSG241" => e.note = Some("first batch".into()),
                "JSG242" => {
                    e.from = None;
                    e.note = Some("wrong crystal".into());
                }
                _ => {}
            }
        }
        let text = format!(
            "{}\n[[cell]]\ncode = \"ligand L1\"\nsg = \"C 2/c\"\ncell = [24.31, 6.12, 18.77, 90.0, 109.4, 90.0]\n",
            toml::to_string(&File { cell: entries }).unwrap()
        );
        std::fs::write(dir.join(FILE), &text).unwrap();
        let mut db = CellDb::load(Some(&dir));
        assert_eq!(db.len(), 4);
        // a rescan with other cells updates recorded entries, not hand-made ones
        let mut other = Cache::default();
        let mut exp = Experiment::default();
        exp.infos.insert("JSG242".into(), sample("JSG242", [9.0, 9.0, 9.0, 90.0, 90.0, 90.0], "P 2 3"));
        let mut a2 = a;
        a2[0] += 0.01;
        exp.infos.insert("JSG241".into(), sample("JSG241", a2, "P 21/c"));
        other.experiments.insert("bl13/projects/cycle2025-II/2025000000-pi/20250704".into(), exp);
        db.record_cache(&other);
        db.record("20261023", &sample("JSG301", [9.5, 9.5, 9.5, 90.0, 90.0, 90.0], "P 2 3"));
        // meanwhile another window adds a sample
        let mut win2 = CellDb::load(Some(&dir));
        win2.record("20261106", &sample("JSG400", [6.0, 7.0, 8.0, 90.0, 90.0, 90.0], "P 21 21 21"));
        win2.save(&dir).unwrap();
        db.save(&dir).unwrap();

        let back = CellDb::load(Some(&dir));
        let get = |code: &str| back.entries.iter().find(|e| e.code == code).unwrap();
        assert_eq!(back.len(), 5);
        assert_eq!(get("JSG242").cell, [5.1, 6.2, 7.3, 90.0, 90.0, 90.0]);
        assert_eq!(get("JSG242").note.as_deref(), Some("wrong crystal"));
        assert_eq!(get("JSG241").cell[0], 8.224);
        assert_eq!(get("JSG241").note.as_deref(), Some("first batch"));
        assert_eq!(get("JSG301").cell[0], 9.5);
        assert_eq!(get("JSG400").date, "20261106");
        assert!(get("ligand L1").from.is_none());
        assert_eq!(back.entries[0].code, "JSG242", "hand-made entries first");
        // the ligand is found from its primitive cell
        let p = setting([24.31, 6.12, 18.77, 90.0, 109.4, 90.0], primitive_basis('C'));
        assert_eq!(back.matches(p, None, TOL, None)[0].entry.code, "ligand L1");

        // a file that doesn't parse is never overwritten
        std::fs::write(dir.join(FILE), "[[cell]]\ncode = \n").unwrap();
        let mut broken = CellDb::load(Some(&dir));
        assert!(broken.error.is_some());
        broken.record("20261106", &sample("JSG401", a, "P 21/c"));
        assert!(!broken.dirty());
        broken.save(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "[[cell]]\ncode = \n");
        std::fs::remove_dir_all(&dir).ok();
    }
}
