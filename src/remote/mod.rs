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
            println!("{sample}:");
            for r in &info.runs {
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
