pub mod scan;
pub mod session;
pub mod sftp;
pub mod worker;

pub use worker::{DownloadEvent, DownloadJob, RemoteCmd, RemoteEvent, RemoteHandle, Settings, spawn};

/// Live check against the real server, run by hand only:
/// `ALBA_USER=… ALBA_PASS=… cargo test live_probe -- --ignored --nocapture`.
/// About 30 read-only requests; downloads one 514-byte file into the temp dir.
#[cfg(test)]
mod live {
    use super::scan;
    use super::session::{self, Target};
    use super::sftp::Limiter;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[tokio::test]
    #[ignore]
    async fn live_probe() {
        let (Ok(user), Ok(pass)) = (std::env::var("ALBA_USER"), std::env::var("ALBA_PASS")) else {
            panic!("set ALBA_USER and ALBA_PASS");
        };
        let count = Arc::new(AtomicU64::new(0));
        let kh = std::env::temp_dir().join("alba_watch_live_known_hosts");
        let beamlines = vec!["bl13".to_string(), "bl06".to_string()];
        let target = Target {
            host: "userdata.cells.es",
            port: 443,
            known_hosts: Some(kh),
            limiter: Arc::new(Limiter::new(3.0, 6, count.clone())),
            beamlines: &beamlines,
            remote_root: None,
        };
        let (conn, hk) = session::connect(&target, &user, &pass).await.expect("connect");
        println!("host key: {hk:?}");
        let sftp = conn.sftp.clone();

        let dates = scan::list_experiments(&sftp).await.unwrap();
        println!("proposals: {}\ndates: {:?}", sftp.describe(), dates.ids);
        let date = std::env::var("ALBA_DATE").unwrap_or_else(|_| "20251211".into());
        let (samples, flat) = scan::list_samples(&sftp, &date).await.unwrap();
        println!("{date}: {} samples, first {:?}", samples.len(), &samples[..3.min(samples.len())]);
        if let Ok(list) = std::env::var("ALBA_SAMPLE") {
          println!("flat={flat}");
          for sample in list.split(',') {
            let info = scan::scan_sample(&sftp, &date, sample, flat, None, |_| {}).await.unwrap();
            println!("{sample}:  test_shots_only={} unprocessed={}", info.test_shots_only(), info.unprocessed());
            for r in &info.runs {
                println!("  run {} prefix {} characterisation={} solves={}", r.run, r.prefix, r.characterisation, r.solves.len());
                for s in &r.solves {
                    let o = s.stats.as_ref().and_then(|st| st.overall.clone()).unwrap_or_default();
                    println!(
                        "  run {} {:<16} {:?} sg={:?} d={:?} rmerge%={:?} hkl={:?} logs={} {:?}",
                        r.run,
                        s.kind.label(),
                        s.status,
                        s.stats.as_ref().and_then(|st| st.space_group.clone()),
                        o.d_high,
                        o.rmerge_pct,
                        s.hkl.iter().map(|f| (f.name().to_string(), f.size)).collect::<Vec<_>>(),
                        s.logs.len(),
                        s.note
                    );
                }
            }
            if std::env::var("ALBA_FRAMES").is_ok() {
                let runs: Vec<(String, u32)> = info.runs.iter().map(|r| (r.prefix.clone(), r.run)).collect();
                let frames = scan::list_frames(&sftp, &date, sample, &runs).await.unwrap();
                let total: u64 = frames.iter().filter_map(|f| f.size).sum();
                println!("  frames: {} files, {} bytes, e.g. {:?}", frames.len(), total, frames.first().map(|f| f.name()));
            }
          }
            println!("total requests: {}", count.load(Ordering::Relaxed));
            conn.close().await;
            return;
        }

        let t = std::time::Instant::now();
        let info = scan::scan_sample(&sftp, "20251211", "JSG285b", false, None, |_| {}).await.unwrap();
        println!("scan JSG285b took {:.1}s, requests so far {}", t.elapsed().as_secs_f64(), count.load(Ordering::Relaxed));
        for r in &info.runs {
            println!("run {} prefix {} char={}", r.run, r.prefix, r.characterisation);
            for s in &r.solves {
                let o = s.stats.as_ref().and_then(|st| st.overall.clone()).unwrap_or_default();
                println!(
                    "  {:<16} {:?} sg={:?} d={:?} rmerge%={:?} cc={:?} hkl={:?} logs={}",
                    s.kind.label(),
                    s.status,
                    s.stats.as_ref().and_then(|st| st.space_group.clone()),
                    o.d_high,
                    o.rmerge_pct,
                    o.cc_half,
                    s.hkl.iter().map(|f| (f.name().to_string(), f.size)).collect::<Vec<_>>(),
                    s.logs.len()
                );
            }
        }

        let frames = scan::list_frames(&sftp, "20251211", "JSG285b", &[("manually-mounted".into(), 2)])
            .await
            .unwrap();
        println!("run 2 frames: {} ({:?})", frames.len(), frames.iter().map(|f| f.name()).collect::<Vec<_>>());

        let ins = info
            .solves()
            .flat_map(|s| s.hkl.iter())
            .find(|f| f.name() == "shelxt.ins")
            .expect("xia2chem shelxt.ins");
        let bytes = sftp.read_small(&ins.path, 4096).await.unwrap();
        println!("shelxt.ins first line: {}", String::from_utf8_lossy(&bytes).lines().next().unwrap_or(""));
        println!("total requests: {}", count.load(Ordering::Relaxed));
        conn.close().await;
    }
}

