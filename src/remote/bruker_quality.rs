//! Data quality from the listings Bruker's programs leave in `work/` (docs/BRUKER.md §4.2c):
//! XPREP's `.prp` (statistics per resolution shell, in the space group chosen), SADABS's or
//! TWINABS's `.abs` (error model, R(int) per run) and SAINT's merged `_0m._ls` (coverage per
//! shell, before scaling). Each gives lines for the Info panel and, where it has them, the
//! overall and outer shells.

use crate::model::Shell;

/// What one listing says about the data: its table by resolution (for the Shells subtab), its
/// other figures (for the Scaling subtab) and the shells the results table shows.
#[derive(Debug, Default, PartialEq)]
pub struct Quality {
    pub table: Vec<String>,
    pub summary: Vec<String>,
    pub overall: Option<Shell>,
    pub inner: Option<Shell>,
    pub outer: Option<Shell>,
}

/// How much of SAINT's merged listing is read: its coverage table comes after the global
/// statistics (about 70 KB in); a per-run listing has it at the end and is not read.
pub const SAINT_LS_LIMIT: u64 = 160 * 1024;
/// `.prp` and `.abs` are small (7–20 KB; TWINABS's longer); a cap all the same.
pub const LISTING_LIMIT: u64 = 256 * 1024;

fn num(s: &str) -> Option<f64> {
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn pct(v: Option<f64>) -> Option<f64> {
    v.map(|v| v * 100.0)
}

fn f(v: Option<f64>, digits: usize) -> String {
    v.map_or_else(|| "–".into(), |v| format!("{v:.digits$}"))
}

// ------------------------------------------------------------------------------------- XPREP

/// One row of XPREP's table: `  Inf - 3.96     111    111    100.0     2.68     49.48   41.76  0.0234  0.0141`
/// (resolution range, #Data, #Theory, %Complete, Redundancy, Mean I, Mean I/s, Rmerge, Rsigma).
fn prp_row(line: &str) -> Option<(Shell, Option<f64>)> {
    let t: Vec<&str> = line.split_whitespace().collect();
    if t.len() != 11 || t[1] != "-" {
        return None;
    }
    let d_low = if t[0].eq_ignore_ascii_case("inf") { None } else { Some(num(t[0])?) };
    let shell = Shell {
        d_low,
        d_high: Some(num(t[2])?),
        n_unique: t[3].parse().ok(),
        completeness: num(t[5]),
        multiplicity: num(t[6]),
        i_sig: num(t[8]),
        rmerge_pct: pct(num(t[9])),
        ..Shell::default()
    };
    Some((shell, pct(num(t[10]))))
}

fn range(s: &Shell) -> String {
    let low = s.d_low.map_or_else(|| "∞".into(), |d| format!("{d:.2}"));
    format!("{low}–{}", f(s.d_high, 2))
}

/// XPREP's `INTENSITY STATISTICS FOR DATASET` table (the last one, if it ran more than once),
/// with the space group it was merged in.
pub fn parse_prp(name: &str, text: &str) -> Option<Quality> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().rposition(|l| l.trim_start().starts_with("INTENSITY STATISTICS FOR DATASET"))?;
    let mut shells = Vec::new();
    let mut summary = Vec::new();
    let mut after_rule = false;
    for l in &lines[start + 1..] {
        let t = l.trim();
        if t.starts_with("Resolution") || (t.is_empty() && shells.is_empty()) {
            continue;
        }
        if t.starts_with("---") {
            after_rule = true;
            continue;
        }
        match prp_row(t) {
            Some(row) if after_rule => summary.push(row),
            Some(row) => shells.push(row),
            None => break,
        }
    }
    if shells.is_empty() {
        return None;
    }
    // XPREP's two summary lines: the highest-resolution group, then everything
    let overall = summary.iter().find(|(s, _)| s.d_low.is_none()).or(summary.last()).cloned();
    let outer = summary.iter().find(|(s, _)| s.d_low.is_some()).cloned().or_else(|| shells.last().cloned());

    // `Option [A] chosen` names a row of the space-group table: `[A] C2/c   # 15 …`
    let chosen = lines[..start].iter().rev().find_map(|l| {
        let letter = l.trim().strip_prefix("Option [")?.split(']').next()?.to_string();
        l.contains("chosen").then_some(letter)
    });
    let sg = chosen.and_then(|letter| {
        let tag = format!("[{letter}]");
        lines[..start].iter().find_map(|l| l.trim().strip_prefix(tag.as_str())?.split_whitespace().next().map(String::from))
    });

    let row = |label: &str, (s, rsig): &(Shell, Option<f64>)| {
        format!(
            "{label:<8} {:<12} {:>6} {:>5} {:>6} {:>7} {:>7}",
            range(s),
            f(s.completeness, 1),
            f(s.multiplicity, 2),
            f(s.i_sig, 1),
            f(s.rmerge_pct, 1),
            f(*rsig, 1)
        )
    };
    let mut out = vec![
        match &sg {
            Some(g) => format!("XPREP ({name}), merged in {g}:"),
            None => format!("XPREP ({name}):"),
        },
        format!("{:<8} {:<12} {:>6} {:>5} {:>6} {:>7} {:>7}", "shell", "d (Å)", "Cmpl%", "Mult", "I/σ", "Rmerge%", "Rsigma%"),
    ];
    if let Some(o) = &overall {
        out.push(row("overall", o));
    }
    if let Some(o) = &outer {
        out.push(row("outer", o));
    }
    for s in &shells {
        out.push(row("", s));
    }
    Some(Quality {
        table: out,
        summary: Vec::new(),
        overall: overall.map(|(s, _)| s),
        inner: shells.first().map(|(s, _)| s.clone()),
        outer: outer.map(|(s, _)| s),
    })
}

