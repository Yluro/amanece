//! Opt-in archive reader. Listings and bounded metadata reads only; no remote writes.
//! Each metadata file is a separate sample, so unrelated cells never get merged.
use super::{scan::Experiments, sftp::ReadOnlySftp};
use crate::model::{RemoteFile, Run, SampleInfo, Solve, SolveKind, Stats, Status, now_secs};
use anyhow::{Result, bail, ensure};
use sha2::{Digest, Sha256};

const HEADER_BYTES: u64 = 64 * 1024;

pub fn validate_root(root: &str) -> Result<()> {
    ensure!(
        root.starts_with('/') && root.trim_matches('/').len() > 0,
        "NAS mode requires an absolute remote_root below / (the SFTP path, not the NAS filesystem path)"
    );
    validate_relative(root.strip_prefix('/').unwrap_or(root).trim_end_matches('/'))
}

pub fn validate_relative(path: &str) -> Result<()> {
    ensure!(
        !path.starts_with('/')
            && !path.contains('\\')
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|p| p != ".." && p != "." && (!p.is_empty() || path.is_empty())),
        "invalid archive path"
    );
    Ok(())
}

pub fn source_id(host: &str, port: u16, root: &str) -> String {
    let digest = Sha256::digest(format!("{host}:{port}:{root}").as_bytes());
    format!("nas-{:x}", digest)[..20].to_string()
}

fn metadata(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".ins")
        || name.ends_with(".res")
        || name == "xds_ascii.hkl"
        || name == "ep_xds_ascii.hkl"
}

fn visible(name: &str) -> bool {
    !name.starts_with(['.', '@', '#']) && validate_relative(name).is_ok() && !name.contains('/')
}

fn group_path(sftp: &ReadOnlySftp, date: &str) -> Result<String> {
    let id = sftp
        .nas_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("not a NAS session"))?;
    if date == id {
        return Ok(String::new());
    }
    let group = date
        .strip_prefix(&format!("{id}/"))
        .ok_or_else(|| anyhow::anyhow!("wrong NAS source"))?;
    validate_relative(group)?;
    ensure!(
        !group.is_empty() && group.split('/').all(visible),
        "invalid NAS group"
    );
    Ok(format!("/{group}"))
}

pub async fn list_experiments(sftp: &ReadOnlySftp) -> Result<Experiments> {
    let top = sftp.list("/").await?;
    let id = sftp.nas_id.as_ref().expect("NAS mode");
    let mut ids: Vec<String> = top
        .iter()
        .filter(|e| e.is_dir && !e.is_symlink && visible(&e.name))
        .map(|e| format!("{id}/{}", e.name))
        .collect();
    // Root files are a separate, non-recursive group, not duplicated in subgroups.
    if top
        .iter()
        .any(|e| !e.is_dir && !e.is_symlink && metadata(&e.name))
    {
        ids.push(id.clone());
    }
    Ok(Experiments {
        ids,
        top: top.into_iter().map(|e| e.name).collect(),
    })
}

pub async fn list_samples(sftp: &ReadOnlySftp, date: &str) -> Result<(Vec<String>, bool)> {
    let base = group_path(sftp, date)?;
    let entries = sftp.list(if base.is_empty() { "/" } else { &base }).await?;
    let mut samples = Vec::new();
    for e in entries {
        if e.is_symlink || !visible(&e.name) {
            continue;
        }
        if e.is_dir {
            samples.push(format!("{}/", e.name));
        } else if metadata(&e.name) {
            samples.push(e.name);
        }
    }
    samples.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
    Ok((samples, false))
}

