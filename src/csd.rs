//! CellCheckCSD, CCDC's free offline cell check against the CSD (DESIGN.md §2.18).
//!
//! `ccdc_searcher.exe -query q.xml -database cell_check.csdsql -results r.xml` searches
//! ~1.8 million CSD and ICSD cells in well under a second (0.08 s with start-up). The query
//! is the conventional cell and its centring letter; the results list every entry whose
//! reduced cell matches within CellCheckCSD's own tolerances, sorted by identifier, which
//! amanece re-ranks with its own lattice comparison.
//!
//! Licence: CellCheckCSD may not be bundled or redistributed, so amanece only runs a copy
//! the user installed and keeps nothing but the hits it shows.

use crate::cells;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Where to get it (free after registration). amanece never downloads or ships it.
pub const DOWNLOAD: &str = "https://www.ccdc.cam.ac.uk/solutions/software/cellcheckcsd/";
/// A search that takes longer than this is killed (a normal one takes 0.1 s).
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq)]
pub struct Install {
    pub dir: PathBuf,
    /// Build date of the program and its data, `2026-11-10`, from `version.info`.
    pub built: Option<String>,
}

#[cfg(windows)]
const EXE: &str = "ccdc_searcher.exe";
#[cfg(not(windows))]
const EXE: &str = "bin/ccdc_searcher";
const DATABASE: &str = "cell_check.csdsql";

impl Install {
    /// `dir` from the config, else the default install folder; `None` unless the program
    /// and its database are both there.
    pub fn find(dir: Option<&Path>) -> Option<Install> {
        if cfg!(test) && dir.is_none() {
            return None;
        }
        let dir = dir.map(Path::to_path_buf).or_else(default_dir)?;
        Install::at(&dir)
    }

    fn at(dir: &Path) -> Option<Install> {
        if !dir.join(EXE).is_file() || !dir.join(DATABASE).is_file() {
            return None;
        }
        let built = std::fs::read_to_string(dir.join("version.info"))
            .ok()
            .and_then(|s| s.get(..10).map(str::to_string))
            .filter(|d| d.chars().all(|c| c.is_ascii_digit() || c == '-'));
        Some(Install { dir: dir.to_path_buf(), built })
    }

    /// Runs one search. `work` is a folder for the query and results files (the config
    /// dir); both are removed afterwards. Blocking: call it off the UI thread.
    pub fn search(&self, q: &Query, work: &Path) -> Result<Outcome> {
        let stamp = format!("{}_{}", std::process::id(), q.id);
        let query = work.join(format!("csd-query-{stamp}.xml"));
        let results = work.join(format!("csd-results-{stamp}.xml"));
        std::fs::write(&query, q.xml()).with_context(|| format!("writing {}", query.display()))?;
        let out = self.run(&query, &results);
        let _ = std::fs::remove_file(&query);
        let text = out.and_then(|_| {
            std::fs::read_to_string(&results).with_context(|| format!("reading {}", results.display()))
        });
        let _ = std::fs::remove_file(&results);
        parse(&text?)
    }

    fn run(&self, query: &Path, results: &Path) -> Result<()> {
        let mut cmd = Command::new(self.dir.join(EXE));
        cmd.arg("-query")
            .arg(query)
            .arg("-database")
            .arg(self.dir.join(DATABASE))
            .arg("-results")
            .arg(results)
            .current_dir(&self.dir)
            // its banner and INFO lines would draw over the TUI
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().context("starting ccdc_searcher")?;
        let start = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if start.elapsed() > TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                bail!("ccdc_searcher took over {} s and was stopped", TIMEOUT.as_secs());
            }
            std::thread::sleep(Duration::from_millis(15));
        };
        if !status.success() {
            let mut msg = String::new();
            if let Some(mut o) = child.stdout.take() {
                use std::io::Read;
                let _ = o.read_to_string(&mut msg);
            }
            let fatal = msg.lines().find(|l| l.contains("FATAL")).unwrap_or("no message").trim().to_string();
            bail!("ccdc_searcher failed: {fatal}");
        }
        Ok(())
    }
}