/// Reproduces key renegotiation (rekey) during a download, cheaply: the client rekeys
/// every `ALBA_REKEY_MB` instead of every 1 GiB. Reads `ALBA_FILES` raw frames of
/// `ALBA_SAMPLE` (run 1) into memory. `ALBA_CIPHER=aes` prefers AES-GCM.
/// `cargo test live_rekey -- --ignored --nocapture`
#[cfg(test)]
mod live_rekey {
    use super::scan;
    use super::session::{self, Target};
    use super::sftp::Limiter;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    #[tokio::test]
    #[ignore]
    async fn live_rekey() {
        let env = |k: &str| std::env::var(k).ok();
        let (Some(user), Some(pass), Some(date), Some(sample)) =
            (env("ALBA_USER"), env("ALBA_PASS"), env("ALBA_DATE"), env("ALBA_SAMPLE"))
        else {
            panic!("set ALBA_USER, ALBA_PASS, ALBA_DATE, ALBA_SAMPLE");
        };
        let mb: usize = env("ALBA_REKEY_MB").and_then(|v| v.parse().ok()).unwrap_or(16);
        let files: usize = env("ALBA_FILES").and_then(|v| v.parse().ok()).unwrap_or(6);
        let mut cfg = session::client_config();
        cfg.limits = russh::Limits {
            rekey_write_limit: mb << 20,
            rekey_read_limit: mb << 20,
            rekey_time_limit: std::time::Duration::from_secs(3600),
        };
        if env("ALBA_CIPHER").as_deref() == Some("aes") {
            cfg.preferred = russh::Preferred {
                cipher: std::borrow::Cow::Borrowed(&[russh::cipher::AES_256_GCM, russh::cipher::AES_128_GCM]),
                ..Default::default()
            };
        }
        let beamlines = vec!["bl13".to_string(), "bl06".to_string()];
        let target = Target {
            host: "userdata.cells.es",
            port: 443,
            known_hosts: Some(std::env::temp_dir().join("alba_watch_live_known_hosts")),
            limiter: Arc::new(Limiter::new(3.0, 6, Arc::new(AtomicU64::new(0)))),
            beamlines: &beamlines,
            remote_root: None,
        };
        let (conn, _) = session::connect_with(&target, &user, &pass, cfg).await.expect("connect");
        let frames = scan::list_frames(&conn.sftp, &date, &sample, &[(sample.clone(), 1)]).await.unwrap();
        println!("rekey every {mb} MiB; {} frames listed, reading {files}", frames.len());
        let mut total = 0usize;
        for f in frames.iter().take(files) {
            let t = std::time::Instant::now();
            let res: anyhow::Result<usize> = async {
                let mut r = conn.sftp.open_read(&f.path).await?;
                let mut buf = vec![0u8; 256 * 1024];
                let mut n = 0;
                loop {
                    let k = r.read(&mut buf).await?;
                    if k == 0 {
                        break;
                    }
                    n += k;
                }
                r.close().await;
                Ok(n)
            }
            .await;
            match res {
                Ok(n) => {
                    total += n;
                    println!("  {} {} bytes in {:.1}s (total {:.1} MiB)", f.name(), n, t.elapsed().as_secs_f64(), total as f64 / 1048576.0);
                }
                Err(e) => {
                    println!("  {} FAILED after {:.1} MiB total: {e:#}", f.name(), total as f64 / 1048576.0);
                    break;
                }
            }
        }
        println!("session closed: {}", conn.handle.is_closed());
        conn.close().await;
    }
}

