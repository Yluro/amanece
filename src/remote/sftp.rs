//! Read-only view of the SFTP session. Only listing, stat and reading are reachable
//! from the rest of the app: nothing here can create, modify or delete remote files.

use anyhow::{Result, anyhow};
use russh_sftp::client::SftpSession;
use russh_sftp::client::fs::File;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

/// Token bucket shared by every remote request (listings, stats, opens).
pub struct Limiter {
    state: tokio::sync::Mutex<(f64, Instant)>,
    rate: f64,
    burst: f64,
    pub count: Arc<AtomicU64>,
}

impl Limiter {
    pub fn new(rate: f64, burst: u32, count: Arc<AtomicU64>) -> Self {
        let burst = f64::from(burst.max(1));
        Limiter {
            state: tokio::sync::Mutex::new((burst, Instant::now())),
            rate,
            burst,
            count,
        }
    }

    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut s = self.state.lock().await;
                let now = Instant::now();
                s.0 = (s.0 + now.duration_since(s.1).as_secs_f64() * self.rate).min(self.burst);
                s.1 = now;
                if s.0 >= 1.0 {
                    s.0 -= 1.0;
                    None
                } else {
                    Some(Duration::from_secs_f64((1.0 - s.0) / self.rate))
                }
            };
            match wait {
                None => {
                    self.count.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Some(d) => tokio::time::sleep(d).await,
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub size: Option<u64>,
    pub mtime: Option<u32>,
}

/// File opens per second for downloads (browsing/scanning keeps the main limiter).
const DOWNLOAD_OPENS_PER_SECOND: f64 = 10.0;

pub struct ReadOnlySftp {
    inner: SftpSession,
    limiter: Arc<Limiter>,
    /// Separate budget for download opens, so frames can use a fast link while
    /// listings stay at a few requests per second. Counts into the same total.
    downloads: Arc<Limiter>,
    /// The account's login folder. App paths (`/20251211/...`) are relative to it.
    root: String,
    /// Proposal folders holding beamtimes, relative to `root` (`""` = the root itself).
    proposals: Vec<String>,
    /// Id of the folder source (`nas-…`, `bruker-…`) once `configure_folders` ran; the
    /// read-only path guard applies exactly when this is set.
    pub nas_id: Option<String>,
    kind: crate::config::ServerKind,
}

impl ReadOnlySftp {
    pub fn new(inner: SftpSession, limiter: Arc<Limiter>) -> Self {
        let downloads = Arc::new(Limiter::new(DOWNLOAD_OPENS_PER_SECOND, 10, limiter.count.clone()));
        ReadOnlySftp {
            inner,
            limiter,
            downloads,
            root: "/".into(),
            nas_id: None,
            kind: crate::config::ServerKind::Alba,
            proposals: vec![String::new()],
        }
    }

    /// Finds the proposal folders that hold the beamtime folders.
    ///
    /// Older proposal accounts log straight into theirs (`/20251211/...`). Newer ones log
    /// into the beamline tree; for each beamline (`bl13` XALOC, `bl06` XAIRA …) the folder
    /// named after the account number is looked up in `/<bl>/projects/cycle*/`, only in
    /// cycles from the proposal's year on (2–3 listings per beamline).
    pub async fn locate_proposals(&mut self, user: &str, beamlines: &[String], configured: Option<&str>) -> Result<()> {
        if let Some(r) = configured.map(str::trim).filter(|r| !r.is_empty()) {
            let r = r.trim_end_matches('/');
            self.root = if r.is_empty() { "/".into() } else { r.to_string() };
            self.proposals = vec![String::new()];
            return Ok(());
        }
        self.limiter.acquire().await;
        let home = self
            .inner
            .canonicalize(".")
            .await
            .ok()
            .filter(|r| r.starts_with('/'))
            .unwrap_or_else(|| "/".into());
        self.root = home.clone();
        self.proposals = vec![String::new()];
        let top = self.list_abs(&home).await?;
        if top.iter().any(|e| e.is_dir && crate::layout::is_date_dir(&e.name)) {
            return Ok(());
        }
        let digits = user.trim().trim_start_matches(['u', 'U']).to_string();
        if digits.is_empty() {
            return Ok(());
        }
        let year: Option<u32> = digits.get(..4).and_then(|y| y.parse().ok());
        let wanted = format!("{digits}-");
        let mut found = Vec::new();
        for bl in beamlines {
            if !top.iter().any(|e| e.is_dir && e.name == *bl) {
                continue;
            }
            let projects = format!("{bl}/projects");
            let Ok(entries) = self.list_abs(&join(&home, &projects)).await else {
                continue;
            };
            let mut cycles: Vec<String> = entries
                .into_iter()
                .filter(|e| e.is_dir && e.name.starts_with("cycle"))
                .filter(|e| {
                    let cy: Option<u32> = e.name.get(5..9).and_then(|y| y.parse().ok());
                    matches!((year, cy), (Some(p), Some(c)) if c >= p) || year.is_none() || cy.is_none()
                })
                .map(|e| e.name)
                .collect();
            cycles.sort_by(|a, b| b.cmp(a));
            for c in cycles {
                let Ok(entries) = self.list_abs(&join(&home, &format!("{projects}/{c}"))).await else {
                    continue;
                };
                for e in entries
                    .iter()
                    .filter(|e| e.is_dir && (e.name == digits || e.name.starts_with(&wanted)))
                {
                    found.push(format!("{projects}/{c}/{}", e.name));
                }
            }
        }
        if !found.is_empty() {
            self.proposals = found;
        }
        Ok(())
    }

    /// What this session browses: ALBA until `configure_folders` says otherwise.
    pub fn kind(&self) -> crate::config::ServerKind {
        self.kind
    }

    /// Browse folders (`Folders` or `Bruker`) from `root`; without one, `Folders` starts in
    /// the login folder and `Bruker` in its usual `/xray/data/user`.
    pub async fn configure_folders(
        &mut self,
        kind: crate::config::ServerKind,
        host: &str,
        port: u16,
        root: Option<&str>,
    ) -> Result<()> {
        debug_assert!(kind.source_ids());
        let root = match root.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => r.to_string(),
            None if kind == crate::config::ServerKind::Bruker => crate::config::BRUKER_DEFAULT_ROOT.to_string(),
            None => {
                self.limiter.acquire().await;
                self.inner
                    .canonicalize(".")
                    .await
                    .ok()
                    .filter(|r| r.starts_with('/'))
                    .unwrap_or_else(|| "/".into())
            }
        };
        super::nas::validate_root(&root)?;
        self.root = root.trim_end_matches('/').to_string();
        self.nas_id = Some(super::nas::source_id(kind, host, port, &self.root));
        self.kind = kind;
        Ok(())
    }

    /// Check resolved paths too: a symlink must not escape the selected archive.
    async fn check_nas_path(&self, path: &str) -> Result<()> {
        if self.nas_id.is_none() {
            return Ok(());
        }
        super::nas::validate_relative(path.trim_start_matches('/'))?;
        self.limiter.acquire().await;
        let resolved = self.inner.canonicalize(self.full(path)).await?;
        anyhow::ensure!(resolved == self.root || resolved.starts_with(&format!("{}/", self.root)),
            "path is outside the NAS archive: {path}");
        Ok(())
    }

    /// Only a bounded header is read, even for large reflection files.
    pub async fn read_prefix(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let mut r = self.open_read(path).await?;
        let mut buf = Vec::new();
        let result = (&mut r.file).take(max_bytes).read_to_end(&mut buf).await;
        r.close().await;
        result?;
        Ok(buf)
    }

    pub fn proposals(&self) -> &[String] {
        &self.proposals
    }

    /// The proposal folders as absolute server paths, for messages.
    pub fn describe(&self) -> String {
        self.proposals
            .iter()
            .map(|p| {
                if p.is_empty() {
                    self.root.clone()
                } else {
                    join(&self.root, p)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    fn full(&self, path: &str) -> String {
        let root = self.root.trim_end_matches('/');
        if root.is_empty() {
            path.to_string()
        } else if path == "/" {
            self.root.clone()
        } else {
            format!("{root}{path}")
        }
    }

    /// Lists a folder given relative to the root (`/20251211/...`).
    pub async fn list(&self, path: &str) -> Result<Vec<Entry>> {
        self.check_nas_path(path).await?;
        self.list_abs(&self.full(path)).await
    }

    async fn list_abs(&self, path: &str) -> Result<Vec<Entry>> {
        self.limiter.acquire().await;
        let rd = self
            .inner
            .read_dir(path)
            .await
            .map_err(|e| anyhow!("ls {path}: {e}"))?;
        let mut v: Vec<Entry> = rd
            .filter(|e| {
                let n = e.file_name();
                n != "." && n != ".."
            })
            .map(|e| {
                let md = e.metadata();
                Entry {
                    name: e.file_name(),
                    is_dir: md.is_dir(),
                    is_symlink: md.is_symlink(),
                    size: md.size,
                    mtime: md.mtime,
                }
            })
            .collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(v)
    }

    /// Reads a small file completely; refuses anything larger than `max_bytes`.
    pub async fn read_small(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>> {
        let mut r = self.open_read(path).await?;
        if let Some(size) = r.size
            && size > max_bytes
        {
            r.close().await;
            return Err(anyhow!("{path} is too large to preview ({size} bytes)"));
        }
        let mut buf = Vec::new();
        let res = (&mut r.file)
            .take(max_bytes + 1)
            .read_to_end(&mut buf)
            .await;
        r.close().await;
        res.map_err(|e| anyhow!("read {path}: {e}"))?;
        if buf.len() as u64 > max_bytes {
            return Err(anyhow!("{path} is too large to preview"));
        }
        Ok(buf)
    }

    pub async fn open_read(&self, path: &str) -> Result<RemoteReader> {
        self.open_with(&self.limiter, path).await
    }

    /// Opens a file to download, on the download budget.
    pub async fn open_download(&self, path: &str) -> Result<RemoteReader> {
        self.open_with(&self.downloads, path).await
    }

    async fn open_with(&self, limiter: &Limiter, path: &str) -> Result<RemoteReader> {
        self.check_nas_path(path).await?;
        limiter.acquire().await;
        let file = self
            .inner
            .open(self.full(path))
            .await
            .map_err(|e| anyhow!("open {}: {e}", self.full(path)))?;
        let size = file.metadata().await.ok().and_then(|m| m.size);
        Ok(RemoteReader { file, size })
    }

    pub async fn close(&self) {
        let _ = self.inner.close().await;
    }
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// A remote file opened read-only; exposes reading only.
pub struct RemoteReader {
    file: File,
    pub size: Option<u64>,
}

impl RemoteReader {
    pub async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file.read(buf).await
    }

    /// Continue reading from byte `pos` (resuming a download).
    pub async fn seek(&mut self, pos: u64) -> std::io::Result<u64> {
        use tokio::io::AsyncSeekExt;
        self.file.seek(std::io::SeekFrom::Start(pos)).await
    }

    pub async fn close(self) {
        let _ = self.file.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn limiter_paces_after_burst() {
        let count = Arc::new(AtomicU64::new(0));
        let l = Limiter::new(10.0, 2, count.clone());
        let t = Instant::now();
        for _ in 0..4 {
            l.acquire().await;
        }
        // 2 from the burst, then 2 more at 10/s ≈ 0.2 s
        assert!(t.elapsed() >= Duration::from_millis(150));
        assert_eq!(count.load(Ordering::Relaxed), 4);
    }
}
