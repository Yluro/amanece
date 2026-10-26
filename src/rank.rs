//! Picks the best solve of a sample from its statistics.
//!
//! Only finished solves with a downloadable HKL file compete. The score (0–1) mixes
//! overall completeness (30 %), overall CC½ (30 %), the inner-shell R factor (20 %,
//! strong low-resolution data, so not inflated by weak high-resolution shells) and the
//! resolution (20 %, full marks at 0.80 Å, none at 1.20 Å). Missing values drop out
//! and the remaining weights are renormalised.

use crate::model::{SampleInfo, Shell, Solve, SolveKind, Status};

/// Inner-shell R factor in %: Rmerge, or Rmeas when the pipeline doesn't report Rmerge
/// (xia2chem from 2026 on).
pub fn r_inner(s: &Shell) -> Option<f64> {
    s.rmerge_pct.or(s.rmeas_pct)
}

pub fn score(s: &Solve) -> Option<f64> {
    if s.status != Status::Done || s.hkl.is_empty() {
        return None;
    }
    let st = s.stats.as_ref()?;
    let o = st.overall.as_ref()?;
    let mut terms: Vec<(f64, f64)> = Vec::new();
    if let Some(c) = o.completeness {
        terms.push((0.3, (c / 100.0).clamp(0.0, 1.0)));
    }
    if let Some(h) = o.cc_half {
        terms.push((0.3, h.clamp(0.0, 1.0)));
    }
    if let Some(r) = st.inner.as_ref().and_then(r_inner) {
        terms.push((0.2, 1.0 - (r / 20.0).clamp(0.0, 1.0)));
    }
    if let Some(d) = o.d_high {
        terms.push((0.2, ((1.2 - d) / 0.4).clamp(0.0, 1.0)));
    }
    let weight: f64 = terms.iter().map(|t| t.0).sum();
    if weight == 0.0 {
        return None;
    }
    let mut v = terms.iter().map(|t| t.0 * t.1).sum::<f64>() / weight;
    // SHELX-ready output wins near-ties.
    if s.kind == SolveKind::Xia2Chem {
        v += 0.005;
    }
    Some(v)
}

/// Key and score of the best solve, if any solve qualifies. Ties go to the earlier
/// kind (xia2chem, ednaproc, autoPROC noanom, autoPROC anom).
pub fn best(info: &SampleInfo) -> Option<(String, f64)> {
    info.solves()
        .filter_map(|s| score(s).map(|v| (s.key(), v, s.kind)))
        .max_by(|a, b| a.1.total_cmp(&b.1).then(b.2.cmp(&a.2)))
        .map(|(k, v, _)| (k, v))
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

    #[test]
    fn rmeas_stands_in_for_missing_rmerge() {
        let s = solve(SolveKind::Xia2Chem, "xia2chem_2026_ispyb.xml");
        let inner = s.stats.as_ref().unwrap().inner.clone().unwrap();
        assert!(inner.rmerge_pct.is_none());
        assert!((r_inner(&inner).unwrap() - 57.63).abs() < 0.1);
        assert!(score(&s).is_some());
    }
}