// ----------------------------------------------------------------------------- SADABS/TWINABS

/// The number after `key` on a line: `g = 0.0636 gave best …` → 0.0636.
fn after(line: &str, key: &str) -> Option<f64> {
    num(line.split_once(key)?.1.split_whitespace().next()?)
}

/// SADABS's (or TWINABS's) last scaling: the error model, wR2(int) before and after the
/// refinement, rejections, transmission and the per-run table.
pub fn parse_abs(name: &str, text: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let program = lines
        .iter()
        .find_map(|l| {
            let t = l.trim();
            (t.starts_with("SADABS") || t.starts_with("TWINABS")).then(|| t.split_whitespace().next().unwrap_or(t).to_string())
        })
        .unwrap_or_else(|| "SADABS".into());
    let last = |key: &str| lines.iter().rev().find_map(|l| after(l, key));
    let wr2 = |when: &str| lines.iter().rev().find(|l| l.contains("wR2(int)") && l.contains(when)).and_then(|l| after(l, "wR2(int) ="));
    let g = last("g =").filter(|_| lines.iter().any(|l| l.contains("error model")));
    let ratio = last("data to parameter ratio =");
    // `11080 total and 5425 unique reflections assuming Friedel's law.` / `… left after |I-<I>|/su test`
    let totals = |key: &str| {
        lines.iter().rev().find(|l| l.contains(key) && l.contains("total and")).and_then(|l| l.split_whitespace().next()?.parse::<u64>().ok())
    };
    let (before, kept) = (totals("unique reflections assuming"), totals("left after"));
    let trans = lines.iter().rev().find_map(|l| {
        let rest = l.split_once("minimum and maximum transmission:")?.1;
        let mut v = rest.split_whitespace().filter_map(num);
        Some((v.next()?, v.next()?))
    });

    let mut first = Vec::new();
    if let Some(g) = g {
        first.push(format!("g = {g}"));
    }
    match (wr2("before"), wr2("after")) {
        (Some(b), Some(a)) => first.push(format!("wR2(int) {b:.4} → {a:.4}")),
        (None, Some(a)) => first.push(format!("wR2(int) {a:.4}")),
        _ => {}
    }
    if let Some(r) = ratio {
        first.push(format!("data/parameters {r}"));
    }
    let mut second = Vec::new();
    if let (Some(b), Some(k)) = (before, kept) {
        second.push(format!("{} of {b} reflections rejected as outliers", b.saturating_sub(k)));
    }
    if let Some((lo, hi)) = trans {
        second.push(format!("Tmin/Tmax {lo:.4}/{hi:.4} (ratio {:.3})", lo / hi));
    }

    // the last per-run table: `  Run 2-theta  R(int)  Incid. factors  Diffr. factors    K     Total I>2sig(I)`
    let mut runs = Vec::new();
    if let Some(head) = lines.iter().rposition(|l| l.contains("R(int)") && l.contains("Run") && l.contains("K")) {
        for l in &lines[head + 1..] {
            let t: Vec<&str> = l.split_whitespace().collect();
            // run 2θ R(int) a - b c - d K total strong
            if t.len() != 12 || t[4] != "-" || t[7] != "-" {
                break;
            }
            runs.push(format!("{:>4} {:>6} {:>7} {:>6} {:>6} {:>6}", t[0], t[1], t[2], t[9], t[10], t[11]));
        }
    }
    if first.is_empty() && second.is_empty() && runs.is_empty() {
        return None;
    }
    let mut out = vec![format!("{program} ({name}): {}", first.join(" · "))];
    if !second.is_empty() {
        out.push(second.join(" · "));
    }
    if !runs.is_empty() {
        out.push(format!("{:>4} {:>6} {:>7} {:>6} {:>6} {:>6}", "run", "2θ", "R(int)", "K", "refl", "I>2σ"));
        out.extend(runs);
    }
    Some(out)
}