pub async fn scan_sample(sftp: &ReadOnlySftp, date: &str, sample: &str) -> Result<SampleInfo> {
    validate_relative(sample)?;
    ensure!(
        metadata(sample.rsplit('/').next().unwrap_or(sample)),
        "unsupported NAS metadata file"
    );
    let path = format!("{}/{sample}", group_path(sftp, date)?);
    let bytes = sftp.read_prefix(&path, HEADER_BYTES).await?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("metadata is not UTF-8: {path}"))?;
    let stats = parse_header(sample, text)?;
    let mut solve = Solve::new(SolveKind::Archive, &path, 1);
    solve.status = Status::Done;
    solve.stats = Some(stats);
    solve.note = Some("Archive metadata only; no processing quality score. SHELX LATT is centring, not a full space group.".into());
    solve.hkl.push(RemoteFile::new(&path, None));
    // Companion reflections only; never discover/download image frames here.
    if sample.to_ascii_lowercase().ends_with(".ins")
        || sample.to_ascii_lowercase().ends_with(".res")
    {
        let (parent, name) = path.rsplit_once('/').expect("absolute path");
        let stem = &name[..name.len() - 4];
        for e in sftp
            .list(if parent.is_empty() { "/" } else { parent })
            .await?
        {
            if !e.is_dir
                && !e.is_symlink
                && visible(&e.name)
                && e.name.eq_ignore_ascii_case(&format!("{stem}.hkl"))
            {
                solve
                    .hkl
                    .push(RemoteFile::new(format!("{parent}/{}", e.name), e.size));
            }
        }
    }
    Ok(SampleInfo {
        name: sample.to_string(),
        scanned_at: now_secs(),
        runs: vec![Run {
            prefix: sample.rsplit('/').next().unwrap_or(sample).to_string(),
            run: 1,
            characterisation: false,
            solves: vec![solve],
        }],
    })
}

fn valid_cell(c: [f64; 6]) -> bool {
    if !c.iter().all(|v| v.is_finite())
        || !c[..3].iter().all(|v| *v > 0.)
        || !c[3..].iter().all(|v| *v > 0. && *v < 180.)
    {
        return false;
    }
    let [a, b, g] = [
        c[3].to_radians().cos(),
        c[4].to_radians().cos(),
        c[5].to_radians().cos(),
    ];
    1. + 2. * a * b * g - a * a - b * b - g * g > 1e-12
}

pub fn parse_header(name: &str, text: &str) -> Result<Stats> {
    let xds = name.to_ascii_lowercase().ends_with(".hkl");
    let mut cell = None;
    let mut sg = None;
    for line in text.lines() {
        let line = line.trim();
        if xds {
            if line == "!END_OF_HEADER" {
                break;
            }
            if let Some(value) = line.strip_prefix("!UNIT_CELL_CONSTANTS=") {
                cell = parse_cell(value.split_whitespace());
            }
            if let Some(value) = line.strip_prefix("!SPACE_GROUP_NUMBER=") {
                if let Ok(n @ 1..=230) = value.trim().parse::<u32>() {
                    sg = Some(format!("#{n}"));
                }
            }
        } else {
            let mut fields = line.split_whitespace();
            match fields.next().map(str::to_ascii_uppercase).as_deref() {
                Some("CELL") => {
                    fields.next();
                    cell = parse_cell(fields);
                }
                Some("LATT") => {
                    sg = fields
                        .next()
                        .and_then(|n| n.parse::<i32>().ok())
                        .and_then(|n| match n.unsigned_abs() {
                            1 => Some('P'),
                            2 => Some('I'),
                            3 => Some('R'),
                            4 => Some('F'),
                            5 => Some('A'),
                            6 => Some('B'),
                            7 => Some('C'),
                            _ => None,
                        })
                        .map(|c| format!("{c} (centring only)"));
                }
                Some("HKLF" | "END") => break,
                _ => {}
            }
        }
    }
    ensure!(
        sg.is_some(),
        "missing lattice centring / space group in {name}; not assuming primitive"
    );
    match cell {
        Some(c) if valid_cell(c) => Ok(Stats {
            cell: Some(c),
            space_group: sg,
            ..Stats::default()
        }),
        _ => bail!("no valid unit cell in first {HEADER_BYTES} bytes of {name}"),
    }
}

