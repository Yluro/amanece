//! Version check and self-update from the GitHub releases of Yluro/amanece.
//!
//! The check is one small HTTPS request to the GitHub API (at startup and once a day,
//! `check_updates = false` in config.toml turns it off). Updating only happens when
//! the user asks for it: the archive for this platform is downloaded, checked against
//! the `.sha256` published next to it, the binary in it is started once with
//! `--version` to make sure it runs here, and only then it replaces the running exe.
//! Nothing of this touches the ALBA server.

use crate::event::AppEvent;
use anyhow::{Context, Result, anyhow, bail};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST_URL: &str = "https://api.github.com/repos/Yluro/amanece/releases/latest";
pub const RELEASES_URL: &str = "https://github.com/Yluro/amanece/releases";
/// Archives are a few MB; anything much bigger is not ours.
const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
#[cfg(windows)]
const EXE_NAME: &str = "amanece.exe";
#[cfg(not(windows))]
const EXE_NAME: &str = "amanece";

/// The release workflow's build targets (`.github/workflows/release.yml`).
pub const TARGET: Option<&str> = if cfg!(all(windows, target_arch = "x86_64")) {
    Some("x86_64-pc-windows-msvc")
} else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some("aarch64-apple-darwin")
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some("x86_64-apple-darwin")
} else if cfg!(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")) {
    Some("x86_64-unknown-linux-gnu")
} else {
    None
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// "0.4.0"
    pub version: String,
    /// The release page, for the browser.
    pub page: String,
    /// The archive for this platform and its `.sha256`, when the release has both.
    pub archive: Option<(Asset, Asset)>,
}

#[derive(Debug)]
pub enum UpdateEvent {
    /// `Ok(None)`: this is the latest version. `manual`: the user asked, so say it.
    Checked { result: Result<Option<Release>, String>, manual: bool },
    Progress { done: u64, total: Option<u64> },
    Installed(String),
    Failed(String),
}

/// What the status-bar widget shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Not checked (yet, or `check_updates = false`).
    Idle,
    Checking,
    UpToDate,
    CheckFailed(String),
    Available(Release),
    Downloading { release: Release, done: u64, total: Option<u64> },
    /// Replaced on disk; the running program is still the old one.
    Installed(String),
    Failed { release: Release, error: String },
}

impl State {
    pub fn busy(&self) -> bool {
        matches!(self, State::Checking | State::Downloading { .. })
    }
}

/// "v0.4.0", "0.4.0-rc.1" → (0, 4, 0). Anything after `-`/`+` is ignored.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let major = parts.next()??;
    let minor = parts.next().unwrap_or(Some(0))?;
    let patch = parts.next().unwrap_or(Some(0))?;
    parts.next().is_none().then_some((major, minor, patch))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Reads the `releases/latest` JSON. `Ok(None)` when it isn't newer than `current`.
fn parse_latest(json: &str, current: &str, target: Option<&str>) -> Result<Option<Release>> {
    let v: serde_json::Value = serde_json::from_str(json).context("GitHub sent no JSON")?;
    let tag = v["tag_name"].as_str().ok_or_else(|| anyhow!("release without tag"))?;
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if parse_version(&version).is_none() {
        bail!("cannot read the version of release {tag:?}");
    }
    if !is_newer(&version, current) {
        return Ok(None);
    }
    let page = v["html_url"].as_str().unwrap_or(RELEASES_URL).to_string();
    let assets: Vec<Asset> = v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Asset {
                        name: x["name"].as_str()?.to_string(),
                        url: x["browser_download_url"].as_str()?.to_string(),
                        size: x["size"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let archive = target.and_then(|t| {
        let ext = if t.contains("windows") { "zip" } else { "tar.gz" };
        let name = format!("amanece-{tag}-{t}.{ext}");
        let find = |n: &str| assets.iter().find(|a| a.name == n).cloned();
        Some((find(&name)?, find(&format!("{name}.sha256"))?))
    });
    Ok(Some(Release { version, page, archive }))
}

fn agent() -> ureq::Agent {
    use ureq::tls::{RootCerts, TlsConfig};
    ureq::Agent::config_builder()
        .user_agent(format!("amanece/{CURRENT}"))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        // the system's certificates, so networks with their own CA work too
        .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
        .build()
        .into()
}

fn latest_json() -> Result<String> {
    let mut resp = agent()
        .get(LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .config()
        .timeout_global(Some(Duration::from_secs(45)))
        .build()
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(404) => anyhow!("no releases published yet"),
            ureq::Error::StatusCode(403 | 429) => anyhow!("GitHub rate limit, try again later"),
            e => anyhow!(e),
        })?;
    Ok(resp.body_mut().read_to_string()?)
}

fn fetch_latest() -> Result<Option<Release>> {
    parse_latest(&latest_json()?, CURRENT, TARGET)
}

/// Asks GitHub for the latest release in a background thread; answers with
/// `UpdateEvent::Checked`.
pub fn check(tx: Sender<AppEvent>, manual: bool) {
    std::thread::spawn(move || {
        let result = fetch_latest().map_err(|e| format!("{e:#}"));
        let _ = tx.send(AppEvent::Update(UpdateEvent::Checked { result, manual }));
    });
}

/// Downloads and installs `release` in a background thread; answers with
/// `Progress` events, then `Installed` or `Failed`.
pub fn install(release: Release, tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        let ev = match install_blocking(&release, &tx) {
            Ok(()) => UpdateEvent::Installed(release.version),
            Err(e) => UpdateEvent::Failed(format!("{e:#}")),
        };
        let _ = tx.send(AppEvent::Update(ev));
    });
}

