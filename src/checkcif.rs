//! checkCIF-style alerts from a solve's merging statistics, so a dataset that will draw
//! A/B/C alerts shows up while you can still recollect.
//!
//! Three IUCr/PLATON tests can already be judged from the pipeline's overall shell:
//! - THETM01, resolution: sin θ/λ = 1/(2·d_min) below 0.590 (C), 0.575 (B), 0.550 (A).
//! - PLAT029, completeness: below 99 % (C), 97 % (B), 95 % (A). checkCIF tests the fraction
//!   to θ_full and doesn't publish its limits; these are read off real reports. The
//!   pipeline's completeness runs to d_min, so it is the stricter of the two.
//! - RINTA01, Rint: above 10 % (C), 15 % (B), 20 % (A), compared with the overall Rmerge
//!   (the same sum over equivalents), or Rmeas when the pipeline reports no Rmerge.
//!
//! The limits live in `config.toml` (`[checkcif]`), each list for levels C, B, A.

use crate::model::Shell;
use crate::rank;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    C,
    B,
    A,
}

impl Level {
    pub fn letter(self) -> &'static str {
        match self {
            Level::C => "C",
            Level::B => "B",
            Level::A => "A",
        }
    }
}

/// Alert limits, each listed for levels C, B, A.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// THETM01: sin θ/λ in Å⁻¹, alert below.
    pub sin_theta_lambda: [f64; 3],
    /// PLAT029: completeness in %, alert below.
    pub completeness: [f64; 3],
    /// RINTA01: Rint in %, alert above.
    pub rint: [f64; 3],
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            sin_theta_lambda: [0.590, 0.575, 0.550],
            completeness: [99.0, 97.0, 95.0],
            rint: [10.0, 15.0, 20.0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    Resolution,
    Completeness,
    /// `rmeas`: the pipeline gave no Rmerge, so Rmeas stood in.
    Rint { rmeas: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Alert {
    pub check: Check,
    pub level: Level,
    /// sin θ/λ, completeness % or R %.
    pub value: f64,
    pub limit: f64,
}

impl Alert {
    /// `d_min 0.94 Å = sin θ/λ 0.532 < 0.550`, `Cmpl 96.2 % < 97` …
    pub fn text(&self) -> String {
        match self.check {
            Check::Resolution => format!(
                "d_min {:.2} Å = sin θ/λ {:.3} < {:.3}",
                0.5 / self.value,
                self.value,
                self.limit
            ),
            Check::Completeness => format!("Cmpl {:.1} % < {}", self.value, self.limit),
            Check::Rint { rmeas } => format!(
                "{} {:.1} % > {} (Rint)",
                if rmeas { "Rmeas" } else { "Rmerge" },
                self.value,
                self.limit
            ),
        }
    }
}

/// Level of the strictest limit `v` falls below (`low_is_bad`) or above.
fn level(v: f64, limits: [f64; 3], low_is_bad: bool) -> Option<(Level, f64)> {
    // Most severe limit first, whatever order the config lists them in.
    let mut l = limits;
    l.sort_by(|a, b| if low_is_bad { a.total_cmp(b) } else { b.total_cmp(a) });
    let hit = |lim: f64| if low_is_bad { v < lim } else { v > lim };
    [Level::A, Level::B, Level::C]
        .into_iter()
        .zip(l)
        .find(|&(_, lim)| hit(lim))
}

/// The alerts an overall shell would draw, most severe first.
pub fn check(overall: &Shell, limits: &Limits) -> Vec<Alert> {
    let rmeas = overall.rmerge_pct.is_none();
    let tests = [
        (Check::Resolution, overall.d_high.filter(|d| *d > 0.0).map(|d| 0.5 / d), limits.sin_theta_lambda, true),
        (Check::Completeness, overall.completeness, limits.completeness, true),
        (Check::Rint { rmeas }, rank::r_inner(overall), limits.rint, false),
    ];
    let mut out: Vec<Alert> = tests
        .into_iter()
        .filter_map(|(check, v, lim, low_is_bad)| {
            let v = v?;
            let (level, limit) = level(v, lim, low_is_bad)?;
            Some(Alert { check, level, value: v, limit })
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.level));
    out
}

/// Level of the alert of one kind (`Check::Rint` matches either R).
pub fn level_of(alerts: &[Alert], check: Check) -> Option<Level> {
    alerts
        .iter()
        .find(|a| match (a.check, check) {
            (Check::Rint { .. }, Check::Rint { .. }) => true,
            (x, y) => x == y,
        })
        .map(|a| a.level)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(d: f64, cmpl: f64, rmerge: Option<f64>, rmeas: Option<f64>) -> Shell {
        Shell {
            d_high: Some(d),
            completeness: Some(cmpl),
            rmerge_pct: rmerge,
            rmeas_pct: rmeas,
            ..Shell::default()
        }
    }

    #[test]
    fn levels_follow_the_checkcif_limits() {
        let l = Limits::default();
        // 0.80 Å, complete, low R: nothing
        assert!(check(&shell(0.80, 99.9, Some(5.0), None), &l).is_empty());
        // sin θ/λ: 0.84 Å → 0.595 (none), 0.86 → 0.581 (C), 0.88 → 0.568 (B), 0.94 → 0.532 (A)
        let res = |d| level_of(&check(&shell(d, 100.0, Some(5.0), None), &l), Check::Resolution);
        assert_eq!(res(0.84), None);
        assert_eq!(res(0.86), Some(Level::C));
        assert_eq!(res(0.88), Some(Level::B));
        assert_eq!(res(0.94), Some(Level::A));
        let cmpl = |c| level_of(&check(&shell(0.8, c, Some(5.0), None), &l), Check::Completeness);
        assert_eq!(cmpl(99.0), None);
        assert_eq!(cmpl(98.9), Some(Level::C));
        assert_eq!(cmpl(96.0), Some(Level::B));
        assert_eq!(cmpl(91.0), Some(Level::A));
        let rint = |r| level_of(&check(&shell(0.8, 100.0, Some(r), None), &l), Check::Rint { rmeas: false });
        assert_eq!(rint(10.0), None);
        assert_eq!(rint(12.0), Some(Level::C));
        assert_eq!(rint(17.0), Some(Level::B));
        assert_eq!(rint(25.0), Some(Level::A));
    }

    #[test]
    fn most_severe_first_and_rmeas_stands_in() {
        let a = check(&shell(0.86, 91.0, None, Some(12.0)), &Limits::default());
        assert_eq!(a.iter().map(|a| a.level).collect::<Vec<_>>(), [Level::A, Level::C, Level::C]);
        let r = a.iter().find(|a| matches!(a.check, Check::Rint { .. })).unwrap();
        assert_eq!(r.check, Check::Rint { rmeas: true });
        assert_eq!(r.text(), "Rmeas 12.0 % > 10 (Rint)");
        let d = a.iter().find(|a| a.check == Check::Resolution).unwrap();
        assert_eq!(d.text(), "d_min 0.86 Å = sin θ/λ 0.581 < 0.590");
    }

    #[test]
    fn limits_in_any_order_and_from_toml() {
        let l: Limits = toml::from_str("completeness = [90.0, 98.0, 95.0]").unwrap();
        assert_eq!(l.rint, Limits::default().rint);
        let cmpl = |c| level_of(&check(&shell(0.8, c, None, None), &l), Check::Completeness);
        assert_eq!(cmpl(97.0), Some(Level::C));
        assert_eq!(cmpl(94.0), Some(Level::B));
        assert_eq!(cmpl(89.0), Some(Level::A));
    }
}
