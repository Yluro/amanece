//! Parser for the ISPyB `AutoProcContainer` XML that every ALBA pipeline writes.

use crate::model::{Shell, Stats};
use anyhow::{Context, Result, anyhow};
use roxmltree::{Document, Node};

/// `r_fraction`: the file stores R factors as fractions (xia2 `ispyb.xml`, autoPROC's own
/// `autoPROC.xml`, `fast_dp.xml`) rather than percentages (EDNA's
/// `ISPyBStoreAutoProcv1_4_dataInput.xml`). They come out as percentages either way.
pub fn parse(xml: &str, r_fraction: bool) -> Result<Stats> {
    let doc = Document::parse(xml).context("invalid ISPyB XML")?;
    let container = doc
        .descendants()
        .find(|n| is(n, "AutoProcContainer"))
        .ok_or_else(|| anyhow!("no AutoProcContainer element"))?;

    let mut stats = Stats::default();

    if let Some(ap) = child(container, "AutoProc") {
        stats.space_group = text(ap, "spaceGroup").map(|s| s.to_string());
        let cell: Vec<Option<f64>> = ["a", "b", "c", "alpha", "beta", "gamma"]
            .iter()
            .map(|c| num(ap, &format!("refinedCell_{c}")))
            .collect();
        if cell.iter().all(Option::is_some) {
            let mut arr = [0.0; 6];
            for (i, v) in cell.into_iter().enumerate() {
                arr[i] = v.unwrap_or_default();
            }
            stats.cell = Some(arr);
        }
    }

    let r_scale = if r_fraction { 100.0 } else { 1.0 };
    for st in container
        .descendants()
        .filter(|n| is(n, "AutoProcScalingStatistics"))
    {
        let shell = Shell {
            d_low: num(st, "resolutionLimitLow"),
            d_high: num(st, "resolutionLimitHigh"),
            completeness: num(st, "completeness"),
            multiplicity: num(st, "multiplicity"),
            i_sig: num(st, "meanIOverSigI"),
            rmerge_pct: num(st, "rMerge").map(|v| v * r_scale),
            rmeas_pct: num(st, "rMeasAllIPlusIMinus")
                .or_else(|| num(st, "rMeasWithinIPlusIMinus"))
                .map(|v| v * r_scale),
            rpim_pct: num(st, "rPimAllIPlusIMinus")
                .or_else(|| num(st, "rPimWithinIPlusIMinus"))
                .map(|v| v * r_scale),
            cc_half: num(st, "ccHalf"),
            n_obs: num(st, "nTotalObservations").map(|v| v as u64),
            n_unique: num(st, "nTotalUniqueObservations").map(|v| v as u64),
        };
        if stats.anomalous.is_none() {
            stats.anomalous = text(st, "anomalous").map(|t| t.eq_ignore_ascii_case("true"));
        }
        match text(st, "scalingStatisticsType") {
            Some("overall") => stats.overall = Some(shell),
            Some("innerShell") => stats.inner = Some(shell),
            Some("outerShell") => stats.outer = Some(shell),
            _ => {}
        }
    }

    if let Some(int) = container
        .descendants()
        .find(|n| is(n, "AutoProcIntegration"))
        && let (Some(a), Some(b)) = (num(int, "startImageNumber"), num(int, "endImageNumber"))
    {
        stats.images = Some((a as u32, b as u32));
    }

    if stats.overall.is_none() && stats.space_group.is_none() {
        return Err(anyhow!("ISPyB XML holds no statistics"));
    }
    Ok(stats)
}

fn is(n: &Node, name: &str) -> bool {
    n.is_element() && n.tag_name().name().eq_ignore_ascii_case(name)
}

fn child<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| is(c, name))
}

fn text<'a>(n: Node<'a, '_>, name: &str) -> Option<&'a str> {
    child(n, name)
        .and_then(|c| c.text())
        .map(str::trim)
        .filter(|t| !t.is_empty() && *t != "None")
}

fn num(n: Node, name: &str) -> Option<f64> {
    text(n, name).and_then(|t| t.parse::<f64>().ok()).filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/docs/server_samples/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn xia2chem_fractions_become_percent() {
        let s = parse(&fixture("xia2chem_ispyb.xml"), true).unwrap();
        assert_eq!(s.space_group.as_deref(), Some("P 4"));
        let o = s.overall.unwrap();
        assert!((o.rmerge_pct.unwrap() - 8.529).abs() < 0.01);
        assert!((o.d_high.unwrap() - 0.733).abs() < 0.001);
        assert!((o.cc_half.unwrap() - 0.998).abs() < 0.001);
        assert_eq!(o.n_unique, Some(16078));
        assert_eq!(s.images, Some((1, 720)));
        assert!((s.cell.unwrap()[0] - 28.45).abs() < 0.01);
    }

    #[test]
    fn ednaproc_percent_stays_percent() {
        let s = parse(&fixture("ednaproc_ispyb.xml"), false).unwrap();
        let o = s.overall.unwrap();
        assert!((o.rmerge_pct.unwrap() - 7.2).abs() < 0.01);
        assert!((o.d_high.unwrap() - 0.94).abs() < 0.001);
        assert_eq!(s.anomalous, Some(true));
        // EDNA spells it ntotalUniqueObservations
        assert!(o.n_unique.is_some());
        assert!(s.inner.is_some() && s.outer.is_some());
    }

    #[test]
    fn autoproc_wrapped_container() {
        let s = parse(&fixture("autoproc_noanom_ispyb.xml"), false).unwrap();
        let o = s.overall.unwrap();
        assert!((o.rmerge_pct.unwrap() - 8.9).abs() < 0.01);
        assert!((o.d_high.unwrap() - 0.859).abs() < 0.001);
        assert_eq!(s.anomalous, Some(false));
    }

    #[test]
    fn rejects_empty() {
        assert!(parse("<a/>", true).is_err());
    }

    #[test]
    fn xaira_autoproc_and_fast_dp_use_fractions() {
        let a = parse(&fixture("xaira_autoPROC.xml"), true).unwrap();
        assert_eq!(a.space_group.as_deref(), Some("P 21 3"));
        let o = a.overall.unwrap();
        assert!((o.rmerge_pct.unwrap() - 11.5).abs() < 0.01);
        assert!((o.d_high.unwrap() - 0.807).abs() < 0.001);
        let f = parse(&fixture("xaira_fast_dp.xml"), true).unwrap();
        let o = f.overall.unwrap();
        assert!((o.rmerge_pct.unwrap() - 8.5).abs() < 0.01);
        assert!((o.cc_half.unwrap() - 0.999).abs() < 0.001);
        // ALBA's own autoPROC.xml (2025) is fractions too
        let old = parse(&fixture("autoproc_autoPROC.xml"), true).unwrap();
        assert!((old.overall.unwrap().rmerge_pct.unwrap() - 8.9).abs() < 0.01);
    }
}