fn download(agent: &ureq::Agent, url: &str, mut progress: impl FnMut(u64, Option<u64>)) -> Result<Vec<u8>> {
    let mut resp = agent
        .get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .call()?;
    let total = resp.body().content_length();
    let mut reader = resp.body_mut().with_config().limit(MAX_ARCHIVE).reader();
    let mut data = Vec::with_capacity(total.unwrap_or(0).min(MAX_ARCHIVE) as usize);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&chunk[..n]);
        progress(data.len() as u64, total);
    }
    Ok(data)
}

/// The first word of a `sha256sum` line: `<64 hex>  <file name>`.
fn parse_sha256_file(text: &str) -> Result<[u8; 32]> {
    let hex = text.split_whitespace().next().unwrap_or("");
    let mut out = [0u8; 32];
    if hex.len() != 64 || !hex.is_ascii() {
        bail!("the .sha256 file has no checksum");
    }
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).context("the .sha256 file has no checksum")?;
    }
    Ok(out)
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(data).into()
}

/// The `amanece(.exe)` inside a release `.zip`.
#[cfg(windows)]
fn exe_from_zip(data: &[u8]) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(data)).context("not a zip archive")?;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let is_exe = f.name()?.rsplit(['/', '\\']).next() == Some(EXE_NAME);
        if f.is_file() && is_exe {
            let mut out = Vec::with_capacity(f.size() as usize);
            f.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    bail!("{EXE_NAME} is not in the archive")
}

