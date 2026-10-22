//! SSH connection, host-key pinning (trust on first use) and password login.

use super::sftp::{Limiter, ReadOnlySftp};
use russh::client::{self, AuthResult, Handle, KeyboardInteractiveAuthResponse};
use russh::keys::{HashAlg, PublicKeyOrCertificate};
use russh::MethodKind;
use russh_sftp::client::SftpSession;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKey {
    Known,
    Added { fingerprint: String },
    Mismatch { expected: String, got: String },
    /// No config dir to keep known hosts in.
    Unchecked,
}

pub struct Client {
    host_id: String,
    known_hosts: Option<PathBuf>,
    outcome: Arc<Mutex<Option<HostKey>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            return Ok(false);
        };
        let fp = key.fingerprint(HashAlg::Sha256).to_string();
        let algo = key.algorithm().to_string();
        let outcome = check_and_record(self.known_hosts.as_deref(), &self.host_id, &algo, &fp);
        let ok = !matches!(outcome, HostKey::Mismatch { .. });
        if let Ok(mut o) = self.outcome.lock() {
            *o = Some(outcome);
        }
        Ok(ok)
    }
}

/// `known_hosts` lines: `<host>:<port> <algorithm> <SHA256 fingerprint>`.
fn check_and_record(path: Option<&Path>, host_id: &str, algo: &str, fp: &str) -> HostKey {
    let Some(path) = path else {
        return HostKey::Unchecked;
    };
    let text = std::fs::read_to_string(path).unwrap_or_default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        if it.next() != Some(host_id) || it.next() != Some(algo) {
            continue;
        }
        return match it.next() {
            Some(known) if known == fp => HostKey::Known,
            Some(known) => HostKey::Mismatch {
                expected: known.to_string(),
                got: fp.to_string(),
            },
            None => continue,
        };
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut text = text;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("{host_id} {algo} {fp}\n"));
    let _ = std::fs::write(path, text);
    HostKey::Added {
        fingerprint: fp.to_string(),
    }
}

#[derive(Debug)]
pub enum ConnectError {
    /// Wrong username or password. Counts towards the login attempt limit.
    Auth(String),
    HostKey(String),
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Auth(m) | ConnectError::HostKey(m) | ConnectError::Other(m) => f.write_str(m),
        }
    }
}

pub struct Connection {
    pub handle: Handle<Client>,
    pub sftp: Arc<ReadOnlySftp>,
}

impl Connection {
    pub async fn close(self) {
        self.sftp.close().await;
        let _ = self
            .handle
            .disconnect(russh::Disconnect::ByApplication, "bye", "en")
            .await;
    }
}

pub struct Target<'a> {
    pub host: &'a str,
    pub port: u16,
    pub known_hosts: Option<PathBuf>,
    pub limiter: Arc<Limiter>,
    pub beamline: &'a str,
    /// Config override for the proposal folder.
    pub remote_root: Option<&'a str>,
}

pub async fn connect(t: &Target<'_>, user: &str, password: &str) -> Result<(Connection, HostKey), ConnectError> {
    let other = |e: &dyn std::fmt::Display| ConnectError::Other(e.to_string());
    let config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 4,
        inactivity_timeout: None,
        ..Default::default()
    });
    let outcome = Arc::new(Mutex::new(None));
    let handler = Client {
        host_id: format!("{}:{}", t.host, t.port),
        known_hosts: t.known_hosts.clone(),
        outcome: outcome.clone(),
    };

    let connecting = client::connect(config, (t.host, t.port), handler);
    let mut handle = match tokio::time::timeout(Duration::from_secs(20), connecting).await {
        Err(_) => return Err(ConnectError::Other(format!("{}:{} did not answer within 20 s", t.host, t.port))),
        Ok(Err(e)) => {
            let hk = outcome.lock().ok().and_then(|o| o.clone());
            if let Some(HostKey::Mismatch { expected, got }) = hk {
                return Err(ConnectError::HostKey(format!(
                    "HOST KEY CHANGED for {}:{} (expected {expected}, got {got}). Refusing to connect. \
                     If ALBA really changed its key, delete the line from {}",
                    t.host,
                    t.port,
                    t.known_hosts.as_deref().map(|p| p.display().to_string()).unwrap_or_default()
                )));
            }
            return Err(other(&e));
        }
        Ok(Ok(h)) => h,
    };
    let host_key = outcome
        .lock()
        .ok()
        .and_then(|o| o.clone())
        .unwrap_or(HostKey::Unchecked);

    // Ask which methods the server accepts, so a wrong password costs exactly one
    // failed attempt on the server (never password + keyboard-interactive).
    let methods = match handle.authenticate_none(user).await.map_err(|e| other(&e))? {
        AuthResult::Success => Vec::new(),
        AuthResult::Failure { remaining_methods, .. } => remaining_methods.to_vec(),
    };
    let ok = if methods.is_empty() {
        true
    } else if methods.contains(&MethodKind::Password) {
        handle
            .authenticate_password(user, password)
            .await
            .map_err(|e| other(&e))?
            .success()
    } else if methods.contains(&MethodKind::KeyboardInteractive) {
        keyboard_interactive(&mut handle, user, password)
            .await
            .map_err(|e| other(&e))?
    } else {
        return Err(ConnectError::Other(format!(
            "server offers no password login (methods: {methods:?})"
        )));
    };
    if !ok {
        return Err(ConnectError::Auth("wrong username or password".into()));
    }

    let channel = handle.channel_open_session().await.map_err(|e| other(&e))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| other(&e))?;
    let cfg = russh_sftp::client::Config {
        request_timeout_secs: 60,
        ..Default::default()
    };
    let sftp = SftpSession::new_with_config(channel.into_stream(), cfg)
        .await
        .map_err(|e| other(&e))?;
    let mut sftp = ReadOnlySftp::new(sftp, t.limiter.clone());
    sftp.locate_root(user, t.beamline, t.remote_root)
        .await
        .map_err(|e| ConnectError::Other(format!("cannot find the proposal folder: {e:#}")))?;
    Ok((
        Connection {
            handle,
            sftp: Arc::new(sftp),
        },
        host_key,
    ))
}

async fn keyboard_interactive(handle: &mut Handle<Client>, user: &str, password: &str) -> Result<bool, russh::Error> {
    let mut reply = handle.authenticate_keyboard_interactive_start(user, None).await?;
    for _ in 0..4 {
        match reply {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let answers = prompts.iter().map(|_| password.to_string()).collect();
                reply = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tofu_known_hosts() {
        let p = std::env::temp_dir().join(format!("alba_watch_kh_{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let h = "userdata.cells.es:443";
        assert!(matches!(check_and_record(Some(&p), h, "ecdsa-sha2-nistp256", "SHA256:aaa"), HostKey::Added { .. }));
        assert_eq!(check_and_record(Some(&p), h, "ecdsa-sha2-nistp256", "SHA256:aaa"), HostKey::Known);
        assert!(matches!(check_and_record(Some(&p), h, "ecdsa-sha2-nistp256", "SHA256:bbb"), HostKey::Mismatch { .. }));
        let _ = std::fs::remove_file(&p);
    }
}