fn default_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let pf = std::env::var_os("ProgramFiles").map(PathBuf::from).unwrap_or_else(|| r"C:\Program Files".into());
        Some(pf.join("CCDC").join("CellCheckCSD"))
    }
    #[cfg(not(windows))]
    {
        Some(PathBuf::from("/opt/CCDC/CellCheckCSD"))
    }
}

/// One search: a conventional cell, its centring and the tolerances.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// Unique per search (names the temp files).
    pub id: u64,
    pub cell: [f64; 6],
    /// P A B C F I R, as `cells::centring` gives it.
    pub centring: char,
    /// % of the longest edge.
    pub length_pct: f64,
    /// Absolute degrees.
    pub angle_deg: f64,
}

impl Query {
    pub fn xml(&self) -> String {
        let [a, b, c, al, be, ga] = self.cell;
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <query name=\"reduced_cell_search\" version=\"1.0\" originator=\"generic\">\n\
             \x20 <lattice_centring>{}</lattice_centring>\n\
             \x20 <a>{a:.4}</a><b>{b:.4}</b><c>{c:.4}</c>\n\
             \x20 <alpha>{al:.3}</alpha><beta>{be:.3}</beta><gamma>{ga:.3}</gamma>\n\
             \x20 <settings>\n\
             \x20   <dimension_tolerance>{:.2}</dimension_tolerance>\n\
             \x20   <angle_tolerance>{:.2}</angle_tolerance>\n\
             \x20   <maximum_hits>0</maximum_hits>\n\
             \x20 </settings>\n\
             </query>\n",
            self.centring, self.length_pct, self.angle_deg
        )
    }
}

/// One matching entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// CSD refcode (`EPULIL`), or an ICSD number (`1137`).
    pub id: String,
    pub cell: [f64; 6],
    /// `P21/n`, no spaces.
    pub sg: String,
    /// CSD moiety style: `C23 H39 Cl2 N3 Ru1`, `(Cl1 K1 O4)n`, `C6 Cr1 O12 3-,4.5(C3 H7 N1 O1)`.
    pub formula: String,
    /// Closeness to the query in amanece's own terms (see `rank`), 100 = same cell.
    pub sim: f64,
}

impl Hit {
    /// ICSD entries have numbers for identifiers; CSD ones refcodes.
    pub fn is_icsd(&self) -> bool {
        self.id.chars().all(|c| c.is_ascii_digit())
    }

    /// The CCDC's free "Access Structures" page for a CSD refcode.
    pub fn url(&self) -> Option<String> {
        (!self.is_icsd()).then(|| format!("https://www.ccdc.cam.ac.uk/structures/Search?Ccdcid={}", self.id))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub hits: Vec<Hit>,
    /// Entries searched (1 805 887 in the 2026-11 data).
    pub size: Option<u64>,
}

/// Reads a results file.
pub fn parse(xml: &str) -> Result<Outcome> {
    let doc = roxmltree::Document::parse(xml).context("CellCheckCSD results are not XML")?;
    let root = doc.root_element();
    if root.tag_name().name() != "search_results" {
        bail!("unexpected CellCheckCSD results ({})", root.tag_name().name());
    }
    let size = root
        .descendants()
        .find(|n| n.has_tag_name("size"))
        .and_then(|n| n.text())
        .and_then(|t| t.trim().parse().ok());
    let mut hits = Vec::new();
    for m in root.children().filter(|n| n.has_tag_name("match")) {
        let Some(id) = m.attribute("identifier") else { continue };
        let param = |t: &str| {
            m.descendants()
                .find(|n| n.has_tag_name("parameter") && n.attribute("type") == Some(t))
                .and_then(|n| n.text())
                .map(|s| s.trim().to_string())
        };
        let num = |t: &str| param(t).and_then(|s| s.parse::<f64>().ok());
        let (Some(a), Some(b), Some(c), Some(al), Some(be), Some(ga)) = (
            num("cell_length_a"),
            num("cell_length_b"),
            num("cell_length_c"),
            num("cell_angle_alpha"),
            num("cell_angle_beta"),
            num("cell_angle_gamma"),
        ) else {
            continue;
        };
        hits.push(Hit {
            id: id.to_string(),
            cell: [a, b, c, al, be, ga],
            sg: param("space_group").unwrap_or_default(),
            formula: param("chemical_formula").unwrap_or_default(),
            sim: 0.0,
        });
    }
    Ok(Outcome { hits, size })
}

/// Scores each hit against the query cell with amanece's lattice comparison (so `sim`
/// means what it means for similar cells) and sorts closest first. CellCheckCSD's edge
/// tolerance is a share of the longest edge, looser than ours on short edges, so the
/// comparison here allows 3× ours and `sim` can drop below the similar-cells floor.
pub fn rank(out: &mut Outcome, cell: [f64; 6], sg: Option<&str>, tol: cells::Tolerance) {
    let loose = cells::Tolerance { length_pct: 3.0 * tol.length_pct, angle_deg: 3.0 * tol.angle_deg };
    let me = cells::reduce(cell, sg);
    for h in &mut out.hits {
        let them = cells::reduce(h.cell, Some(&h.sg));
        let score = me.zip(them).and_then(|(p, q)| cells::same_lattice(&p, &q, loose));
        h.sim = score.map_or(0.0, |s| (100.0 - s * loose.length_pct).max(0.0));
    }
    out.hits.sort_by(|a, b| b.sim.total_cmp(&a.sim).then_with(|| a.id.cmp(&b.id)));
}

/// The hits whose formula contains every element of `required` (all of them when it is
/// empty), in ranked order. A hit may have more elements (H, solvent, counter-ions).
pub fn filter<'a>(hits: &'a [Hit], required: &[String]) -> Vec<&'a Hit> {
    if required.is_empty() {
        return hits.iter().collect();
    }
    hits.iter()
        .filter(|h| {
            let have = elements(&h.formula);
            required.iter().all(|e| have.contains(e))
        })
        .collect()
}

