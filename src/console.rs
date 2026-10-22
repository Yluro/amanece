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

    /// Run the input line. Returns a builtin for the app to handle.
    pub fn submit(
        &mut self,
        cwd: &Path,
        is_external: impl Fn(&str) -> bool,
        tx: &Sender<AppEvent>,
    ) -> Option<Builtin> {
        let line = self.input.trim().to_string();
        self.input.clear();
        self.cursor = 0;
        self.hist_pos = None;
        self.scroll = 0;
        if line.is_empty() {
            return None;
        }
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
        }
        self.push(LineKind::Cmd, format!("> {line}"));

        let mut parts = line.splitn(2, char::is_whitespace);
        let program = parts.next().unwrap_or_default().to_string();
        let rest = parts.next().unwrap_or_default().trim().to_string();
        match program.to_ascii_lowercase().as_str() {
            "cd" => return Some(Builtin::Cd(rest)),
            "clear" | "cls" => return Some(Builtin::Clear),
            "help" => return Some(Builtin::Help),
            _ => {}
        }

        if is_external(&program) {
            match spawn_external(&line, &program, cwd, tx.clone()) {
                Ok(()) => self.push(
                    LineKind::Info,
                    format!("{program} opened in its own window"),
                ),
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

    /// `start` gives the program a real console window of its own; `/WAIT` lets us
    /// report its exit code.
    pub fn external(line: &str, title: &str) -> Command {
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/C start \"{title}\" /WAIT cmd /C {line}"))
            .creation_flags(CREATE_NO_WINDOW);
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

    pub fn external(line: &str, _title: &str) -> Command {
        let mut c = Command::new("x-terminal-emulator");
        c.arg("-e").arg("sh").arg("-c").arg(line);
        c
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

fn spawn_external(line: &str, program: &str, cwd: &Path, tx: Sender<AppEvent>) -> std::io::Result<()> {
    let mut child = sys::external(line, program)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let label = program.to_string();
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
        assert!(c.submit(&std::env::temp_dir(), |_| false, &tx).is_none());
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
    fn builtins() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut c = Console::default();
        c.input = "cd JSG285b".into();
        assert!(matches!(
            c.submit(Path::new("."), |_| false, &tx),
            Some(Builtin::Cd(d)) if d == "JSG285b"
        ));
    }
}
