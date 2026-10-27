//! Bottom panel: a terminal. Console programs run in a pseudo-terminal (see `pty.rs`),
//! so interactive ones (SADABS, …) work: while a program runs, each line you type plus
//! Enter is its answer. Windows GUI programs (XPREP) start directly in their own window.

use crate::event::AppEvent;
use crate::pty::PtyRun;
use portable_pty::CommandBuilder;
use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::mpsc::Sender;
use std::thread;

const MAX_LINES: usize = 3000;
/// Longer lines (a program printing without newlines) are cut.
const MAX_LINE_CHARS: usize = 4000;
/// At startup a log above `LOG_MAX` bytes is cut down to its last `LOG_KEEP` bytes.
const LOG_MAX: u64 = 5 << 20;
const LOG_KEEP: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Cmd,
    Out,
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
    /// The running program printed something (redraw).
    Output,
    Exited {
        label: String,
        code: Option<i32>,
        external: bool,
    },
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
    /// The console program currently running, if any.
    pub pty: Option<PtyRun>,
    /// Terminal width in columns (the panel's inner width).
    pub cols: u16,
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
        trim_log(path);
        self.log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
        self.started = Some(std::time::Instant::now());
        let unix = crate::model::now_secs();
        self.log_only(&format!(
            "==== amanece {} started (unix time {unix})",
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
            let l = l.trim_end_matches('\r');
            let text = match l.char_indices().nth(MAX_LINE_CHARS) {
                Some((cut, _)) => format!("{} … [{} more bytes]", &l[..cut], l.len() - cut),
                None => l.to_string(),
            };
            self.lines.push_back(ConsoleLine { kind, text });
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
        // While a program runs, the line (even an empty one) is its answer.
        if self.pty.is_some() {
            let answer = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.scroll = 0;
            let failed = self.pty.as_mut().and_then(|run| {
                run.send_line(&answer)
                    .err()
                    .map(|e| format!("cannot send to {}: {e}", run.label))
            });
            if let Some(msg) = failed {
                self.push(LineKind::Error, msg);
            }
            return None;
        }
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

        match PtyRun::spawn(pty_command(&line, exe.as_deref()), &program, cwd, self.cols, tx.clone()) {
            Ok(run) => self.pty = Some(run),
            Err(e) => self.push(LineKind::Error, format!("cannot run {program}: {e}")),
        }
        None
    }

    /// Ctrl-C: interrupt the running program; a second Ctrl-C within 2 s kills it.
    pub fn interrupt(&mut self) {
        let Some(run) = self.pty.as_mut() else {
            return;
        };
        let label = run.label.clone();
        let msg = if run.interrupt() {
            format!("killed {label}")
        } else {
            format!("Ctrl-C sent to {label} (again within 2 s to kill it)")
        };
        self.push(LineKind::Warn, msg);
    }

    /// Stops the running program right away (quitting the app).
    pub fn kill(&mut self) {
        if let Some(run) = &mut self.pty {
            run.interrupt();
            run.interrupt();
        }
    }

    /// The program exited: keep its output as text in the log.
    pub fn finish_run(&mut self) {
        if let Some(run) = self.pty.take() {
            for line in run.transcript() {
                self.push(LineKind::Out, line);
            }
        }
    }
}

/// `cmd /C <line>` for shell builtins and scripts; executables run directly.
fn pty_command(line: &str, exe: Option<&Path>) -> CommandBuilder {
    let rest = line.splitn(2, char::is_whitespace).nth(1).unwrap_or_default();
    match exe {
        Some(p)
            if p
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("exe") || e.eq_ignore_ascii_case("com")) =>
        {
            let mut c = CommandBuilder::new(p);
            c.args(split_args(rest));
            c
        }
        _ if cfg!(windows) => {
            let mut c = CommandBuilder::new("cmd.exe");
            c.arg("/C");
            c.args(split_args(line));
            c
        }
        _ => {
            let mut c = CommandBuilder::new("sh");
            c.arg("-c");
            c.arg(line);
            c
        }
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

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
}