/// Element symbols, for checking what is typed into the filter.
const SYMBOLS: [&str; 118] = [
    "H", "He", "Li", "Be", "B", "C", "N", "O", "F", "Ne", "Na", "Mg", "Al", "Si", "P", "S", "Cl", "Ar", "K", "Ca",
    "Sc", "Ti", "V", "Cr", "Mn", "Fe", "Co", "Ni", "Cu", "Zn", "Ga", "Ge", "As", "Se", "Br", "Kr", "Rb", "Sr", "Y",
    "Zr", "Nb", "Mo", "Tc", "Ru", "Rh", "Pd", "Ag", "Cd", "In", "Sn", "Sb", "Te", "I", "Xe", "Cs", "Ba", "La", "Ce",
    "Pr", "Nd", "Pm", "Sm", "Eu", "Gd", "Tb", "Dy", "Ho", "Er", "Tm", "Yb", "Lu", "Hf", "Ta", "W", "Re", "Os", "Ir",
    "Pt", "Au", "Hg", "Tl", "Pb", "Bi", "Po", "At", "Rn", "Fr", "Ra", "Ac", "Th", "Pa", "U", "Np", "Pu", "Am", "Cm",
    "Bk", "Cf", "Es", "Fm", "Md", "No", "Lr", "Rf", "Db", "Sg", "Bh", "Hs", "Mt", "Ds", "Rg", "Cn", "Nh", "Fl", "Mc",
    "Lv", "Ts", "Og",
];

/// What was typed into the elements field → (elements, sorted, no repeats; words that
/// aren't elements). Spaces, commas or nothing between symbols: `Cu N O`, `Cu,N,O`,
/// `CuNO`; any case for separated symbols (`cu n o`).
pub fn parse_elements(input: &str) -> (Vec<String>, Vec<String>) {
    let mut ok: Vec<String> = Vec::new();
    let mut bad = Vec::new();
    for word in input.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| !w.is_empty()) {
        let one = |w: &str| {
            let mut s = w[..1].to_ascii_uppercase();
            s.push_str(&w[1..].to_ascii_lowercase());
            SYMBOLS.contains(&s.as_str()).then_some(s)
        };
        // a lone symbol in any case, else run-together symbols in their proper case
        let found = match one(word) {
            Some(s) => vec![s],
            None if word.chars().any(|c| c.is_ascii_uppercase()) => {
                let parts = elements(word);
                let joined: usize = parts.iter().map(String::len).sum();
                if joined == word.len() && parts.iter().all(|p| SYMBOLS.contains(&p.as_str())) {
                    parts
                } else {
                    bad.push(word.to_string());
                    Vec::new()
                }
            }
            None => {
                bad.push(word.to_string());
                Vec::new()
            }
        };
        for s in found {
            if !ok.contains(&s) {
                ok.push(s);
            }
        }
    }
    ok.sort();
    (ok, bad)
}