fn parse_cell<'a>(values: impl Iterator<Item = &'a str>) -> Option<[f64; 6]> {
    let v: Vec<f64> = values
        .take(6)
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()
        .ok()?;
    v.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shelx_cell_and_centring_without_inventing_space_group() {
        let st = parse_header(
            "sample.res",
            "TITL misleading P1\nCELL 0.71073 10 20 30 90 100 90\nLATT -7\n",
        )
        .unwrap();
        assert_eq!(st.cell, Some([10., 20., 30., 90., 100., 90.]));
        assert_eq!(st.space_group.as_deref(), Some("C (centring only)"));
        assert_eq!(
            crate::cells::centring(st.space_group.as_ref().unwrap()),
            'C'
        );
        assert!(st.overall.is_none());
    }
    #[test]
    fn xds_header_stops_before_reflections() {
        let st = parse_header("XDS_ASCII.HKL", "!SPACE_GROUP_NUMBER= 19\n!UNIT_CELL_CONSTANTS= 10 20 30 90 90 90\n!END_OF_HEADER\n!UNIT_CELL_CONSTANTS= 0 0 0 0 0 0").unwrap();
        assert_eq!(st.space_group.as_deref(), Some("#19"));
        assert_eq!(st.cell.unwrap()[0], 10.);
    }
    #[test]
    fn invalid_cells_are_not_indexed() {
        for c in [
            "NaN 20 30 90 90 90",
            "0 20 30 90 90 90",
            "10 20 30 1 1 179",
            "10 20 30 90 90",
            "10 20 30 90 180 90",
        ] {
            assert!(parse_header("a.ins", &format!("CELL 1 {c}\nLATT 1")).is_err());
        }
    }
    #[test]
    fn missing_centring_is_not_silently_treated_as_primitive() {
        assert!(parse_header("a.ins", "CELL 1 10 20 30 90 90 90").is_err());
        assert!(parse_header("XDS_ASCII.HKL", "!UNIT_CELL_CONSTANTS= 10 20 30 90 90 90").is_err());
    }
    #[test]
    fn cache_scope_keeps_alba_compatible_and_nas_roots_separate() {
        let mut cfg = crate::config::Config::default();
        assert_eq!(cfg.cache_host(), cfg.host);
        let alba = cfg.cache_host();
        cfg.nas = true;
        cfg.remote_root = Some("/data/XRay".into());
        let xray = cfg.cache_host();
        assert_ne!(alba, xray);
        cfg.remote_root = Some("/data/XRay/".into());
        assert_eq!(cfg.cache_host(), xray);
        cfg.remote_root = Some("/Public/ALBA".into());
        assert_ne!(cfg.cache_host(), xray);
    }
    #[test]
    fn paths_and_source_identity() {
        for root in ["", "/", "relative", "/a/../b", "/a//b", "/a\nb"] {
            assert!(validate_root(root).is_err(), "{root:?}");
        }
        assert!(validate_root("/Public/ALBA").is_ok());
        assert!(!visible("@eaDir"));
        assert!(!visible("../secret"));
        assert_ne!(source_id("nas", 22, "/a"), source_id("nas", 22, "/b"));
        assert_ne!(source_id("nas", 22, "/a"), source_id("nas", 2222, "/a"));
        assert_ne!(
            crate::cells::sample_key("nas-a/user", "a.res"),
            crate::cells::sample_key("nas-b/user", "a.res")
        );
    }
}

