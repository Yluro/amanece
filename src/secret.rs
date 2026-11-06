//! Opt-in saved passwords ("Remember password" at login). They live only in the
//! operating system's credential store (Windows Credential Manager, the macOS
//! Keychain, the Secret Service on Linux), never in a file amanece writes. One entry
//! per `user@host`, under the service name `amanece`.
//!
//! Tests use an in-memory store, so they never touch the real one.

const SERVICE: &str = "amanece";

fn account(user: &str, host: &str) -> String {
    format!("{}@{host}", user.trim())
}

/// Where the passwords go, for the login dialog.
pub fn store_name() -> &'static str {
    if cfg!(windows) {
        "Windows Credential Manager"
    } else if cfg!(target_os = "macos") {
        "the macOS Keychain"
    } else {
        "the system keyring"
    }
}

/// The saved password of `user`, if any. A store that can't be reached counts as none.
pub fn load(user: &str, host: &str) -> Option<String> {
    backend::get(SERVICE, &account(user, host)).filter(|p| !p.is_empty())
}

pub fn save(user: &str, host: &str, password: &str) -> Result<(), String> {
    backend::set(SERVICE, &account(user, host), password)
}

/// Deletes the saved password of `user`; nothing saved is not an error.
pub fn forget(user: &str, host: &str) -> Result<(), String> {
    backend::delete(SERVICE, &account(user, host))
}

#[cfg_attr(test, allow(dead_code))]
mod os {
    use keyring::{Entry, Error};

    pub fn get(service: &str, account: &str) -> Option<String> {
        Entry::new(service, account).ok()?.get_password().ok()
    }

    pub fn set(service: &str, account: &str, password: &str) -> Result<(), String> {
        Entry::new(service, account)
            .and_then(|e| e.set_password(password))
            .map_err(|e| e.to_string())
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        match Entry::new(service, account).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(not(test))]
use os as backend;

#[cfg(test)]
mod backend {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn map() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
        static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
        MAP.get_or_init(Default::default).lock().unwrap()
    }

    pub fn get(service: &str, account: &str) -> Option<String> {
        map().get(&format!("{service}/{account}")).cloned()
    }

    pub fn set(service: &str, account: &str, password: &str) -> Result<(), String> {
        map().insert(format!("{service}/{account}"), password.to_string());
        Ok(())
    }

    pub fn delete(service: &str, account: &str) -> Result<(), String> {
        map().remove(&format!("{service}/{account}"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_loads_and_forgets() {
        assert_eq!(load("u-secret-test", "h"), None);
        save(" u-secret-test ", "h", "p w").unwrap();
        assert_eq!(load("u-secret-test", "h").as_deref(), Some("p w"));
        assert_eq!(load("u-secret-test", "other-host"), None);
        forget("u-secret-test", "h").unwrap();
        forget("u-secret-test", "h").unwrap();
        assert_eq!(load("u-secret-test", "h"), None);
    }

    /// Round trip through the real credential store with a throwaway entry
    /// (service `amanece-selftest`), deleted again at the end.
    #[test]
    #[ignore]
    fn live_os_store_roundtrip() {
        let (svc, acct) = ("amanece-selftest", "probe@localhost");
        os::set(svc, acct, "not-a-real-password").expect("store reachable");
        assert_eq!(os::get(svc, acct).as_deref(), Some("not-a-real-password"));
        os::delete(svc, acct).unwrap();
        assert_eq!(os::get(svc, acct), None);
        os::delete(svc, acct).unwrap();
    }
}
