//! Picks the best solve of a sample from its statistics.
//!
//! Only finished solves with a downloadable HKL file compete. The score (0–1) mixes
//! overall completeness (30 %), overall CC½ (30 %), the inner-shell R factor (20 %,
//! strong low-resolution data, so not inflated by weak high-resolution shells) and the
//! resolution (20 %, full marks at 0.80 Å, none at 1.20 Å). Missing values drop out
//! and the remaining weights are renormalised. The Stats panel shows the breakdown
//! (`breakdown`), so you can see why a solve got the ★.

use crate::model::{SampleInfo, Shell, Solve, SolveKind, Status};

/// Inner-shell R factor in %: Rmerge, or Rmeas when the pipeline doesn't report Rmerge
/// (xia2chem from 2026 on).
pub fn r_inner(s: &Shell) -> Option<f64> {
    s.rmerge_pct.or(s.rmeas_pct)
}

/// What the score is made of, in the order it is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Criterion {
    Completeness,
    CcHalf,
    RInner,
    Resolution,
}

impl Criterion {
    pub const ALL: [Criterion; 4] =
        [Criterion::Completeness, Criterion::CcHalf, Criterion::RInner, Criterion::Resolution];

    pub fn label(self) -> &'static str {
        match self {
            Criterion::Completeness => "Cmpl",
            Criterion::CcHalf => "CC½",
            Criterion::RInner => "R inner",
            Criterion::Resolution => "d_min",
        }
    }

    pub fn weight(self) -> f64 {
        match self {
            Criterion::Completeness | Criterion::CcHalf => 0.3,
            Criterion::RInner | Criterion::Resolution => 0.2,
        }
    }
}

/// Bonus that lets SHELX-ready xia2chem output win near-ties.
pub const XIA2CHEM_BONUS: f64 = 0.005;

/// A solve's score, criterion by criterion.
#[derive(Debug, Clone, PartialEq)]
pub struct Breakdown {
    /// Fraction (0–1) of each criterion's weight earned; `None` when the value is missing.
    pub parts: [(Criterion, Option<f64>); 4],
    pub bonus: f64,
}

impl Breakdown {
    /// Sum of the weights of the criteria that have a value.
    pub fn weight(&self) -> f64 {
        self.parts.iter().filter(|p| p.1.is_some()).map(|p| p.0.weight()).sum()
    }

    /// Points a criterion earned, out of 100 × its weight (Cmpl 29.9 of 30).
    pub fn earned(&self, c: Criterion) -> Option<f64> {
        let f = self.parts.iter().find(|p| p.0 == c)?.1?;
        Some(100.0 * c.weight() * f)
    }

    /// Points (out of 100) a criterion adds to the score, after renormalising.
    pub fn points(&self, c: Criterion) -> Option<f64> {
        let w = self.weight();
        (w > 0.0).then(|| self.earned(c).map(|e| e / w)).flatten()
    }

    pub fn score(&self) -> f64 {
        let w = self.weight();
        let sum: f64 = self.parts.iter().filter_map(|p| p.1.map(|f| p.0.weight() * f)).sum();
        sum / w + self.bonus
    }
}

/// Why a solve doesn't compete for ★.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unranked {
    Running,
    Failed,
    NoHkl,
    NoStats,
}

impl Unranked {
    pub fn reason(self) -> &'static str {
        match self {
            Unranked::Running => "still running",
            Unranked::Failed => "failed",
            Unranked::NoHkl => "no HKL file to download",
            Unranked::NoStats => "no statistics",
        }
    }
}

pub fn breakdown(s: &Solve) -> Result<Breakdown, Unranked> {
    match s.status {
        Status::Running => return Err(Unranked::Running),
        Status::Failed => return Err(Unranked::Failed),
        Status::Done => {}
    }
    if s.hkl.is_empty() {
        return Err(Unranked::NoHkl);
    }
    let st = s.stats.as_ref().ok_or(Unranked::NoStats)?;
    let o = st.overall.as_ref().ok_or(Unranked::NoStats)?;
    let b = Breakdown {
        parts: Criterion::ALL.map(|c| {
            let f = match c {
                Criterion::Completeness => o.completeness.map(|c| (c / 100.0).clamp(0.0, 1.0)),
                Criterion::CcHalf => o.cc_half.map(|h| h.clamp(0.0, 1.0)),
                Criterion::RInner => {
                    st.inner.as_ref().and_then(r_inner).map(|r| 1.0 - (r / 20.0).clamp(0.0, 1.0))
                }
                Criterion::Resolution => o.d_high.map(|d| ((1.2 - d) / 0.4).clamp(0.0, 1.0)),
            };
            (c, f)
        }),
        bonus: if s.kind == SolveKind::Xia2Chem { XIA2CHEM_BONUS } else { 0.0 },
    };
    if b.weight() == 0.0 {
        return Err(Unranked::NoStats);
    }
    Ok(b)
}

pub fn score(s: &Solve) -> Option<f64> {
    breakdown(s).ok().map(|b| b.score())
}

/// Key and score of every solve that competes, best first. Ties go to the earlier kind
/// (xia2chem, ednaproc, autoPROC noanom, autoPROC anom), then to the later run.
pub fn ranking(info: &SampleInfo) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64, SolveKind)> = info
        .solves()
        .filter_map(|s| score(s).map(|v| (s.key(), v, s.kind)))
        .collect();
    // reversed, so the stable sort puts the later of two equal solves first
    v.reverse();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    v.into_iter().map(|(k, v, _)| (k, v)).collect()
}