#[cfg(not(windows))]
mod sys {
    use std::process::Command;

    pub fn detached(exe: &std::path::Path, _gui: bool) -> Command {
        Command::new(exe)
    }
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

/// Keeps the log small: above `LOG_MAX`, only the last `LOG_KEEP` bytes (from a line
/// start) are kept.
fn trim_log(path: &Path) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(len) = std::fs::metadata(path).map(|m| m.len()) else {
        return;
    };
    if len <= LOG_MAX {
        return;
    }
    let Ok(mut f) = std::fs::File::open(path) else {
        return;
    };
    let mut tail = Vec::new();
    if f.seek(SeekFrom::End(-(LOG_KEEP as i64))).is_err() || f.read_to_end(&mut tail).is_err() {
        return;
    }
    drop(f);
    let start = tail.iter().position(|&b| b == b'\n').map_or(tail.len(), |i| i + 1);
    let _ = std::fs::write(path, &tail[start..]);
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

    /// Pumps console events until the running program exits (or `timeout`).
    fn wait_exit(c: &mut Console, rx: &std::sync::mpsc::Receiver<AppEvent>, timeout: u64) -> Option<i32> {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
        while std::time::Instant::now() < end {
            if let Ok(AppEvent::Console(ConsoleEvent::Exited { code, .. })) =
                rx.recv_timeout(std::time::Duration::from_millis(200))
            {
                c.finish_run();
                return code;
            }
        }
        None
    }

    fn screen_text(c: &Console) -> String {
        c.pty
            .as_ref()
            .map(|r| r.parser.lock().unwrap().screen().contents())
            .unwrap_or_default()
    }

    #[test]
    fn runs_a_command_in_the_terminal() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut c = Console {
            cols: 100,
            ..Console::default()
        };
        c.input = "echo hello".into();
        assert!(c.submit(&std::env::temp_dir(), None, |_| false, &tx).is_none());
        assert_eq!(wait_exit(&mut c, &rx, 20), Some(0));
        assert!(c.lines.iter().any(|l| l.text.trim() == "hello"), "{:?}", c.lines);
    }

    #[cfg(windows)]
    #[test]
    fn answers_reach_an_interactive_program() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut c = Console {
            cols: 100,
            ..Console::default()
        };
        c.input = "cmd /V:ON /C set /p X=Name? & echo got !X!".into();
        c.submit(&std::env::temp_dir(), None, |_| false, &tx);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !screen_text(&c).contains("Name?") && std::time::Instant::now() < end {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
        assert!(screen_text(&c).contains("Name?"), "prompt shown before input");
        c.input = "abc".into();
        c.submit(&std::env::temp_dir(), None, |_| false, &tx);
        assert_eq!(wait_exit(&mut c, &rx, 20), Some(0));
        assert!(c.lines.iter().any(|l| l.text.contains("got abc")), "{:?}", c.lines);
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
    fn long_lines_are_cut() {
        let mut c = Console::default();
        c.push(LineKind::Out, "Expert mode (Y or N) [N]: ".repeat(1000));
        let l = &c.lines.back().unwrap().text;
        assert!(l.len() < MAX_LINE_CHARS + 50 && l.ends_with("more bytes]"));
    }

    #[test]
    fn big_logs_are_trimmed_at_startup() {
        let p = std::env::temp_dir().join(format!("amanece_logtrim_{}.log", std::process::id()));
        let line = "x".repeat(99) + "\n";
        std::fs::write(&p, line.repeat(60_000) + "last line\n").unwrap();
        trim_log(&p);
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.len() as u64 <= LOG_KEEP);
        assert!(after.starts_with('x') && after.ends_with("last line\n"));
        std::fs::remove_file(&p).ok();
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
