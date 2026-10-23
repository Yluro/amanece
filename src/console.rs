//! Bottom panel: a simple command runner (no PTY). Output streams into the scrollback;
//! programs listed in `external_window` (XPREP) open in their own console window.

use crate::event::AppEvent;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Sender;
use std::thread;

const MAX_LINES: usize = 3000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Cmd,
    Out,
    Err,
    Info,
    Good,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct ConsoleLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug)]
pub enum ConsoleEvent {
    Line(LineKind, String),
    Exited {
        label: String,
        code: Option<i32>,
        external: bool,
    },
}

pub struct Running {
    pub pid: u32,
    pub label: String,
}

/// What `submit` asks the app to do for builtins.
pub enum Builtin {
    Cd(String),
    Clear,
    Help,
}

#[derive(Default)]
pub struct Console {
    pub lines: VecDeque<ConsoleLine>,
    pub input: String,
    /// Cursor position in chars.
    pub cursor: usize,
    pub history: Vec<String>,
    hist_pos: Option<usize>,
    pub running: Option<Running>,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
    log: Option<std::fs::File>,
    started: Option<std::time::Instant>,
}

impl Console {
    /// Mirror every console line into a log file (appending, local only).
    pub fn open_log(&mut self, path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        self.log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        self.started = Some(std::time::Instant::now());
        let unix = crate::model::now_secs();
        self.log_only(&format!(
            "==== alba_watch {} started (unix time {unix})",
            env!("CARGO_PKG_VERSION")
        ));
        let existing: Vec<String> = self.lines.iter().map(|l| l.text.clone()).collect();
        for l in existing {
            self.log_only(&l);
        }
    }

    pub fn log_only(&mut self, text: &str) {
        use std::io::Write;
        let t = self.started.map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
        if let Some(f) = &mut self.log {
            let _ = writeln!(f, "{t:>9.1}s {text}");
        }
    }

    pub fn push(&mut self, kind: LineKind, text: impl Into<String>) {
        let text = text.into();
        self.log_only(&text);
        for l in text.split('\n') {
            self.lines.push_back(ConsoleLine {
                kind,
                text: l.trim_end_matches('\r').to_string(),
            });
            // Keep the view still while the user is scrolled up.
            if self.scroll > 0 {
                self.scroll += 1;
            }
        }
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
        self.scroll = self.scroll.min(self.lines.len());
    }