/// The `amanece` inside a release `.tar.gz`.
fn exe_from_tar_gz(data: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(data));
    for entry in tar.entries().context("not a .tar.gz archive")? {
        let mut e = entry?;
        let is_exe = e.header().entry_type().is_file()
            && e.path()?.file_name().and_then(|n| n.to_str()) == Some(EXE_NAME);
        if is_exe {
            let mut out = Vec::new();
            e.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    bail!("{EXE_NAME} is not in the archive")
}

fn exe_from_archive(name: &str, data: &[u8]) -> Result<Vec<u8>> {
    if name.ends_with(".zip") {
        #[cfg(windows)]
        return exe_from_zip(data);
        #[cfg(not(windows))]
        bail!("zip archives are only for Windows");
    }
    exe_from_tar_gz(data)
}

/// Removes the file when dropped (the staged new binary).
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Runs `exe --version` and checks it prints `version`.
fn runs_here(exe: &Path, version: &str) -> Result<()> {
    let out = std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .context("the new version does not start on this computer")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || !text.contains(version) {
        bail!("the new version does not start on this computer ({})", text.trim());
    }
    Ok(())
}

fn install_blocking(release: &Release, tx: &Sender<AppEvent>) -> Result<()> {
    let (archive, checksum) = release
        .archive
        .as_ref()
        .ok_or_else(|| anyhow!("no download for this platform"))?;
    let exe = std::env::current_exe().context("cannot find amanece's own file")?;
    let dir = exe.parent().ok_or_else(|| anyhow!("cannot find amanece's folder"))?;

    let agent = agent();
    let expected = parse_sha256_file(&String::from_utf8_lossy(&download(&agent, &checksum.url, |_, _| {})?))?;
    let mut last = Instant::now();
    let data = download(&agent, &archive.url, |done, total| {
        let total = total.or((archive.size > 0).then_some(archive.size));
        if last.elapsed() >= Duration::from_millis(150) || Some(done) == total {
            last = Instant::now();
            let _ = tx.send(AppEvent::Update(UpdateEvent::Progress { done, total }));
        }
    })?;
    if sha256(&data) != expected {
        bail!("{} does not match its SHA-256, nothing was changed", archive.name);
    }
    let new_exe = exe_from_archive(&archive.name, &data)?;

    // Staged next to the running exe: proves the folder is writable before anything
    // is touched, and avoids a /tmp mounted noexec.
    let staged = TempFile(dir.join(format!(".amanece-update-{}{}", release.version, std::env::consts::EXE_SUFFIX)));
    std::fs::write(&staged.0, &new_exe)
        .with_context(|| format!("cannot write to {} (update by hand)", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged.0, std::fs::Permissions::from_mode(0o755))?;
    }
    runs_here(&staged.0, &release.version)?;
    self_replace::self_replace(&staged.0)
        .with_context(|| format!("cannot replace {}", exe.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("0.4.0-rc.1"), Some((0, 4, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert!(is_newer("v0.10.0", "0.9.1"));
        assert!(is_newer("0.3.1", "0.3.0"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        assert!(!is_newer("garbage", "0.3.0"));
    }

    const LATEST: &str = r#"{
        "tag_name": "v0.4.0",
        "html_url": "https://github.com/Yluro/amanece/releases/tag/v0.4.0",
        "assets": [
            {"name": "amanece-v0.4.0-x86_64-pc-windows-msvc.zip", "size": 3358132,
             "browser_download_url": "https://example/w.zip"},
            {"name": "amanece-v0.4.0-x86_64-pc-windows-msvc.zip.sha256", "size": 108,
             "browser_download_url": "https://example/w.zip.sha256"},
            {"name": "amanece-v0.4.0-aarch64-apple-darwin.tar.gz", "size": 3330168,
             "browser_download_url": "https://example/m.tar.gz"}
        ]
    }"#;

    #[test]
    fn picks_this_platforms_archive_and_checksum() {
        let r = parse_latest(LATEST, "0.3.0", Some("x86_64-pc-windows-msvc")).unwrap().unwrap();
        assert_eq!(r.version, "0.4.0");
        assert!(r.page.ends_with("/tag/v0.4.0"));
        let (a, c) = r.archive.unwrap();
        assert_eq!((a.url.as_str(), a.size), ("https://example/w.zip", 3358132));
        assert_eq!(c.url, "https://example/w.zip.sha256");
        // the mac archive has no .sha256 next to it: only the release page is offered
        let mac = parse_latest(LATEST, "0.3.0", Some("aarch64-apple-darwin")).unwrap().unwrap();
        assert_eq!(mac.archive, None);
        let other = parse_latest(LATEST, "0.3.0", None).unwrap().unwrap();
        assert_eq!(other.archive, None);
        // up to date, or ahead (a development build)
        assert_eq!(parse_latest(LATEST, "0.4.0", TARGET).unwrap(), None);
        assert_eq!(parse_latest(LATEST, "0.5.0", TARGET).unwrap(), None);
        assert!(parse_latest("{}", "0.3.0", TARGET).is_err());
        assert!(parse_latest("<html>", "0.3.0", TARGET).is_err());
    }

    #[test]
    fn reads_sha256sum_lines() {
        let hex = "a".repeat(64);
        assert_eq!(parse_sha256_file(&format!("{hex}  amanece.zip\n")).unwrap(), [0xaa; 32]);
        assert_eq!(parse_sha256_file(&format!("{hex}\n")).unwrap(), [0xaa; 32]);
        assert!(parse_sha256_file("").is_err());
        assert!(parse_sha256_file(&"g".repeat(64)).is_err());
        assert!(parse_sha256_file("abc  amanece.zip").is_err());
        // a known digest
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256(b""), parse_sha256_file(empty).unwrap());
    }

    #[test]
    fn finds_the_binary_in_a_tar_gz() {
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (path, body) in [("amanece-v0.4.0-x/README.md", &b"readme"[..]), (&format!("amanece-v0.4.0-x/{EXE_NAME}"), b"ELF")] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, path, body).unwrap();
        }
        let gz = tar.into_inner().unwrap().finish().unwrap();
        assert_eq!(exe_from_archive("a.tar.gz", &gz).unwrap(), b"ELF");
        assert!(exe_from_archive("a.tar.gz", b"not gzip").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn finds_the_binary_in_a_zip() {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("amanece-v0.4.0-x/README.md", opts).unwrap();
        zip.write_all(b"readme").unwrap();
        zip.start_file("amanece-v0.4.0-x/amanece.exe", opts).unwrap();
        zip.write_all(b"MZ").unwrap();
        let data = zip.finish().unwrap().into_inner();
        assert_eq!(exe_from_archive("a.zip", &data).unwrap(), b"MZ");
        assert!(exe_from_archive("a.zip", b"not a zip").is_err());
    }

    /// Asks GitHub for real, downloads this platform's archive of the latest release,
    /// checks it and runs `--version` on the binary in it (in a temp folder: the test
    /// binary itself is not replaced).
    /// `cargo test live_update -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_update_download_and_verify() {
        let r = parse_latest(&latest_json().unwrap(), "0.0.1", TARGET).unwrap().expect("a release newer than 0.0.1");
        println!("latest: v{} {}", r.version, r.page);
        let (archive, checksum) = r.archive.clone().expect("an archive for this platform");
        let agent = agent();
        let sum = download(&agent, &checksum.url, |_, _| {}).unwrap();
        let data = download(&agent, &archive.url, |_, _| {}).unwrap();
        assert_eq!(sha256(&data), parse_sha256_file(&String::from_utf8_lossy(&sum)).unwrap());
        let exe = exe_from_archive(&archive.name, &data).unwrap();
        let dir = std::env::temp_dir().join(format!("amanece_live_update_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = TempFile(dir.join(EXE_NAME));
        std::fs::write(&path.0, exe).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        runs_here(&path.0, &r.version).unwrap();
        drop(path);
        std::fs::remove_dir_all(&dir).ok();
        println!("{} ({} bytes) verified and runs", archive.name, data.len());
    }
}