/// Key and score of the best solve, if any solve qualifies.
pub fn best(info: &SampleInfo) -> Option<(String, f64)> {
    ranking(info).into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RemoteFile, Run};

    fn solve(kind: SolveKind, fixture: &str) -> Solve {
        let xml = std::fs::read_to_string(format!(
            "{}/docs/server_samples/{fixture}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let mut s = Solve::new(kind, format!("/r/{}", kind.folder()), 1);
        s.status = Status::Done;
        let fraction = matches!(kind, SolveKind::Xia2 | SolveKind::Xia2Chem);
        s.stats = Some(crate::ispyb::parse(&xml, fraction).unwrap());
        s.hkl = vec![RemoteFile::new("/r/x.hkl", Some(1))];
        s
    }

    #[test]
    fn picks_xia2chem_for_jsg285b() {
        let info = SampleInfo {
            name: "JSG285b".into(),
            scanned_at: 0,
            runs: vec![Run {
                prefix: "p".into(),
                run: 3,
                characterisation: false,
                solves: vec![
                    solve(SolveKind::EdnaProc, "ednaproc_ispyb.xml"),
                    solve(SolveKind::AutoProcNoAnom, "autoproc_noanom_ispyb.xml"),
                    solve(SolveKind::Xia2Chem, "xia2chem_ispyb.xml"),
                ],
            }],
        };
        let (key, v) = best(&info).unwrap();
        assert!(key.ends_with("#xia2chem"), "{key}");
        assert!(v > 0.8 && v < 1.01);
    }

    #[test]
    fn only_done_solves_with_hkl_compete() {
        let mut s = solve(SolveKind::Xia2, "xia2chem_ispyb.xml");
        s.hkl.clear();
        assert!(score(&s).is_none());
        let mut s = solve(SolveKind::EdnaProc, "ednaproc_ispyb.xml");
        s.status = Status::Failed;
        assert!(score(&s).is_none());
    }

    fn jsg285b() -> SampleInfo {
        SampleInfo {
            name: "JSG285b".into(),
            scanned_at: 0,
            runs: vec![Run {
                prefix: "p".into(),
                run: 3,
                characterisation: false,
                solves: vec![
                    solve(SolveKind::EdnaProc, "ednaproc_ispyb.xml"),
                    solve(SolveKind::AutoProcNoAnom, "autoproc_noanom_ispyb.xml"),
                    solve(SolveKind::Xia2Chem, "xia2chem_ispyb.xml"),
                ],
            }],
        }
    }

    #[test]
    fn breakdown_adds_up_to_the_score() {
        let info = jsg285b();
        for s in info.solves() {
            let b = breakdown(s).unwrap();
            assert_eq!(b.weight(), 1.0);
            let sum: f64 = Criterion::ALL.iter().filter_map(|c| b.points(*c)).sum();
            assert!((sum / 100.0 + b.bonus - score(s).unwrap()).abs() < 1e-9);
            for c in Criterion::ALL {
                assert!(b.earned(c).unwrap() <= 100.0 * c.weight() + 1e-9);
            }
        }
        // xia2chem: 0.73 Å earns the full 20, and the tie-break bonus
        let x = info.solves().find(|s| s.kind == SolveKind::Xia2Chem).unwrap();
        let b = breakdown(x).unwrap();
        assert_eq!(b.earned(Criterion::Resolution), Some(20.0));
        assert_eq!(b.bonus, XIA2CHEM_BONUS);
    }

    #[test]
    fn ranking_is_best_first_and_agrees_with_best() {
        let info = jsg285b();
        let r = ranking(&info);
        assert_eq!(r.len(), 3);
        assert!(r.windows(2).all(|w| w[0].1 >= w[1].1));
        assert_eq!(best(&info).unwrap().0, r[0].0);
        assert!(r[0].0.ends_with("#xia2chem"));
    }

    #[test]
    fn missing_values_drop_out_and_the_rest_is_rescaled() {
        let mut s = solve(SolveKind::EdnaProc, "ednaproc_ispyb.xml");
        let full = breakdown(&s).unwrap();
        s.stats.as_mut().unwrap().overall.as_mut().unwrap().cc_half = None;
        let b = breakdown(&s).unwrap();
        assert_eq!(b.earned(Criterion::CcHalf), None);
        assert!((b.weight() - 0.7).abs() < 1e-9);
        assert_eq!(b.earned(Criterion::Completeness), full.earned(Criterion::Completeness));
        let c = b.points(Criterion::Completeness).unwrap();
        assert!((c - b.earned(Criterion::Completeness).unwrap() / 0.7).abs() < 1e-9);
        s.stats.as_mut().unwrap().overall = None;
        assert_eq!(breakdown(&s), Err(Unranked::NoStats));
        s.hkl.clear();
        assert_eq!(breakdown(&s), Err(Unranked::NoHkl));
        s.status = Status::Running;
        assert_eq!(breakdown(&s), Err(Unranked::Running));
    }

    #[test]
    fn rmeas_stands_in_for_missing_rmerge() {
        let s = solve(SolveKind::Xia2Chem, "xia2chem_2026_ispyb.xml");
        let inner = s.stats.as_ref().unwrap().inner.clone().unwrap();
        assert!(inner.rmerge_pct.is_none());
        assert!((r_inner(&inner).unwrap() - 57.63).abs() < 0.1);
        assert!(score(&s).is_some());
    }
}
