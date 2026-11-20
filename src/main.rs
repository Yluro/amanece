mod app;
mod cache;
mod cbf;
mod cells;
mod checkcif;
mod config;
mod console;
mod csd;
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
mod update;

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
    /// SFTP host of a server made from the command line (ALBA's, unless --type/--nas)
    #[arg(long)]
    host: Option<String>,
    /// SFTP port of that server (default ALBA's: 443)
    #[arg(long)]
    port: Option<u16>,
    /// Start without the login dialog (cached data and local files only)
    #[arg(long)]
    offline: bool,
    /// Type of that server: alba (default), bruker, or folders (plain folders, like a NAS)
    #[arg(long = "type", value_enum)]
    server_type: Option<config::ServerKind>,
    /// Same as --type folders
    #[arg(long, conflicts_with = "server_type")]
    nas: bool,
    /// Where browsing starts (folders; default the login folder) or ALBA's proposal folder
    #[arg(long, visible_alias = "root")]
    remote_root: Option<String>,
    /// Server to select first in the login dialog (a name from config.toml's [[servers]])
    #[arg(long)]
    server: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (mut cfg, cfg_path) = config::Config::load()?;
    // Command-line server flags make one more server, selected first: ALBA's, changed.
    let kind = if args.nas { Some(config::ServerKind::Folders) } else { args.server_type };
    let mut servers = cfg.server_list();
    if args.host.is_some() || args.port.is_some() || kind.is_some() || args.remote_root.is_some() {
        let mut p = cfg.builtin_servers().remove(0);
        p.name = "command line".into();
        if let Some(h) = args.host {
            p.host = h;
        }
        if let Some(port) = args.port {
            p.port = port;
        }
        if let Some(k) = kind {
            p.kind = k;
            if k.source_ids() {
                p.root = None; // ALBA's proposal folder means nothing here
            }
        }
        if let Some(root) = args.remote_root {
            p.root = Some(root);
        }
        servers.insert(0, p);
    }
    if let Some(name) = &args.server {
        let i = servers
            .iter()
            .position(|s| &s.name == name)
            .ok_or_else(|| anyhow::anyhow!("no server named \"{name}\" in config.toml or on the command line"))?;
        let s = servers.remove(i);
        servers.insert(0, s);
    }
    config::Config::check_servers(&servers)?;
    if servers.iter().any(|s| s.kind.source_ids()) {
        cfg.check_updates = false; // Do not replace this local build with an upstream release.
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
            rate: cfg.request_rate(),
            burst: cfg.burst,
            known_hosts: config::Config::known_hosts_path(),
            beamlines: cfg.beamlines.clone(),
            parallel_downloads: cfg.parallel_downloads,
            background_gap: cfg.background_gap(),
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

    let mut app = app::App::new(cfg, servers, cfg_path, base, remote, tx, args.user, args.offline);
    app.start_update_checks();
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