/// Throughput of frame downloads: sequential vs N files at a time on one session.
/// `ALBA_PAR` (default 1), `ALBA_FROM` (first frame index), `ALBA_FILES`, `ALBA_WINDOW_MB`.
/// `cargo test live_bench -- --ignored --nocapture`
#[cfg(test)]
mod live_bench {
    use super::scan;
    use super::session::{self, Target};
    use super::sftp::Limiter;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore]
    async fn live_bench() {
        let env = |k: &str| std::env::var(k).ok();
        let (Some(user), Some(pass), Some(date), Some(sample)) =
            (env("ALBA_USER"), env("ALBA_PASS"), env("ALBA_DATE"), env("ALBA_SAMPLE"))
        else {
            panic!("set ALBA_USER, ALBA_PASS, ALBA_DATE, ALBA_SAMPLE");
        };
        let par: usize = env("ALBA_PAR").and_then(|v| v.parse().ok()).unwrap_or(1);
        let from: usize = env("ALBA_FROM").and_then(|v| v.parse().ok()).unwrap_or(300);
        let files: usize = env("ALBA_FILES").and_then(|v| v.parse().ok()).unwrap_or(8);
        let mut cfg = session::client_config();
        if let Some(w) = env("ALBA_WINDOW_MB").and_then(|v| v.parse::<u32>().ok()) {
            cfg.window_size = w << 20;
        }
        let beamlines = vec!["bl13".to_string(), "bl06".to_string()];
        let target = Target {
            host: "userdata.cells.es",
            port: 443,
            known_hosts: Some(std::env::temp_dir().join("alba_watch_live_known_hosts")),
            limiter: Arc::new(Limiter::new(100.0, 100, Arc::new(AtomicU64::new(0)))),
            beamlines: &beamlines,
            remote_root: None,
        };
        let (conn, _) = session::connect_with(&target, &user, &pass, cfg).await.expect("connect");
        let frames = scan::list_frames(&conn.sftp, &date, &sample, &[(sample.clone(), 1)]).await.unwrap();
        let todo: Vec<String> = frames.iter().skip(from).take(files).map(|f| f.path.clone()).collect();
        let queue = Arc::new(tokio::sync::Mutex::new(todo));
        let t = std::time::Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..par {
            let (q, sftp) = (queue.clone(), conn.sftp.clone());
            tasks.spawn(async move {
                let mut bytes = 0usize;
                loop {
                    let Some(path) = q.lock().await.pop() else { break };
                    let mut r = sftp.open_read(&path).await.expect("open");
                    let mut buf = vec![0u8; 256 * 1024];
                    loop {
                        let k = r.read(&mut buf).await.expect("read");
                        if k == 0 {
                            break;
                        }
                        bytes += k;
                    }
                    r.close().await;
                }
                bytes
            });
        }
        let mut total = 0usize;
        while let Some(b) = tasks.join_next().await {
            total += b.unwrap();
        }
        let secs = t.elapsed().as_secs_f64();
        println!(
            "par={par} files={files} window={:?}: {:.1} MB in {:.1}s = {:.1} MB/s",
            env("ALBA_WINDOW_MB"),
            total as f64 / 1e6,
            secs,
            total as f64 / 1e6 / secs
        );
        conn.close().await;
    }
}
