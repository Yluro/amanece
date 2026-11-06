mod app;
mod cache;
mod cbf;
mod cells;
mod checkcif;
mod config;
mod console;
mod download;
mod event;
mod ispyb;
mod layout;
mod local;
mod model;
mod notify;
mod pty;
mod rank;
mod remote;
mod secret;
mod ui;

use anyhow::Result;
use clap::Parser;
use event::AppEvent;
use ratatui::crossterm::event as ct;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::Duration;

/// Browse ALBA BL13-XALOC autoprocessing results over SFTP and fetch HKL files.
/// The Local panel starts in the current directory; amanece's own files (config,
/// cache, log) live in the user config folder, never in the current directory.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Prefill the login username (overrides a remembered one; this flag stores nothing)
    #[arg(long)]
    user: Option<String>,
    /// SFTP host (default from config: userdata.cells.es)
    #[arg(long)]
    host: Option<String>,
    /// SFTP port (default from config: 443)
    #[arg(long)]
    port: Option<u16>,
    /// Start without the login dialog (cached data and local files only)
    #[arg(long)]
    offline: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (mut cfg, cfg_path) = config::Config::load()?;
    if let Some(h) = args.host {
        cfg.host = h;
    }
    if let Some(p) = args.port {
        cfg.port = p;
    }
    let base = local::strip_verbatim(std::env::current_dir()?.canonicalize()?);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let remote = remote::spawn(
        &rt,
        remote::Settings {
            host: cfg.host.clone(),
            port: cfg.port,
            rate: cfg.request_rate(),
            burst: cfg.burst,
            known_hosts: config::Config::known_hosts_path(),
            beamlines: cfg.beamlines.clone(),
            remote_root: cfg.remote_root.clone(),
            parallel_downloads: cfg.parallel_downloads,
        },
        tx.clone(),
    );

    let input_tx = tx.clone();
    std::thread::spawn(move || {
        loop {
            match ct::poll(Duration::from_millis(250)) {
                Ok(true) => match ct::read() {
                    Ok(ev) => {
                        if input_tx.send(AppEvent::Input(ev)).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => {}
                Err(_) => break,
            }
        }
    });

    let mut app = app::App::new(cfg, cfg_path, base, remote, tx, args.user, args.offline);
    if ui::splash::is_premiere_day() {
        app.egg = Some(ui::splash::Egg::new());
    }
    let mut terminal = ratatui::init();
    mouse_capture(true);
    // ratatui's hook restores the screen on a panic; also release the mouse.
    let restore_screen = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        mouse_capture(false);
        restore_screen(info);
    }));
    let result = run(&mut terminal, &mut app, &rx);
    mouse_capture(false);
    ratatui::restore();
    app.shutdown();
    rt.shutdown_timeout(Duration::from_secs(2));
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut app::App, rx: &mpsc::Receiver<AppEvent>) -> Result<()> {
    loop {
        terminal.draw(|f| ui::draw(f, app))?;
        app.sync_layout();
        // Redraw fast enough for the dialogue to look typed.
        let wait = if app.egg.is_some() { 40 } else { 250 };
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(ev) => app.handle(ev),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(ev) = rx.try_recv() {
            app.handle(ev);
        }
        app.tick();
        if app.quit {
            break;
        }
    }
    Ok(())
}

fn mouse_capture(on: bool) {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use ratatui::crossterm::execute;
    let mut out = std::io::stdout();
    let _ = if on {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    };
}
