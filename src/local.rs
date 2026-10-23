//! Left panel: a plain local directory browser.

use std::path::{Path, PathBuf};

pub const SOURCE_FILE: &str = ".alba_source.json";

#[derive(Debug, Clone)]
pub struct LocalEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Folder holds a downloaded solve, or a sample folder holding one.
    pub downloaded: bool,
}

pub struct LocalPanel {
    pub cwd: PathBuf,
    pub entries: Vec<LocalEntry>,
    pub selected: usize,
    pub error: Option<String>,
}

impl LocalPanel {
    pub fn new(cwd: PathBuf) -> Self {
        let mut p = LocalPanel {
            cwd,
            entries: Vec::new(),
            selected: 0,
            error: None,
        };
        p.refresh();
        p
    }

    pub fn refresh(&mut self) {
        let keep = self.selected_entry().map(|e| e.name.clone());
        self.entries.clear();
        self.error = None;
        if self.cwd.parent().is_some() {
            self.entries.push(LocalEntry {
                name: "..".into(),
                is_dir: true,
                size: 0,
                downloaded: false,
            });
        }
        let sources = crate::download::read_sources(&self.cwd);
        match std::fs::read_dir(&self.cwd) {
            Ok(rd) => {
                let mut items: Vec<LocalEntry> = rd
                    .flatten()
                    .filter_map(|e| {
                        let name = e.file_name().to_string_lossy().to_string();
                        if name == ".alba_watch" || name == SOURCE_FILE {
                            return None;
                        }
                        let md = e.metadata().ok()?;
                        let is_dir = md.is_dir();
                        let downloaded = if is_dir {
                            holds_download(&e.path())
                        } else {
                            sources.files.contains_key(&name)
                        };
                        Some(LocalEntry {
                            name,
                            is_dir,
                            size: md.len(),
                            downloaded,
                        })
                    })
                    .collect();
                items.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                self.entries.extend(items);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.selected = keep
            .and_then(|k| self.entries.iter().position(|e| e.name == k))
            .unwrap_or(0)
            .min(self.entries.len().saturating_sub(1));
    }

    pub fn selected_entry(&self) -> Option<&LocalEntry> {
        self.entries.get(self.selected)
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.selected_entry().map(|e| self.cwd.join(&e.name))
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let max = self.entries.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub fn select_name(&mut self, name: &str) {
        if let Some(i) = self.entries.iter().position(|e| e.name == name) {
            self.selected = i;
        }
    }

    /// The selected entry's name when it is a file (what console commands receive).
    pub fn selected_file(&self) -> Option<&str> {
        self.selected_entry()
            .filter(|e| !e.is_dir)
            .map(|e| e.name.as_str())
    }

    /// Creates `name` inside the current folder and selects it.
    pub fn make_dir(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name == "." || name == ".." {
            return Err("empty folder name".into());
        }
        if name.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')) {
            return Err(format!("not a valid folder name: {name}"));
        }
        let path = self.cwd.join(name);
        if path.exists() {
            return Err(format!("{name} already exists"));
        }
        std::fs::create_dir(&path).map_err(|e| format!("cannot create {name}: {e}"))?;
        self.refresh();
        self.select_name(name);
        Ok(())
    }

    pub fn select_last(&mut self) {
        self.selected = self.entries.len().saturating_sub(1);
    }

    /// Enter the selected directory (or go up for `..`). Returns false for files.
    pub fn enter(&mut self) -> bool {
        let Some(e) = self.selected_entry().cloned() else {
            return false;
        };
        if !e.is_dir {
            return false;
        }
        if e.name == ".." {
            self.up();
        } else {
            self.cd(self.cwd.join(&e.name));
        }
        true
    }

    pub fn up(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            let from = self
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            self.cwd = parent;
            self.selected = 0;
            self.refresh();
            if let Some(from) = from
                && let Some(i) = self.entries.iter().position(|e| e.name == from)
            {
                self.selected = i;
            }
        }
    }

    pub fn cd(&mut self, dir: PathBuf) {
        let dir = dir.canonicalize().map(strip_verbatim).unwrap_or(dir);
        if dir.is_dir() {
            self.cwd = dir;
            self.selected = 0;
            self.refresh();
        } else {
            self.error = Some(format!("not a directory: {}", dir.display()));
        }
    }
}

/// "XDS_ASCII.HKL (18.7 MB)" or "folder xia2chem_r3 (3 files, 5.6 MB)".
pub fn describe(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    if path.is_dir() {
        let (mut files, mut bytes) = (0u64, 0u64);
        let mut stack = vec![path.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                match e.metadata() {
                    Ok(m) if m.is_dir() => stack.push(e.path()),
                    Ok(m) => {
                        files += 1;
                        bytes += m.len();
                    }
                    Err(_) => {}
                }
            }
            if files > 100_000 {
                break;
            }
        }
        let s = if files == 1 { "" } else { "s" };
        format!("folder {name} ({files} file{s}, {})", crate::model::human_bytes(bytes))
    } else {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        format!("{name} ({})", crate::model::human_bytes(size))
    }
}

fn holds_download(dir: &Path) -> bool {
    if dir.join(SOURCE_FILE).exists() {
        return true;
    }
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().any(|e| e.path().join(SOURCE_FILE).exists()))
        .unwrap_or(false)
}

/// `canonicalize` on Windows returns `\\?\C:\...`; strip that for display and for child processes.
pub fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_dir_creates_and_selects() {
        let base = std::env::temp_dir().join(format!("alba_watch_local_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut p = LocalPanel::new(base.clone());
        p.make_dir("JSG300").unwrap();
        assert!(base.join("JSG300").is_dir());
        assert_eq!(p.selected_entry().unwrap().name, "JSG300");
        assert!(p.make_dir("JSG300").is_err());
        assert!(p.make_dir("a/b").is_err());
        std::fs::write(base.join("XDS_ASCII.HKL"), b"x").unwrap();
        p.refresh();
        p.select_name("XDS_ASCII.HKL");
        assert_eq!(p.selected_file(), Some("XDS_ASCII.HKL"));
        p.select_name("JSG300");
        assert_eq!(p.selected_file(), None);
        assert_eq!(describe(&base.join("XDS_ASCII.HKL")), "XDS_ASCII.HKL (1 B)");
        std::fs::write(base.join("JSG300").join("a.hkl"), b"12345").unwrap();
        assert_eq!(describe(&base.join("JSG300")), "folder JSG300 (1 file, 5 B)");
        std::fs::remove_dir_all(&base).ok();
    }
}