    pub fn insert_char(&mut self, c: char) {
        let i = self.byte_index();
        self.input.insert(i, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor -= 1;
        let i = self.byte_index();
        self.input.remove(i);
    }

    pub fn delete(&mut self) {
        if self.cursor < self.input.chars().count() {
            let i = self.byte_index();
            self.input.remove(i);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.input.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.input.chars().count();
    }

    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.hist_pos {
            None => self.history.len() - 1,
            Some(p) => p.saturating_sub(1),
        };
        self.hist_pos = Some(pos);
        self.input = self.history[pos].clone();
        self.end();
    }

    pub fn history_next(&mut self) {
        match self.hist_pos {
            Some(p) if p + 1 < self.history.len() => {
                self.hist_pos = Some(p + 1);
                self.input = self.history[p + 1].clone();
            }
            _ => {
                self.hist_pos = None;
                self.input.clear();
            }
        }
        self.end();
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.lines.len());
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Run the input line in `cwd`. A bare program name gets the file selected in the
    /// Local panel as its argument (`xprep` → `xprep XDS_ASCII.HKL`; SHELX programs get
    /// the name without extension). Windows GUI programs such as XPREP are started
    /// directly and keep their own window; console programs stream into the panel.
    /// Returns a builtin for the app to handle.
    pub fn submit(
        &mut self,
        cwd: &Path,
        selected: Option<&str>,
        force_detached: impl Fn(&str) -> bool,
        tx: &Sender<AppEvent>,
    ) -> Option<Builtin> {
        let typed = self.input.trim().to_string();
        self.input.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.scroll = 0;
        if typed.is_empty() {
            return None;
        }
        if self.history.last() != Some(&typed) {
            self.history.push(typed.clone());
        }

        let mut parts = typed.splitn(2, char::is_whitespace);
        let program = parts.next().unwrap_or_default().to_string();
        match program.to_ascii_lowercase().as_str() {
            "cd" => {
                self.push(LineKind::Cmd, format!("> {typed}"));
                return Some(Builtin::Cd(parts.next().unwrap_or_default().trim().to_string()));
            }
            "clear" | "cls" => return Some(Builtin::Clear),
            "help" => return Some(Builtin::Help),
            _ => {}
        }
        let line = with_selected(&typed, selected);
        self.push(LineKind::Cmd, format!("> {line}"));

        let exe = resolve_program(&program, cwd);
        let gui = exe.as_deref().is_some_and(is_gui_exe);
        if gui || force_detached(&program) {
            let args = split_args(line.splitn(2, char::is_whitespace).nth(1).unwrap_or_default());
            let target = exe.unwrap_or_else(|| program.clone().into());
            match spawn_detached(&target, &args, &program, cwd, gui, tx.clone()) {
                Ok(()) => self.push(LineKind::Info, format!("{program} started in its own window")),
                Err(e) => self.push(LineKind::Error, format!("cannot start {program}: {e}")),
            }
            return None;
        }

        if let Some(r) = &self.running {
            self.push(
                LineKind::Warn,
                format!("`{}` is still running (Ctrl-C to stop it)", r.label),
            );
            return None;
        }
        match spawn_piped(&line, cwd, tx.clone()) {
            Ok(pid) => {
                self.running = Some(Running {
                    pid,
                    label: program,
                })
            }
            Err(e) => self.push(LineKind::Error, format!("cannot run: {e}")),
        }
        None
    }

    pub fn kill_running(&mut self) {
        if let Some(r) = &self.running {
            kill_tree(r.pid);
            let label = r.label.clone();
            self.push(LineKind::Warn, format!("stopping {label}…"));
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub fn shell(line: &str) -> Command {
        let mut c = Command::new("cmd");
        c.raw_arg("/C").raw_arg(line).creation_flags(CREATE_NO_WINDOW);
        c
    }

    /// GUI programs open their own window; a console program forced into
    /// `external_window` gets a console window of its own instead of ours.
    pub fn detached(exe: &std::path::Path, gui: bool) -> Command {
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        let mut c = Command::new(exe);
        if !gui {
            c.creation_flags(CREATE_NEW_CONSOLE);
        }
        c
    }

    pub fn kill_tree(pid: u32) {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
}

#[cfg(not(windows))]
mod sys {
    use std::process::Command;

    pub fn shell(line: &str) -> Command {
        let mut c = Command::new("sh");
        c.arg("-c").arg(line);
        c
    }

    pub fn detached(exe: &std::path::Path, _gui: bool) -> Command {
        Command::new(exe)
    }

    pub fn kill_tree(pid: u32) {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

fn kill_tree(pid: u32) {
    sys::kill_tree(pid);
}

fn spawn_piped(line: &str, cwd: &Path, tx: Sender<AppEvent>) -> std::io::Result<u32> {
    let mut child = sys::shell(line)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let out = child.stdout.take().map(|s| pump(s, LineKind::Out, tx.clone()));
    let err = child.stderr.take().map(|s| pump(s, LineKind::Err, tx.clone()));
    let label = line.split_whitespace().next().unwrap_or_default().to_string();
    thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code());
        for h in [out, err].into_iter().flatten() {
            let _ = h.join();
        }
        let _ = tx.send(AppEvent::Console(ConsoleEvent::Exited {
            label,
            code,
            external: false,
        }));
    });
    Ok(pid)
}

fn spawn_detached(
    exe: &Path,
    args: &[String],
    label: &str,
    cwd: &Path,
    gui: bool,
    tx: Sender<AppEvent>,
) -> std::io::Result<()> {
    let mut child = sys::detached(exe, gui)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let label = label.to_string();
    thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code());
        let _ = tx.send(AppEvent::Console(ConsoleEvent::Exited {
            label,
            code,
            external: true,
        }));
    });
    Ok(())
}

const SHELX: [&str; 6] = ["shelxt", "shelxl", "shelxs", "shelxd", "shelxe", "shelxc"];

/// Appends the selected file to a bare program name. SHELX programs take the
/// job name, so they get the file name without its extension.
pub fn with_selected(line: &str, selected: Option<&str>) -> String {
    let line = line.trim();
    let Some(file) = selected else {
        return line.to_string();
    };
    if line.contains(char::is_whitespace) {
        return line.to_string();
    }
    let program = line.to_ascii_lowercase();
    let program = program.trim_end_matches(".exe");
    let arg = if SHELX.contains(&program) {
        match file.rfind('.') {
            Some(i) if i > 0 => &file[..i],
            _ => file,
        }
    } else {
        file
    };
    if arg.contains(char::is_whitespace) {
        format!("{line} \"{arg}\"")
    } else {
        format!("{line} {arg}")
    }
}

/// Splits arguments on whitespace, keeping "quoted parts" together.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(cur);
    }
    out
}