/// Opt-in real server check: NAS_HOST, NAS_PORT, NAS_USER, NAS_PASS,
/// NAS_ROOT, NAS_GROUP (empty for root files), NAS_SAMPLE, NAS_KNOWN_HOSTS.
/// Reads one metadata file; does not create or change anything on the server.
#[cfg(test)]
mod live {
    use super::super::{
        session::{self, Target},
        sftp::Limiter,
    };
    use super::*;
    use std::sync::{Arc, atomic::AtomicU64};
    #[tokio::test]
    #[ignore = "requires explicit NAS credentials and a pinned host key"]
    async fn live_nas_readonly() {
        let env = |key| std::env::var(key).unwrap_or_else(|_| panic!("set {key}"));
        let host = env("NAS_HOST");
        let root = env("NAS_ROOT");
        let group = env("NAS_GROUP");
        let sample = env("NAS_SAMPLE");
        let target = Target {
            host: &host,
            port: env("NAS_PORT").parse().unwrap(),
            known_hosts: Some(env("NAS_KNOWN_HOSTS").into()),
            limiter: Arc::new(Limiter::new(3., 6, Arc::new(AtomicU64::new(0)))),
            beamlines: &[],
            remote_root: Some(&root),
            nas: true,
        };
        let (conn, key) = session::connect(&target, &env("NAS_USER"), &env("NAS_PASS"))
            .await
            .expect("NAS login");
        assert_eq!(
            key,
            session::HostKey::Known,
            "live test requires a previously pinned key"
        );
        let sftp = &conn.sftp;
        let listed = super::super::scan::list_experiments(sftp).await.unwrap();
        let id = sftp.nas_id.as_ref().unwrap();
        let mut date = if group.is_empty() {
            id.clone()
        } else {
            format!("{id}/{group}")
        };
        assert!(listed.ids.contains(&date));
        if std::env::var("NAS_PROBE_ALL_FOLDERS").is_ok() {
            for folder in &listed.ids {
                let start = std::time::Instant::now();
                let (entries, _) = super::super::scan::list_samples(sftp, folder)
                    .await
                    .unwrap();
                println!(
                    "Immediate folder listing: {} entries in {:.2}s",
                    entries.len(),
                    start.elapsed().as_secs_f64()
                );
            }
        }
        let count = target
            .limiter
            .count
            .load(std::sync::atomic::Ordering::Relaxed);
        let (mut samples, _) = super::super::scan::list_samples(sftp, &date).await.unwrap();
        assert_eq!(
            target
                .limiter
                .count
                .load(std::sync::atomic::Ordering::Relaxed)
                - count,
            2,
            "opening a folder must only resolve/list that directory, not walk descendants"
        );
        if let Ok(expected) = std::env::var("NAS_EXPECT_FILES") {
            assert_eq!(samples.len(), expected.parse::<usize>().unwrap());
        }
        if let Ok(forbidden) = std::env::var("NAS_FORBIDDEN_PATH") {
            assert!(sftp.read_prefix(&forbidden, 1).await.is_err());
        }
        let mut components = sample.split('/').peekable();
        let mut basename = "";
        while let Some(component) = components.next() {
            if components.peek().is_some() {
                assert!(samples.contains(&format!("{component}/")));
                date = format!("{date}/{component}");
                (samples, _) = super::super::scan::list_samples(sftp, &date).await.unwrap();
            } else {
                basename = component;
            }
        }
        assert!(samples.contains(&basename.to_string()));
        let info = super::super::scan::scan_sample(sftp, &date, basename, false, None, |_| {})
            .await
            .unwrap();
        let (solve, cell) = crate::cells::sample_cell(&info).expect("indexed cell");
        assert_eq!(solve.kind, SolveKind::Archive);
        assert!(valid_cell(cell));
        let mut db = crate::cells::CellDb::load(None);
        db.record(&date, &info);
        assert_eq!(db.len(), 1);
        if let Ok(expected) = std::env::var("NAS_EXPECT_CELL") {
            let wanted: Vec<f64> = expected.split(',').map(|s| s.parse().unwrap()).collect();
            assert_eq!(cell.as_slice(), wanted.as_slice());
        }
        assert!(
            crate::app::App::stats_rows(&info)
                .iter()
                .all(|r| !matches!(r, crate::app::StatsRow::Frames { .. }))
        );
        assert!(
            super::super::scan::list_frames(sftp, &date, &sample, &[])
                .await
                .is_err()
        );
        assert!(sftp.read_prefix("/../outside", 1).await.is_err());
        // Exercise the same read-only open used by the download worker; no bulk data.
        let mut reader = sftp.open_download(&solve.hkl[0].path).await.unwrap();
        let mut bytes = [0u8; 128];
        assert!(reader.read(&mut bytes).await.unwrap() > 0);
        reader.close().await;
        println!(
            "NAS read-only integration OK: {} metadata files listed; selected cell {cell:?}",
            samples.len()
        );
        conn.close().await;
    }
}
