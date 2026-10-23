//! Console programs run in a pseudo-terminal (Windows ConPTY), so they behave as in a
//! real console: prompts show up before the program waits for input, and the lines you
//! type are delivered as keystrokes (SADABS, interactive SHELX runs, cmd builtins …).
//! A vt100 parser keeps the program's screen; the console panel draws it live and keeps
//! a plain-text transcript once the program exits.

use crate::console::ConsoleEvent;
use crate::event::AppEvent;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Tall screen so output stays on it; the panel shows the part around the cursor.
pub const ROWS: u16 = 200;
const SCROLLBACK: usize = 50_000;
/// A second Ctrl-C within this time kills the program instead of interrupting it.
const KILL_WINDOW: Duration = Duration::from_secs(2);

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct PtyRun {
    pub label: String,
    pub parser: Arc<Mutex<vt100::Parser>>,
    master: Box<dyn MasterPty + Send>,
    writer: SharedWriter,
    killer: Box<dyn ChildKiller + Send + Sync>,
    cols: u16,
    interrupted: Option<Instant>,
}

fn nz(v: u16) -> NonZeroU16 {
    NonZeroU16::new(v.max(1)).unwrap_or(NonZeroU16::MIN)
}

impl PtyRun {
    pub fn spawn(mut cmd: CommandBuilder, label: &str, cwd: &Path, cols: u16, tx: Sender<AppEvent>) -> anyhow::Result<PtyRun> {
        let cols = cols.max(20);
        let pair = native_pty_system().openpty(PtySize {
            rows: ROWS,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        cmd.cwd(cwd);
        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(nz(ROWS), nz(cols), SCROLLBACK)));

        let (p, w, out_tx) = (parser.clone(), writer.clone(), tx.clone());
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                let n = match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                let chunk = &buf[..n];
                let cursor = match p.lock() {
                    Ok(mut parser) => {
                        parser.process(chunk);
                        parser.screen().cursor_position()
                    }
                    Err(_) => break,
                };
                // ConPTY asks where the cursor is (ESC[6n) and waits for the answer.
                if contains(chunk, b"\x1b[6n")
                    && let Ok(mut w) = w.lock()
                {
                    // One write: ConPTY reads a split reply as separate keys.
                    let reply = format!("\x1b[{};{}R", cursor.0 + 1, cursor.1 + 1);
                    let _ = w.write_all(reply.as_bytes());
                    let _ = w.flush();
                }
                if out_tx.send(AppEvent::Console(ConsoleEvent::Output)).is_err() {
                    break;
                }
            }
        });

        let name = label.to_string();
        thread::spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code() as i32);
            // let the reader catch the last output before the transcript is taken
            thread::sleep(Duration::from_millis(250));
            let _ = tx.send(AppEvent::Console(ConsoleEvent::Exited {
                label: name,
                code,
                external: false,
            }));
        });

        Ok(PtyRun {
            label: label.to_string(),
            parser,
            master: pair.master,
            writer,
            killer,
            cols,
            interrupted: None,
        })
    }

    /// Types `line` and Enter into the program.
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| std::io::Error::other("terminal writer poisoned"))?;
        w.write_all(line.as_bytes())?;
        w.write_all(b"\r")?;
        w.flush()
    }

    /// First Ctrl-C is passed to the program; a second one within 2 s kills it.
    /// Returns true when the program was killed.
    pub fn interrupt(&mut self) -> bool {
        if self.interrupted.is_some_and(|t| t.elapsed() < KILL_WINDOW) {
            let _ = self.killer.kill();
            return true;
        }
        self.interrupted = Some(Instant::now());
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(b"\x03");
            let _ = w.flush();
        }
        false
    }

    pub fn resize(&mut self, cols: u16) {
        let cols = cols.max(20);
        if cols == self.cols {
            return;
        }
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows: ROWS,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.screen_mut().set_size(nz(ROWS), nz(cols));
        }
    }

    /// Everything the program printed, as plain text lines.
    pub fn transcript(&self) -> Vec<String> {
        let Ok(mut p) = self.parser.lock() else {
            return Vec::new();
        };
        transcript(&mut p)
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Scrollback (oldest first) followed by the screen, trailing blank lines dropped.
pub fn transcript(parser: &mut vt100::Parser) -> Vec<String> {
    let screen = parser.screen_mut();
    let (rows, cols) = screen.size();
    let rows = usize::from(rows.get());
    let mut out = Vec::new();
    let mut off = screen.scrollback_len();
    while off > 0 {
        screen.set_scrollback(off);
        let take = off.min(rows);
        out.extend(screen.rows(0, cols.get()).take(take));
        off -= take;
    }
    screen.set_scrollback(0);
    out.extend(screen.rows(0, cols.get()));
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out
}

/// Last screen row with text (or the cursor row, if lower).
pub fn last_used_row(screen: &vt100::Screen) -> u16 {
    let (rows, cols) = screen.size();
    let cursor = screen.cursor_position().0;
    (0..rows.get())
        .rev()
        .find(|&r| (0..cols.get()).any(|c| screen.cell(r, c).is_some_and(|cell| cell.has_contents())))
        .unwrap_or(0)
        .max(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_includes_scrollback() {
        let mut p = vt100::Parser::new(nz(3), nz(20), 100);
        for i in 0..7 {
            p.process(format!("line {i}\r\n").as_bytes());
        }
        let t = transcript(&mut p);
        assert_eq!(t.first().map(String::as_str), Some("line 0"));
        assert_eq!(t.last().map(String::as_str), Some("line 6"));
        assert_eq!(t.len(), 7);
    }
}

#[cfg(test)]
/// Opt-in: `cargo test sadabs_prompts_show -- --ignored --nocapture` shows SADABS's first
/// question appearing in the pseudo-terminal before any input.
mod sadabs_check {
    #[test]
    #[ignore]
    fn sadabs_prompts_show() {
        let exe = std::path::Path::new(r"C:\Program Files\ShelX\sadabs.exe");
        if !exe.exists() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("alba_watch_sadabs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let cmd = portable_pty::CommandBuilder::new(exe);
        let mut run = super::PtyRun::spawn(cmd, "sadabs", &dir, 100, tx).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(4));
        let text = run.parser.lock().unwrap().screen().contents();
        for l in text.lines().filter(|l| !l.trim().is_empty()).take(30) {
            println!("| {l}");
        }
        run.interrupt();
        run.interrupt();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let _ = std::fs::remove_dir_all(&dir);
        std::process::exit(0);
    }
}