/// Finds a program the way the shell would: the folder itself, then PATH.
pub fn resolve_program(program: &str, cwd: &Path) -> Option<std::path::PathBuf> {
    let has_ext = Path::new(program).extension().is_some();
    let names: Vec<String> = if has_ext || !cfg!(windows) {
        vec![program.to_string()]
    } else {
        [".exe", ".com", ".bat", ".cmd"]
            .iter()
            .map(|e| format!("{program}{e}"))
            .collect()
    };
    let mut dirs = vec![cwd.to_path_buf()];
    if !program.contains(['/', '\\']) {
        dirs.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
    }
    dirs.iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// True for Windows executables built for the GUI subsystem (they open their own window).
pub fn is_gui_exe(path: &Path) -> bool {
    use std::io::Read;
    let is_exe = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if !is_exe {
        return false;
    }
    let mut head = [0u8; 1024];
    let Ok(n) = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) else {
        return false;
    };
    let head = &head[..n];
    if head.len() < 0x40 || &head[..2] != b"MZ" {
        return false;
    }
    let pe = u32::from_le_bytes([head[0x3C], head[0x3D], head[0x3E], head[0x3F]]) as usize;
    // PE signature, 20-byte COFF header, then the subsystem at offset 68 of the optional header
    let sub = pe + 4 + 20 + 68;
    head.len() >= sub + 2
        && &head[pe..pe + 4] == b"PE\0\0"
        && u16::from_le_bytes([head[sub], head[sub + 1]]) == 2
}

fn pump(stream: impl Read + Send + 'static, kind: LineKind, tx: Sender<AppEvent>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for chunk in reader.split(b'\n').map_while(Result::ok) {
            let text = String::from_utf8_lossy(&chunk);
            // Progress lines overwrite themselves with '\r': keep the last state.
            let text = text
                .trim_end_matches('\r')
                .rsplit('\r')
                .next()
                .unwrap_or_default()
                .to_string();
            if tx.send(AppEvent::Console(ConsoleEvent::Line(kind, text))).is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing() {
        let mut c = Console::default();
        for ch in "shelx".chars() {
            c.insert_char(ch);
        }
        c.insert_char('t');
        c.left();
        c.left();
        c.backspace();
        assert_eq!(c.input, "shext");
        c.home();
        c.delete();
        assert_eq!(c.input, "hext");
    }

    #[test]
    fn runs_a_command_and_reports_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut c = Console::default();
        c.input = "echo hello".into();
        assert!(c.submit(&std::env::temp_dir(), None, |_| false, &tx).is_none());
        let mut got_line = false;
        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap() {
                AppEvent::Console(ConsoleEvent::Line(LineKind::Out, t)) => {
                    got_line |= t.trim() == "hello"
                }
                AppEvent::Console(ConsoleEvent::Exited { code, .. }) => {
                    assert_eq!(code, Some(0));
                    break;
                }
                _ => {}
            }
        }
        assert!(got_line);
    }

    #[test]
    fn selected_file_is_passed() {
        assert_eq!(with_selected("xprep", Some("XDS_ASCII.HKL")), "xprep XDS_ASCII.HKL");
        assert_eq!(with_selected("shelxt", Some("shelxt.hkl")), "shelxt shelxt");
        assert_eq!(with_selected("SHELXL.exe", Some("JSG300.ins")), "SHELXL.exe JSG300");
        assert_eq!(with_selected("xprep", Some("my data.hkl")), "xprep \"my data.hkl\"");
        // typed arguments win; nothing selected: unchanged
        assert_eq!(with_selected("xprep other.hkl", Some("XDS_ASCII.HKL")), "xprep other.hkl");
        assert_eq!(with_selected("dir", None), "dir");
    }

    #[test]
    fn splits_quoted_args() {
        assert_eq!(split_args(r#"a "b c"  d"#), vec!["a", "b c", "d"]);
        assert!(split_args("   ").is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn detects_gui_programs() {
        let xprep = std::path::Path::new(r"C:\Program Files\ShelX\xprep.exe");
        if xprep.exists() {
            assert!(is_gui_exe(xprep));
            assert!(!is_gui_exe(std::path::Path::new(r"C:\Program Files\ShelX\shelxt.exe")));
        }
        let cmd = resolve_program("cmd", Path::new(".")).expect("cmd on PATH");
        assert!(!is_gui_exe(&cmd));
        let notepad = resolve_program("notepad", Path::new(".")).expect("notepad on PATH");
        assert!(is_gui_exe(&notepad));
    }

    #[test]
    fn builtins() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Console::default();
        c.input = "cd JSG285b".into();
        assert!(matches!(
            c.submit(Path::new("."), Some("x.hkl"), |_| false, &tx),
            Some(Builtin::Cd(d)) if d == "JSG285b"
        ));
    }
}