// -------------------------------------------------------------------------------------- SAINT

/// SAINT's merged listing: its global statistics and the first coverage table (component 1 of
/// a twin), cumulative to each resolution limit.
pub fn parse_saint_ls(name: &str, text: &str) -> Option<Quality> {
    let lines: Vec<&str> = text.lines().collect();
    let value = |key: &str| lines.iter().find_map(|l| l.trim().strip_prefix(key)).map(str::trim);
    let spots = value("Good spots written:").and_then(|v| v.parse::<u64>().ok());
    let i_sig = value("Average I/sigma:").and_then(num);
    let weak = value("% less than 2 sigma:").and_then(num);
    let rms = value("RMS X,Y,Z positional errors:").map(|v| v.split_whitespace().collect::<Vec<_>>().join(" "));

    let start = lines.iter().position(|l| l.trim() == "Coverage Statistics");
    let component = start.and_then(|s| {
        lines[s..].iter().take(4).find_map(|l| {
            let c = l.split_once("):")?.1.trim();
            c.starts_with("component").then(|| c.split(" (").next().unwrap_or(c).to_string())
        })
    });
    let twin = text.contains("component 2 in sample");
    // the point group SAINT merged in: the first `Point group "-1"` of the statistics
    let pg = lines.iter().find_map(|l| l.trim().strip_prefix("Point group \"")?.split('"').next().map(String::from));

    // ` to  2.236   1056   1257  84.01   3.13  0.088   735  58.47  0.088  54.10 11.8`
    let mut rows = Vec::new();
    let mut table = Vec::new();
    if let Some(s) = start {
        for l in lines[s..].iter().skip_while(|l| !l.trim_start().starts_with("Angstrms")).skip(1) {
            let t: Vec<&str> = l.split_whitespace().collect();
            if t.len() != 12 || t[0] != "to" {
                break;
            }
            let v: Vec<Option<f64>> = t[1..].iter().map(|x| num(x)).collect();
            rows.push(Shell {
                d_high: v[0],
                n_obs: t[2].parse().ok(),
                completeness: v[3],
                multiplicity: v[4],
                rmerge_pct: pct(v[5]),
                i_sig: v[9],
                ..Shell::default()
            });
            table.push(format!(
                "{:>7} {:>6} {:>6} {:>6} {:>7} {:>6} {:>5}",
                f(v[0], 3),
                f(v[3], 1),
                f(v[4], 2),
                f(pct(v[5]), 1),
                f(pct(v[8]), 1),
                f(v[9], 1),
                f(v[10], 1)
            ));
        }
    }
    if rows.is_empty() && spots.is_none() && i_sig.is_none() {
        return None;
    }

    let mut what = vec!["before scaling".to_string()];
    if let Some(p) = &pg {
        what.push(format!("point group {p}"));
    }
    if twin && let Some(c) = &component {
        what.push(format!("twin, {c}"));
    }
    let mut out = vec![format!("SAINT ({name}), {}:", what.join(", "))];
    let mut summary = Vec::new();
    let mut figures = Vec::new();
    if let Some(n) = spots {
        summary.push(format!("{n} spots"));
    }
    if let Some(v) = i_sig {
        summary.push(format!("<I/σ> {v:.1}"));
    }
    if let Some(v) = weak {
        summary.push(format!("{v:.1} % below 2σ"));
    }
    if let Some(r) = rms {
        summary.push(format!("RMS XYZ errors {r} px"));
    }
    if !summary.is_empty() {
        figures.push(format!("SAINT ({name}):"));
        figures.push(summary.join(" · "));
    }
    if !table.is_empty() {
        out.push(format!("{:>7} {:>6} {:>6} {:>6} {:>7} {:>6} {:>5}", "to d(Å)", "Cmpl%", "Redund", "Rsym%", "Rshell%", "I/σ", "%<2σ"));
        out.extend(table);
        out.push("Cmpl%, Redund and Rsym% are cumulative to d; Rshell%, I/σ and %<2σ are of the shell.".into());
    } else {
        out.clear();
        if start.is_none() {
            figures.push("no coverage table in the part read".into());
        }
    }
    // overall: the last cumulative row, with SAINT's average I/σ
    let overall = rows.last().map(|r| Shell { d_low: None, i_sig: i_sig.or(r.i_sig), ..r.clone() });
    Some(Quality { table: out, summary: figures, overall, inner: rows.first().cloned(), outer: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRP: &str = include_str!("../../docs/server_samples/bruker_xprep.prp");
    const ABS: &str = include_str!("../../docs/server_samples/bruker_sadabs.abs");
    const LS: &str = include_str!("../../docs/server_samples/bruker_saint_0m_ls_excerpt.txt");

    #[test]
    fn xprep_statistics_per_shell() {
        let q = parse_prp("3286AS264_B_0m.prp", PRP).unwrap();
        assert_eq!(q.table[0], "XPREP (3286AS264_B_0m.prp), merged in C2/c:");
        assert!(q.summary.is_empty());
        let o = q.overall.unwrap();
        assert_eq!((o.d_low, o.d_high, o.completeness, o.multiplicity, o.i_sig), (None, Some(0.93), Some(94.9), Some(1.77), Some(4.91)));
        assert!((o.rmerge_pct.unwrap() - 9.0).abs() < 1e-9);
        let out = q.outer.unwrap();
        assert_eq!((out.d_low, out.d_high, out.completeness), (Some(1.03), Some(0.93), Some(87.1)));
        assert_eq!(q.inner.unwrap().d_high, Some(3.96));
        // header, column names, overall, outer, then the 21 shells
        assert_eq!(q.table.len(), 2 + 2 + 21);
        assert!(q.table[2].starts_with("overall  ∞–0.93") && q.table[2].ends_with("16.1"), "{:?}", q.table[2]);
        assert!(q.table[4].contains("∞–3.96") && q.table[4].contains("41.8"), "{:?}", q.table[4]);
        assert!(parse_prp("x.prp", "no table here").is_none());
    }

    #[test]
    fn sadabs_error_model_and_runs() {
        let l = parse_abs("3570JSG276.abs", ABS).unwrap();
        assert_eq!(l[0], "SADABS-2008/1 (3570JSG276.abs): g = 0.0636 · wR2(int) 0.0903 → 0.0723 · data/parameters 3.05");
        assert_eq!(l[1], "225 of 11080 reflections rejected as outliers · Tmin/Tmax 0.5156/0.7444 (ratio 0.693)");
        assert!(l[2].contains("R(int)") && l[2].contains("K"));
        assert_eq!(l[3].split_whitespace().collect::<Vec<_>>(), ["1", "14.0", "0.0566", "1.029", "4487", "3240"]);
        assert_eq!(l.len(), 3 + 4, "four runs");
        assert!(parse_abs("x.abs", "nothing").is_none());
    }

    #[test]
    fn saint_coverage_of_the_first_component() {
        let q = parse_saint_ls("3570JSG276_0m._ls", LS).unwrap();
        assert_eq!(q.table[0], "SAINT (3570JSG276_0m._ls), before scaling, point group -1, twin, component 1 in sample 1:");
        assert_eq!(q.summary, ["SAINT (3570JSG276_0m._ls):", "30780 spots · <I/σ> 15.3 · 36.6 % below 2σ · RMS XYZ errors 0.69 0.60 0.44 px"]);
        // title, header row, ten shells (component 1's, not component 2's), the explanation
        assert_eq!(q.table.len(), 1 + 1 + 10 + 1);
        assert_eq!(q.table[2].split_whitespace().collect::<Vec<_>>(), ["2.236", "84.0", "3.13", "8.8", "8.8", "54.1", "11.8"]);
        let o = q.overall.unwrap();
        assert_eq!((o.d_high, o.completeness, o.multiplicity, o.i_sig), (Some(1.038), Some(43.11), Some(2.04), Some(15.305)));
        // a listing cut before its table still says what it read
        let head: String = LS.lines().take(57).collect::<Vec<_>>().join("\n");
        let q = parse_saint_ls("x_0m._ls", &head).unwrap();
        assert!(q.table.is_empty());
        assert_eq!(q.summary.last().unwrap(), "no coverage table in the part read");
        assert!(parse_saint_ls("x._ls", "SAINT V8").is_none());
    }
}