/// The elements of a formula, sorted: `C6 Cr1 O12 3-,4.5(C3 H7 N1 O1)` → C Cr H N O.
pub fn elements(formula: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let b: Vec<char> = formula.chars().collect();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_uppercase() {
            let mut el = b[i].to_string();
            if i + 1 < b.len() && b[i + 1].is_ascii_lowercase() {
                el.push(b[i + 1]);
                i += 1;
            }
            if !out.contains(&el) {
                out.push(el);
            }
        }
        i += 1;
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULTS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<search_results version="1.0">
  <database>
    <size>1805887</size>
    <name>cell_check</name>
  </database>
  <status_report type="progress">0</status_report>
  <match identifier="1137">
    <parameters>
      <parameter type="cell_angle_alpha">90.000000</parameter>
      <parameter type="cell_angle_beta">90.000000</parameter>
      <parameter type="cell_angle_gamma">90.000000</parameter>
      <parameter type="cell_length_a">8.866</parameter>
      <parameter type="cell_length_b">5.666</parameter>
      <parameter type="cell_length_c">7.254</parameter>
      <parameter type="chemical_formula">Cl1 K1 O4</parameter>
      <parameter type="source_database">cell_check</parameter>
      <parameter type="space_group">Pnma</parameter>
    </parameters>
  </match>
  <match identifier="HUDYOU">
    <parameters>
      <parameter type="cell_angle_alpha">90.000000</parameter>
      <parameter type="cell_angle_beta">90.000000</parameter>
      <parameter type="cell_angle_gamma">90.000000</parameter>
      <parameter type="cell_length_a">7.1978</parameter>
      <parameter type="cell_length_b">8.6893</parameter>
      <parameter type="cell_length_c">5.7266</parameter>
      <parameter type="chemical_formula">C4 H12 N1 1+,N1 O3 1-</parameter>
      <parameter type="source_database">cell_check</parameter>
      <parameter type="space_group">Pmmn</parameter>
    </parameters>
  </match>
  <match identifier="170089">
    <parameters>
      <parameter type="cell_angle_alpha">90.000000</parameter>
      <parameter type="cell_angle_beta">90.000000</parameter>
      <parameter type="cell_angle_gamma">90.000000</parameter>
      <parameter type="cell_length_a">8.7684</parameter>
      <parameter type="cell_length_b">5.6237</parameter>
      <parameter type="cell_length_c">7.2039</parameter>
      <parameter type="chemical_formula">Cl1 K1 O4</parameter>
      <parameter type="source_database">cell_check</parameter>
      <parameter type="space_group">Pnma</parameter>
    </parameters>
  </match>
  <status_report type="progress">100</status_report>
  <status_report type="total_candidates_searched">1805887</status_report>
  <status_report type="total_search_time">0.02</status_report>
</search_results>"#;

    #[test]
    fn query_xml_has_the_schema_fields() {
        let q = Query { id: 1, cell: [8.77, 5.62, 7.21, 90.0, 90.0, 90.0], centring: 'P', length_pct: 1.5, angle_deg: 2.0 };
        let x = q.xml();
        for s in [
            "<query name=\"reduced_cell_search\"",
            "<lattice_centring>P</lattice_centring>",
            "<a>8.7700</a>",
            "<gamma>90.000</gamma>",
            "<dimension_tolerance>1.50</dimension_tolerance>",
            "<angle_tolerance>2.00</angle_tolerance>",
            "<maximum_hits>0</maximum_hits>",
        ] {
            assert!(x.contains(s), "{s}\n{x}");
        }
        assert!(roxmltree::Document::parse(&x).is_ok());
    }

    #[test]
    fn parses_and_ranks_results() {
        let mut out = parse(RESULTS).unwrap();
        assert_eq!(out.size, Some(1_805_887));
        assert_eq!(out.hits.len(), 3);
        assert_eq!(out.hits[0].id, "1137");
        assert_eq!(out.hits[0].sg, "Pnma");
        assert_eq!(out.hits[0].formula, "Cl1 K1 O4");
        assert!(out.hits[0].is_icsd() && !out.hits[1].is_icsd());
        assert_eq!(out.hits[1].url().unwrap(), "https://www.ccdc.cam.ac.uk/structures/Search?Ccdcid=HUDYOU");
        // AS571's cell (8.77 5.62 7.21): ICSD 170089 is nearly identical, so it comes first
        let tol = cells::Tolerance { length_pct: 1.5, angle_deg: 2.0 };
        rank(&mut out, [8.77, 5.62, 7.21, 90.0, 90.0, 90.0], Some("P n m a"), tol);
        assert_eq!(out.hits[0].id, "170089");
        assert!(out.hits[0].sim > 99.5, "{:?}", out.hits[0]);
        assert!(out.hits.windows(2).all(|w| w[0].sim >= w[1].sim));
    }

    #[test]
    fn filter_keeps_hits_with_every_element() {
        let out = parse(RESULTS).unwrap();
        let ids = |req: &[&str]| {
            let req: Vec<String> = req.iter().map(|s| s.to_string()).collect();
            filter(&out.hits, &req).iter().map(|h| h.id.clone()).collect::<Vec<_>>()
        };
        assert_eq!(ids(&[]).len(), 3);
        assert_eq!(ids(&["Cl", "K"]), ["1137", "170089"]);
        // HUDYOU is C4 H12 N1 1+,N1 O3 1-: more elements than asked is fine
        assert_eq!(ids(&["N", "O"]), ["HUDYOU"]);
        assert_eq!(ids(&["O"]).len(), 3);
        assert!(ids(&["Cu"]).is_empty());
    }

    #[test]
    fn typed_elements() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(parse_elements("Cu N O"), (s(&["Cu", "N", "O"]), vec![]));
        assert_eq!(parse_elements("cu, n,o"), (s(&["Cu", "N", "O"]), vec![]));
        assert_eq!(parse_elements("CuNO"), (s(&["Cu", "N", "O"]), vec![]));
        assert_eq!(parse_elements("ClKO4"), (s(&["Cl", "K", "O"]), vec![]));
        assert_eq!(parse_elements("Zn zn ZN"), (s(&["Zn"]), vec![]));
        assert_eq!(parse_elements("Cu Xx abc"), (s(&["Cu"]), s(&["Xx", "abc"])));
        assert_eq!(parse_elements("  "), (vec![], vec![]));
    }

    #[test]
    fn not_results_is_an_error() {
        assert!(parse("garbage").is_err());
        assert!(parse("<query/>").is_err());
    }

    #[test]
    fn formula_elements() {
        assert_eq!(elements("C6 Cr1 O12 3-,4.5(C3 H7 N1 O1),Cl1 1-"), ["C", "Cl", "Cr", "H", "N", "O"]);
        assert_eq!(elements("(Cl1 K1 O4)n"), ["Cl", "K", "O"]);
        assert_eq!(elements("Cl0.948 K0.974 O4"), ["Cl", "K", "O"]);
    }

    /// Runs the real program when it's installed (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn live_search_finds_potassium_perchlorate() {
        let install = Install::at(&default_dir().unwrap()).expect("CellCheckCSD installed");
        let q = Query { id: 7, cell: [8.77, 5.62, 7.21, 90.0, 90.0, 90.0], centring: 'P', length_pct: 1.5, angle_deg: 2.0 };
        let mut out = install.search(&q, &std::env::temp_dir()).unwrap();
        assert!(out.size.unwrap_or(0) > 1_000_000);
        rank(&mut out, q.cell, Some("P n m a"), cells::Tolerance { length_pct: 1.5, angle_deg: 2.0 });
        let kclo4 = filter(&out.hits, &["Cl".into(), "K".into(), "O".into()]);
        println!("{} hits, {} with Cl K O: {:?}", out.hits.len(), kclo4.len(), kclo4.iter().map(|h| &h.id).collect::<Vec<_>>());
        assert!(kclo4.len() >= 5 && kclo4.iter().all(|h| elements(&h.formula) == ["Cl", "K", "O"]));
        // a bad centring letter is reported, not a crash
        let bad = Query { centring: 'X', ..q };
        assert!(install.search(&bad, &std::env::temp_dir()).is_err());
    }
}
