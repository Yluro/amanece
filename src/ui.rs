//! Rendering. Reads `App`, never mutates it.

use crate::app::{AlbaRow, App, Badge, Conn, Dialog, Focus, ListHit, StatsRow};
use crate::config::PollMode;
use crate::layout;
use crate::console::LineKind;
use crate::model::{SampleInfo, Shell, Solve, Status, human_bytes};
use crate::rank;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap};
use std::time::Instant;

const ACCENT: Color = Color::Cyan;
/// Width of the Stats panel in the three-column layout: the table and shell lines fit.
const STATS_WIDTH: u16 = 76;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    // The console grows while you type in it.
    let console_h = if app.focus == Focus::Console {
        (area.height * 2 / 5).clamp(12, 24)
    } else {
        (area.height / 4).clamp(9, 13)
    }
    .min(area.height.saturating_sub(10).max(4));
    let transfer_h = if app.downloads.groups.is_empty() { 0 } else { 1 };
    let [main, console, transfer, status] = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(console_h),
        Constraint::Length(transfer_h),
        Constraint::Length(1),
    ])
    .areas(area);

    let (local_a, alba_a, stats_a) = if main.width >= 150 {
        let [a, b, c] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Fill(1),
            Constraint::Length(STATS_WIDTH),
        ])
        .areas(main);
        (a, b, c)
    } else {
        let [a, right] =
            Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(66)]).areas(main);
        let [b, c] =
            Layout::vertical([Constraint::Percentage(38), Constraint::Percentage(62)]).areas(right);
        (a, b, c)
    };

    draw_local(f, app, local_a);
    draw_alba(f, app, alba_a);
    draw_stats(f, app, stats_a);
    draw_console(f, app, console);
    if transfer_h > 0 {
        draw_transfer(f, app, transfer);
    }
    draw_status(f, app, status);
    if let Some(d) = &app.dialog {
        draw_dialog(f, app, d);
    }
    app.hit.borrow_mut().egg = Rect::new(
        area.right().saturating_sub(splash::HOT_W),
        area.y,
        splash::HOT_W.min(area.width),
        1,
    );
    if let Some(egg) = &app.egg {
        splash::draw(f, egg);
    }
}

fn panel(title: Line<'static>, focused: bool, keys: &[(&str, &str)]) -> Block<'static> {
    let style = if focused {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(Color::DarkGray)
    };
    let block = Block::bordered()
        .border_type(if focused { BorderType::Thick } else { BorderType::Plain })
        .border_style(style)
        .title(title);
    if keys.is_empty() {
        block
    } else {
        block.title_bottom(hints(keys, focused))
    }
}

/// `⏎ open  ⌫ up …` for a panel's bottom border.
fn hints(keys: &[(&str, &str)], focused: bool) -> Line<'static> {
    let (k, d) = if focused {
        (Style::new().fg(ACCENT).add_modifier(Modifier::BOLD), Style::new().fg(Color::Gray))
    } else {
        (Style::new().fg(Color::DarkGray), Style::new().fg(Color::DarkGray))
    };
    let mut spans = vec![Span::raw(" ")];
    for (i, (key, desc)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", d));
        }
        spans.push(Span::styled(key.to_string(), k));
        spans.push(Span::styled(format!(" {desc}"), d));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn title(text: impl Into<String>, focused: bool) -> Line<'static> {
    let s = Span::raw(format!(" {} ", text.into()));
    Line::from(if focused { s.bold().fg(ACCENT) } else { s.fg(Color::Gray) })
}

/// Keep the tail of a path that fits `max` chars.
fn shorten(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max || max < 4 {
        return s.to_string();
    }
    let tail: String = s.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

// ------------------------------------------------------------------ local

fn draw_local(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Local;
    let cwd = app.local.cwd.display().to_string();
    let block = panel(
        title(
            format!("Local  {}", shorten(&cwd, area.width.saturating_sub(12) as usize)),
            focused,
        ),
        focused,
        &[("⏎", "open"), ("⌫", "up"), ("N", "new folder"), ("Supr", "delete")],
    );
    let inner_w = area.width.saturating_sub(4) as usize;
    let items: Vec<ListItem> = app
        .local
        .entries
        .iter()
        .map(|e| {
            let mark = if e.downloaded {
                Span::raw("◆ ").green()
            } else {
                Span::raw("  ")
            };
            if e.is_dir {
                let name = truncate(&format!("{}/", e.name), inner_w.saturating_sub(2));
                ListItem::new(Line::from(vec![mark, Span::raw(name).fg(Color::LightBlue)]))
            } else {
                let size = human_bytes(e.size);
                let room = inner_w.saturating_sub(size.len() + 3);
                let name = truncate(&e.name, room);
                let pad = inner_w.saturating_sub(2 + name.chars().count() + size.len());
                ListItem::new(Line::from(vec![
                    mark,
                    Span::raw(name),
                    Span::raw(" ".repeat(pad)),
                    Span::raw(size).fg(Color::DarkGray),
                ]))
            }
        })
        .collect();
    let rows = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(Some(app.local.selected));
    f.render_stateful_widget(list, area, &mut state);
    app.hit.borrow_mut().local = ListHit {
        area,
        rows,
        offset: state.offset(),
    };
}

fn highlight(focused: bool) -> Style {
    if focused {
        Style::new().bg(Color::Rgb(40, 60, 80)).add_modifier(Modifier::BOLD)
    } else {
        Style::new().bg(Color::Rgb(40, 40, 40))
    }
}

// ------------------------------------------------------------------ alba

fn conn_label(app: &App) -> Span<'static> {
    match &app.conn {
        Conn::Connected(u) => Span::raw(format!("● {u}")).green(),
        Conn::Connecting => Span::raw("◌ connecting").yellow(),
        Conn::Reconnecting(Some(s)) => Span::raw(format!("◌ reconnecting in {s}s")).yellow(),
        Conn::Reconnecting(None) => Span::raw("◌ reconnecting").yellow(),
        Conn::Offline => Span::raw("○ offline").fg(Color::DarkGray),
        Conn::Lost(_) => Span::raw("✗ disconnected").red(),
    }
}

fn status_glyph(s: Status) -> Span<'static> {
    match s {
        Status::Done => Span::raw("✓").green(),
        Status::Running => Span::raw("…").yellow(),
        Status::Failed => Span::raw("✗").red(),
    }
}

fn draw_alba(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Alba;
    app.hit.borrow_mut().alba = ListHit {
        area,
        ..ListHit::default()
    };
    let mut t = title("ALBA", focused);
    t.spans.push(conn_label(app));
    t.spans.push(Span::raw(" "));
    let root = app.remote_root.clone().unwrap_or_else(|| "/".into());
    let path = match &app.alba_date {
        // beamline-tree accounts: the id is the path under /
        Some(d) if d.contains('/') => format!("/{d}"),
        Some(d) => format!("{}/{d}", root.trim_end_matches('/')),
        None if root.contains(", ") => String::new(),
        None => root,
    };
    let room = area.width.saturating_sub(28) as usize;
    t.spans.push(Span::raw(format!("{} ", shorten(&path, room))).fg(Color::Gray));
    let keys: &[(&str, &str)] = if app.alba_date.is_some() {
        &[("⏎", "stats"), ("⌫", "back"), ("S", "scan all")]
    } else {
        &[("⏎", "open"), ("r", "refresh")]
    };
    let block = panel(t, focused, keys);
    let rows = app.alba_rows();
    if rows.is_empty() {
        let text = match (&app.conn, &app.alba_note) {
            (_, Some(note)) => Line::from(note.clone()).yellow(),
            (Conn::Connected(_), None) => Line::from("listing beamtimes…").fg(Color::DarkGray),
            (Conn::Connecting, None) => Line::from("connecting…").fg(Color::DarkGray),
            _ => Line::from("Not connected. Press C to log in.").fg(Color::DarkGray),
        };
        f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }).block(block), area);
        return;
    }
    let inner_w = area.width.saturating_sub(4) as usize;
    let listing = app
        .alba_date
        .as_ref()
        .is_some_and(|d| app.cache.experiments.get(d).is_some_and(|e| e.samples.is_none()));
    let mut items: Vec<ListItem> = rows
        .iter()
        .map(|r| match r {
            AlbaRow::Up => ListItem::new(Line::from("  ../").fg(Color::LightBlue)),
            AlbaRow::Date(d) => {
                let count = app
                    .cache
                    .experiments
                    .get(d)
                    .and_then(|e| e.samples.as_ref())
                    .map(|s| format!("  {} samples", s.len()))
                    .unwrap_or_default();
                let mut spans = vec![Span::raw("  ")];
                if let Some(bl) = layout::beamline_name(d) {
                    let colour = if bl == "XAIRA" { Color::LightMagenta } else { Color::LightCyan };
                    spans.push(Span::raw(format!("{bl:<6} ")).fg(colour).bold());
                }
                spans.push(Span::raw(format!("{}/", layout::beamtime_date(d))).fg(Color::LightBlue).bold());
                spans.push(Span::raw(count).fg(Color::DarkGray));
                ListItem::new(Line::from(spans))
            }
            AlbaRow::Sample { date, name } => sample_item(app, date, name, inner_w),
        })
        .collect();
    if listing {
        items.push(ListItem::new(Line::from("  listing…").fg(Color::DarkGray)));
    }
    let rows = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(highlight(focused));
    let mut state = ListState::default().with_selected(Some(app.alba_sel));
    f.render_stateful_widget(list, area, &mut state);
    app.hit.borrow_mut().alba = ListHit {
        area,
        rows,
        offset: state.offset(),
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalMark {
    /// A folder with the sample's name exists in the Local panel's folder.
    SameName,
    /// …and it holds files downloaded from this beamtime's sample.
    Downloaded,
}

fn local_mark(app: &App, date: &str, name: &str) -> Option<LocalMark> {
    let folder = crate::download::sanitize(name);
    let from_here = |origins: &[(String, String)]| {
        origins.iter().any(|(d, s)| {
            s == name
                // downloads made before beamtime ids became paths stored only the date
                && (d == date || (!d.contains('/') && d == layout::beamtime_date(date)))
        })
    };
    let inside = app
        .local
        .cwd
        .file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&folder));
    let origins = if inside {
        Some(&app.local.here_origins)
    } else {
        app.local
            .entries
            .iter()
            .find(|e| e.is_dir && e.name.eq_ignore_ascii_case(&folder))
            .map(|e| &e.origins)
    }?;
    Some(if from_here(origins) {
        LocalMark::Downloaded
    } else {
        LocalMark::SameName
    })
}

fn sample_item(app: &App, date: &str, name: &str, width: usize) -> ListItem<'static> {
    let mark = match local_mark(app, date, name) {
        Some(LocalMark::Downloaded) => Span::raw("◆").green(),
        Some(LocalMark::SameName) => Span::raw("◆").yellow(),
        None => Span::raw(" "),
    };
    let mut spans = vec![Span::raw("  "), mark, Span::raw(" ")];
    let test = app.is_test_sample(date, name);
    let mut right: Vec<Span> = Vec::new();
    let scanning = app.scanning.contains(&(date.to_string(), name.to_string()));
    match app.info(date, name) {
        Some(info) if info.has_data() => {
            for s in info.solves() {
                right.push(status_glyph(s.status));
            }
        }
        Some(info) if info.unprocessed() => right.push(Span::raw("not processed").fg(Color::Yellow)),
        Some(info) if info.test_shots_only() => right.push(Span::raw("test shots").fg(Color::DarkGray)),
        Some(_) => right.push(Span::raw("empty").fg(Color::DarkGray)),
        None => right.push(Span::raw("·").fg(Color::DarkGray)),
    }
    if scanning {
        right.insert(0, Span::raw("⟳ ").fg(ACCENT));
    }
    let right_w: usize = right.iter().map(|s| s.content.chars().count()).sum();
    let room = width.saturating_sub(5 + right_w);
    let label = truncate(name, room);
    let pad = room.saturating_sub(label.chars().count()) + 1;
    spans.push(if test {
        Span::raw(label).fg(Color::DarkGray)
    } else {
        Span::raw(label)
    });
    spans.push(Span::raw(" ".repeat(pad)));
    spans.extend(right);
    ListItem::new(Line::from(spans))
}

// ------------------------------------------------------------------ stats

fn f1(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "–".into())
}

fn f2(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "–".into())
}

fn cc(v: Option<f64>) -> String {
    v.map(|x| {
        let s = format!("{x:.3}");
        s.strip_prefix('0').map(str::to_string).unwrap_or(s)
    })
    .unwrap_or_else(|| "–".into())
}

fn draw_stats(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Stats;
    app.hit.borrow_mut().stats = ListHit {
        area,
        ..ListHit::default()
    };
    let selected = app.selected_row();
    let Some(AlbaRow::Sample { date, name }) = selected.clone() else {
        let block = panel(title("Stats", focused), focused, &[]);
        let text = match selected {
            Some(AlbaRow::Date(d)) => date_summary(app, &d),
            _ => vec![Line::from("Select a sample in the ALBA panel.").fg(Color::DarkGray)],
        };
        f.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), area);
        return;
    };
    let block = panel(
        title(format!("{name} · {}", layout::beamtime_label(&date)), focused),
        focused,
        &[("␣", "mark"), ("d", "download"), ("F", "frames")],
    );
    let Some(info) = app.info(&date, &name) else {
        let scanning = app.scanning.contains(&(date.clone(), name.clone()));
        let msg = if scanning {
            "scanning…"
        } else if app.connected() {
            "Not scanned yet: wait a moment, or press Enter / s."
        } else {
            "Not scanned yet (offline)."
        };
        f.render_widget(Paragraph::new(msg).fg(Color::DarkGray).block(block), area);
        return;
    };
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = App::stats_rows(info);
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new("Nothing found for this sample: no processing folders and no raw datasets.")
                .fg(Color::DarkGray),
            inner,
        );
        return;
    }
    let details_h = 11.min(inner.height.saturating_sub(4));
    let [table_a, details_a] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(details_h)]).areas(inner);

    let header = Row::new(
        ["", "Solve", "Run", "", "SG", "d_min", "Rmrg%", "CC½", "Cmpl%", "I/σ", "Mult"]
            .map(|h| Cell::from(h).bold()),
    )
    .fg(Color::Gray);
    let best = rank::best(info).map(|(k, _)| k);
    let table_rows: Vec<Row> = rows
        .iter()
        .map(|r| match *r {
            StatsRow::Solve { run, solve } => {
                let s = &info.runs[run].solves[solve];
                solve_row(app, s, info.runs[run].run, best.as_deref() == Some(&s.key()))
            }
            StatsRow::Frames { run } => {
                let r = &info.runs[run];
                let ticked = app.frames_marked(&date, &name, r);
                let what = match (r.characterisation, r.images()) {
                    (true, _) => "frames (char.)".to_string(),
                    (false, Some((a, b))) => format!("frames {}", b.saturating_sub(a) + 1),
                    (false, None) => "frames".to_string(),
                };
                Row::new(vec![
                    Cell::from(if ticked { "[x]" } else { "[ ]" }),
                    Cell::from(what).fg(Color::Magenta),
                    Cell::from(r.run.to_string()),
                ])
                .fg(Color::Gray)
            }
        })
        .collect();
    let widths = [
        Constraint::Length(3),
        Constraint::Length(17),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(9),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(4),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(4),
    ];
    let table = Table::new(table_rows, widths)
        .header(header)
        .column_spacing(1)
        .row_highlight_style(highlight(focused));
    let mut state = TableState::default().with_selected(Some(app.stats_sel.min(rows.len() - 1)));
    f.render_stateful_widget(table, table_a, &mut state);
    app.hit.borrow_mut().stats = ListHit {
        area,
        // rows start below the header line
        rows: Rect {
            y: table_a.y + 1,
            height: table_a.height.saturating_sub(1),
            ..table_a
        },
        offset: state.offset(),
    };

    let lines = match rows.get(app.stats_sel) {
        Some(StatsRow::Solve { run, solve }) => solve_details(app, info, *run, *solve),
        Some(StatsRow::Frames { run }) => {
            let r = &info.runs[*run];
            let imgs = r
                .images()
                .map(|(a, b)| format!("images {a}–{b}, ≈ {} at 6.2 MB/frame", human_bytes((u64::from(b - a) + 1) * 6_230_000)))
                .unwrap_or_else(|| "frame count known once listed".into());
            let what = if r.characterisation {
                Line::from("test shots only: no data collection").fg(Color::DarkGray)
            } else if r.solves.is_empty() {
                Line::from("not processed by ALBA's pipelines: download the frames to process them yourself")
                    .fg(Color::Yellow)
            } else {
                Line::from("")
            };
            vec![
                Line::from(format!("Raw frames of run {} (prefix {})", r.run, r.prefix)).bold(),
                what,
                Line::from(imgs),
                Line::from(""),
                Line::from("Space ticks/unticks the run. F lists the ticked runs and asks before downloading.")
                    .fg(Color::DarkGray),
            ]
        }
        None => vec![],
    };
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(ratatui::widgets::Borders::TOP).border_style(Style::new().fg(Color::DarkGray)))
            .wrap(Wrap { trim: false }),
        details_a,
    );
}

/// Overall Rmerge, or Rmeas marked with `*` when the pipeline reports no Rmerge.
fn r_cell(o: Option<&Shell>) -> String {
    match o {
        Some(o) if o.rmerge_pct.is_some() => f1(o.rmerge_pct),
        Some(o) if o.rmeas_pct.is_some() => format!("{}*", f1(o.rmeas_pct)),
        _ => "–".into(),
    }
}

fn solve_row(app: &App, s: &Solve, run: u32, best: bool) -> Row<'static> {
    let mark = if s.hkl.is_empty() {
        " - ".to_string()
    } else if app.marks.contains(&s.key()) {
        "[x]".to_string()
    } else {
        "[ ]".to_string()
    };
    let st = s.stats.as_ref();
    let o = st.and_then(|st| st.overall.as_ref());
    let sg = st
        .and_then(|st| st.space_group.clone())
        .unwrap_or_else(|| "–".into());
    let label = if best {
        format!("★ {}", s.kind.label())
    } else {
        format!("  {}", s.kind.label())
    };
    let mut row = Row::new(vec![
        Cell::from(mark),
        Cell::from(label),
        Cell::from(run.to_string()),
        Cell::from(status_glyph(s.status)),
        Cell::from(sg),
        Cell::from(f2(o.and_then(|o| o.d_high))),
        Cell::from(r_cell(o)),
        Cell::from(cc(o.and_then(|o| o.cc_half))),
        Cell::from(f1(o.and_then(|o| o.completeness))),
        Cell::from(f1(o.and_then(|o| o.i_sig))),
        Cell::from(f1(o.and_then(|o| o.multiplicity))),
    ]);
    if best {
        row = row.fg(Color::LightGreen).add_modifier(Modifier::BOLD);
    } else if s.status == Status::Failed {
        row = row.fg(Color::DarkGray);
    }
    row
}

fn shell_line(label: &str, s: Option<&Shell>) -> Line<'static> {
    let Some(s) = s else {
        return Line::from(format!("{label:<8} –")).fg(Color::DarkGray);
    };
    let range = format!("{}–{}", f2(s.d_low), f2(s.d_high));
    Line::from(format!(
        "{label:<8} {range:<13} {:>6} {:>5} {:>6} {:>7} {:>6} {:>6} {:>5}",
        f1(s.completeness),
        f1(s.multiplicity),
        f1(s.i_sig),
        f1(s.rmerge_pct),
        f1(s.rmeas_pct),
        f1(s.rpim_pct),
        cc(s.cc_half)
    ))
}

fn solve_details(app: &App, info: &SampleInfo, run: usize, solve: usize) -> Vec<Line<'static>> {
    let r = &info.runs[run];
    let s = &r.solves[solve];
    let status = match s.status {
        Status::Done => Span::raw("done").green(),
        Status::Running => Span::raw("running").yellow(),
        Status::Failed => Span::raw("failed").red(),
    };
    let mut head = vec![
        Span::raw(format!("{} · run {} · ", s.kind.label(), r.run)).bold(),
        status,
    ];
    let best = rank::best(info);
    match (rank::score(s), &best) {
        (Some(v), Some((k, _))) if *k == s.key() => {
            head.push(Span::raw(format!("  ★ best solve (score {v:.2})")).light_green().bold());
        }
        (Some(v), Some((_, b))) => head.push(Span::raw(format!("  score {v:.2} (best {b:.2})")).fg(Color::Gray)),
        _ => {}
    }
    let mut lines = vec![Line::from(head)];
    if let Some(st) = &s.stats {
        let cell = st
            .cell
            .map(|c| {
                format!(
                    "{:.3} {:.3} {:.3}  {:.2} {:.2} {:.2}",
                    c[0], c[1], c[2], c[3], c[4], c[5]
                )
            })
            .unwrap_or_else(|| "–".into());
        let imgs = st
            .images
            .map(|(a, b)| format!("   images {a}–{b}"))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            "SG {}   cell {cell}{imgs}",
            st.space_group.as_deref().unwrap_or("–")
        )));
        lines.push(
            Line::from(format!(
                "{:<8} {:<13} {:>6} {:>5} {:>6} {:>7} {:>6} {:>6} {:>5}",
                "shell", "d (Å)", "Cmpl%", "Mult", "I/σ", "Rmerge%", "Rmeas%", "Rpim%", "CC½"
            ))
            .fg(Color::Gray),
        );
        lines.push(shell_line("overall", st.overall.as_ref()));
        lines.push(shell_line("inner", st.inner.as_ref()));
        lines.push(shell_line("outer", st.outer.as_ref()));
    } else {
        lines.push(Line::from("no statistics yet").fg(Color::DarkGray));
    }
    if !s.hkl.is_empty() {
        let files: Vec<String> = s
            .hkl
            .iter()
            .map(|f| match f.size {
                Some(sz) => format!("{} ({})", f.name(), human_bytes(sz)),
                None => f.name().to_string(),
            })
            .collect();
        lines.push(Line::from(format!("HKL: {}", files.join(", "))));
    }
    if !s.logs.is_empty() {
        let state = if app.include_logs { "included" } else { "not included" };
        lines.push(
            Line::from(format!("logs: {} files, {state} (L toggles)", s.logs.len())).fg(Color::DarkGray),
        );
    }
    if let Some(n) = &s.note {
        lines.push(Line::from(n.clone()).fg(Color::Yellow));
    }
    let rmeas_only = s
        .stats
        .as_ref()
        .and_then(|st| st.overall.as_ref())
        .is_some_and(|o| o.rmerge_pct.is_none() && o.rmeas_pct.is_some());
    if rmeas_only {
        lines.push(Line::from("* no Rmerge reported: the R column shows Rmeas").fg(Color::DarkGray));
    }
    lines.push(
        Line::from("★ best by completeness, CC½, inner R and resolution")
            .fg(Color::DarkGray),
    );
    lines
}

fn date_summary(app: &App, date: &str) -> Vec<Line<'static>> {
    let exp = app.cache.experiments.get(date);
    let total = exp.and_then(|e| e.samples.as_ref()).map(|s| s.len());
    let scanned = exp.map(|e| e.infos.len()).unwrap_or(0);
    let mut lines = vec![Line::from(format!("Beamtime {}", layout::beamtime_label(date))).bold()];
    match total {
        Some(t) => {
            lines.push(Line::from(format!("{t} sample folders, {scanned} scanned")));
            lines.push(Line::from(""));
            lines.push(
                Line::from("Enter opens the beamtime. S fetches stats for every sample (one every 3 s).")
                    .fg(Color::DarkGray),
            );
        }
        None => lines.push(Line::from("not listed yet (Enter to list)").fg(Color::DarkGray)),
    }
    lines
}

// ------------------------------------------------------------------ console

fn line_style(k: LineKind) -> Style {
    match k {
        LineKind::Cmd => Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        LineKind::Out => Style::new(),
        LineKind::Info => Style::new().fg(Color::DarkGray),
        LineKind::Good => Style::new().fg(Color::Green),
        LineKind::Warn => Style::new().fg(Color::Yellow),
        LineKind::Error => Style::new().fg(Color::Red),
    }
}

fn vt_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// The running program's screen, row by row, with its colours.
fn screen_lines(screen: &vt100::Screen) -> Vec<Line<'static>> {
    let cols = screen.size().1.get();
    (0..=crate::pty::last_used_row(screen))
        .map(|r| {
            let mut spans: Vec<Span> = Vec::new();
            let mut text = String::new();
            let mut style = Style::new();
            for c in 0..cols {
                let Some(cell) = screen.cell(r, c) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut st = Style::new();
                if let Some(fg) = vt_color(cell.fgcolor()) {
                    st = st.fg(fg);
                }
                if let Some(bg) = vt_color(cell.bgcolor()) {
                    st = st.bg(bg);
                }
                if cell.bold() {
                    st = st.add_modifier(Modifier::BOLD);
                }
                if cell.underline() {
                    st = st.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    st = st.add_modifier(Modifier::REVERSED);
                }
                if st != style && !text.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut text), style));
                }
                style = st;
                text.push_str(if cell.has_contents() { cell.contents() } else { " " });
            }
            let tail = if style == Style::new() {
                text.trim_end().to_string()
            } else {
                text
            };
            if !tail.is_empty() {
                spans.push(Span::styled(tail, style));
            }
            Line::from(spans)
        })
        .collect()
}

fn draw_console(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Console;
    let c = &app.console;
    let cwd = app.local.cwd.display().to_string();
    let mut t = title(
        format!("Console  {}", shorten(&cwd, area.width.saturating_sub(40) as usize)),
        focused,
    );
    if let Some(r) = &c.pty {
        t.spans.push(Span::raw(format!(" {} running: type the answer, Enter sends it ", r.label)).black().on_yellow());
        t.spans.push(Span::raw(" "));
    }
    if c.scroll > 0 {
        t.spans.push(Span::raw(format!("[↑{}] ", c.scroll)).fg(Color::DarkGray));
    }
    if let Some(file) = app.local.selected_file() {
        t.spans.push(Span::raw("file: ").fg(Color::DarkGray));
        t.spans.push(Span::raw(format!("{file} ")).fg(Color::Gray));
    }
    let keys: &[(&str, &str)] = match (focused, c.pty.is_some()) {
        (true, true) => &[("⏎", "send answer"), ("^C", "interrupt"), ("Esc", "leave")],
        (true, false) => &[("⏎", "run"), ("↑↓", "history"), ("Esc", "leave")],
        (false, _) => &[(":", "type a command")],
    };
    let block = panel(t, focused, keys);
    let inner = block.inner(area);
    f.render_widget(block, area);
    {
        let mut hit = app.hit.borrow_mut();
        hit.console = area;
        hit.console_cols = inner.width;
    }
    if inner.height == 0 {
        return;
    }
    let [log_a, input_a] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let h = log_a.height as usize;

    // the log, followed by the running program's live screen
    let live: Vec<Line> = c
        .pty
        .as_ref()
        .and_then(|r| r.parser.lock().ok().map(|p| screen_lines(p.screen())))
        .unwrap_or_default();
    let need = h + c.scroll;
    let from_log = need.saturating_sub(live.len()).min(c.lines.len());
    let mut all: Vec<Line> = c
        .lines
        .iter()
        .skip(c.lines.len() - from_log)
        .map(|l| Line::styled(l.text.clone(), line_style(l.kind)))
        .collect();
    all.extend(live);
    let end = all.len().saturating_sub(c.scroll);
    let start = end.saturating_sub(h);
    f.render_widget(Paragraph::new(all[start..end].to_vec()), log_a);

    let prompt = if c.pty.is_some() {
        Span::raw("» ").yellow().bold()
    } else {
        Span::raw("> ").fg(if focused { ACCENT } else { Color::DarkGray })
    };
    f.render_widget(
        Paragraph::new(Line::from(vec![prompt, Span::raw(c.input.clone())])),
        input_a,
    );
    if focused && app.dialog.is_none() {
        let before: usize = c.input.chars().take(c.cursor).count();
        f.set_cursor_position((input_a.x + 2 + before as u16, input_a.y));
    }
}

// ------------------------------------------------------------------ status bar

/// Progress bar of the frames dataset being downloaded, above the status bar.
fn draw_transfer(f: &mut Frame, app: &App, area: Rect) {
    let d = &app.downloads;
    let Some(g) = d.groups.iter().find(|g| g.started.is_some()).or_else(|| d.groups.first()) else {
        return;
    };
    let done = d.group_progress(g);
    let frac = if g.bytes > 0 {
        done as f64 / g.bytes as f64
    } else if g.files > 0 {
        g.done_files as f64 / g.files as f64
    } else {
        0.0
    }
    .clamp(0.0, 1.0);
    let left = format!(" ↓ frames {} ", g.label);
    let mut right = format!(
        " {}/{} files · {} / {}",
        g.done_files,
        g.files,
        human_bytes(done),
        human_bytes(g.bytes)
    );
    match g.started {
        Some(t) => {
            let speed = done as f64 / t.elapsed().as_secs_f64().max(0.5);
            right.push_str(&format!(" · {}/s", human_bytes(speed as u64)));
            if speed > 1.0 {
                let eta = g.bytes.saturating_sub(done) as f64 / speed;
                right.push_str(&format!(" · {} left", crate::app::clock(eta)));
            }
        }
        None => right.push_str(" · waiting"),
    }
    if g.failed > 0 {
        right.push_str(&format!(" · {} failed", g.failed));
    }
    if d.groups.len() > 1 {
        right.push_str(&format!(" · +{} queued", d.groups.len() - 1));
    }
    right.push(' ');
    let bar_w = (area.width as usize).saturating_sub(left.chars().count() + right.chars().count());
    let filled = ((frac * bar_w as f64).round() as usize).min(bar_w);
    let line = Line::from(vec![
        Span::raw(left).fg(ACCENT).bold(),
        Span::raw("█".repeat(filled)).fg(ACCENT),
        Span::raw("░".repeat(bar_w - filled)).fg(Color::DarkGray),
        Span::raw(right).fg(Color::Gray),
    ]);
    f.render_widget(Paragraph::new(line).bg(BAR_BG), area);
}

fn badge(text: String, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::new().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    )
}

const OFF_FG: Color = Color::Gray;
const OFF_BG: Color = Color::Rgb(55, 55, 55);
const BAR_BG: Color = Color::Rgb(25, 25, 25);

/// Status on the right (connection, polling, sound, logs, downloads), global keys on
/// the left, the latest message in between.
fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let mut right: Vec<Span> = Vec::new();
    let d = &app.downloads;
    if d.queued > 0 {
        let waiting = d.queued.saturating_sub(d.active.len());
        let speed = human_bytes(d.speed() as u64);
        let s = match d.active.values().collect::<Vec<_>>().as_slice() {
            [] => format!("↓ {waiting} queued"),
            [a] => {
                let pct = a
                    .total
                    .filter(|t| *t > 0)
                    .map(|t| format!(" {}%", a.done * 100 / t))
                    .unwrap_or_default();
                let name = a.label.rsplit('/').next().unwrap_or(&a.label);
                format!("↓ {name}{pct} · {speed}/s · {waiting} queued")
            }
            many => format!("↓ {} files · {speed}/s · {waiting} queued", many.len()),
        };
        right.push(badge(s, Color::Black, ACCENT));
        right.push(Span::raw(" "));
    }
    let mut tags: Vec<(usize, Badge)> = Vec::new();
    tags.push((right.len(), Badge::Conn));
    right.push(match &app.conn {
        Conn::Connected(u) => badge(format!("● {u}"), Color::Black, Color::Green),
        Conn::Connecting => badge("◌ connecting".into(), Color::Black, Color::Yellow),
        Conn::Reconnecting(Some(s)) => badge(format!("◌ reconnecting {s}s"), Color::Black, Color::Yellow),
        Conn::Reconnecting(None) => badge("◌ reconnecting".into(), Color::Black, Color::Yellow),
        Conn::Offline => badge("○ offline".into(), OFF_FG, OFF_BG),
        Conn::Lost(_) => badge("✗ disconnected".into(), Color::White, Color::Red),
    });
    right.push(Span::raw(" "));
    tags.push((right.len(), Badge::Poll));
    let every = app
        .cfg
        .poll_interval(app.poll_mode)
        .map(crate::config::interval_label)
        .unwrap_or_default();
    right.push(match (app.poll_mode, app.next_poll) {
        (PollMode::Manual, _) => badge("poll manual".into(), OFF_FG, OFF_BG),
        (_, Some(at)) => {
            let left = at.saturating_duration_since(Instant::now()).as_secs();
            badge(format!("poll {every} · {}:{:02}", left / 60, left % 60), Color::Black, Color::LightBlue)
        }
        (_, None) => badge(format!("poll {every}"), Color::Black, Color::LightBlue),
    });
    right.push(Span::raw(" "));
    tags.push((right.len(), Badge::Sound));
    right.push(if app.sound {
        badge("♪ sound on".into(), Color::Black, Color::LightMagenta)
    } else {
        badge("♪ sound off".into(), OFF_FG, OFF_BG)
    });
    right.push(Span::raw(" "));
    tags.push((right.len(), Badge::Logs));
    right.push(if app.include_logs {
        badge("logs on".into(), Color::Black, Color::LightYellow)
    } else {
        badge("logs off".into(), OFF_FG, OFF_BG)
    });
    let req = app.remote.requests.load(std::sync::atomic::Ordering::Relaxed);
    right.push(Span::raw(format!(" req {req} ")).fg(Color::DarkGray));
    let right = Line::from(right);

    let key = |k: &'static str, d: &'static str| {
        [
            Span::raw(k).fg(Color::Gray).add_modifier(Modifier::BOLD),
            Span::raw(format!(" {d}   ")).fg(Color::DarkGray),
        ]
    };
    let full: Vec<Span> = [
        key("Tab", "panels"),
        key("p", "poll"),
        key("n", "sound"),
        key("L", "logs"),
        key("?", "help"),
        key("q", "quit"),
    ]
    .into_iter()
    .flatten()
    .collect();
    let short: Vec<Span> = [key("?", "help"), key("q", "quit")].into_iter().flatten().collect();
    let mut left = Line::from([vec![Span::raw(" ")], full].concat());
    if left.width() + right.width() + 4 > area.width as usize {
        left = Line::from([vec![Span::raw(" ")], short].concat());
    }

    f.render_widget(Block::default().bg(BAR_BG), area);
    let [left_a, msg_a, right_a] = Layout::horizontal([
        Constraint::Length(left.width() as u16),
        Constraint::Min(0),
        Constraint::Length(right.width() as u16),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(left), left_a);
    if let Some((msg, kind)) = app.status_text() {
        let room = msg_a.width.saturating_sub(2) as usize;
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(truncate(msg, room), line_style(kind)))),
            msg_a,
        );
    }
    // badge positions, for clicks
    let mut x = right_a.x + right_a.width.saturating_sub(right.width() as u16);
    let mut badges = Vec::new();
    for (i, span) in right.spans.iter().enumerate() {
        let w = span.width() as u16;
        if let Some((_, b)) = tags.iter().find(|(j, _)| *j == i) {
            badges.push((Rect::new(x, area.y, w, 1), *b));
        }
        x += w;
    }
    app.hit.borrow_mut().badges = badges;
    f.render_widget(Paragraph::new(right).alignment(ratatui::layout::Alignment::Right), right_a);
}

// ------------------------------------------------------------------ dialogs

fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn dialog_block(t: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(format!(" {t} ")).bold())
}

fn draw_dialog(f: &mut Frame, app: &App, d: &Dialog) {
    let area = f.area();
    match d {
        Dialog::Login(l) => {
            let r = centered(62, 13, area);
            f.render_widget(Clear, r);
            let field = |label: &str, value: String, active: bool| {
                let style = if active {
                    Style::new().fg(Color::White).bg(Color::Rgb(40, 60, 80))
                } else {
                    Style::new().fg(Color::Gray)
                };
                Line::from(vec![
                    Span::raw(format!("  {label:<10}")),
                    Span::styled(format!(" {value:<36}"), style),
                ])
            };
            let tick = format!("[{}] Remember username", if l.remember { "x" } else { " " });
            let tick_style = if l.field == 2 {
                Style::new().fg(Color::White).bg(Color::Rgb(40, 60, 80))
            } else {
                Style::new().fg(Color::Gray)
            };
            let mut remember = vec![Span::raw(format!("  {:<10} ", "")), Span::styled(tick.clone(), tick_style)];
            if l.field == 2 {
                remember.push(Span::raw("  Space toggles").fg(Color::DarkGray));
            }
            let mut lines = vec![
                Line::from(format!("  {}:{}", app.cfg.host, app.cfg.port)).fg(Color::DarkGray),
                Line::from(""),
                field("Username", l.user.clone(), l.field == 0),
                field("Password", "•".repeat(l.pass.chars().count()), l.field == 1),
                Line::from(remember),
            ];
            // the box itself is clickable
            app.hit.borrow_mut().login_remember = Rect::new(r.x + 1 + 13, r.y + 5, tick.chars().count() as u16, 1);
            if l.busy {
                lines.push(Line::from("  connecting…").yellow());
            } else if let Some(e) = &l.error {
                lines.push(Line::from(format!("  {e}")).red());
            } else {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("  Enter connect · Tab next field · Esc work offline").fg(Color::DarkGray));
            let note = if l.remember {
                "  Only the username is saved, never the password."
            } else {
                "  Nothing is saved to disk."
            };
            lines.push(Line::from(note).fg(Color::DarkGray));
            f.render_widget(
                Paragraph::new(lines).block(dialog_block("Connect to ALBA")),
                r,
            );
            if !l.busy {
                let (y, len) = match l.field {
                    0 => (r.y + 3, l.user.chars().count()),
                    1 => (r.y + 4, l.pass.chars().count()),
                    // inside the [ ]
                    _ => (r.y + 5, 1),
                };
                f.set_cursor_position((r.x + 1 + 12 + 1 + len as u16, y));
            }
        }
        Dialog::Input { title, value, .. } => {
            let w = (title.chars().count() as u16 + 6).clamp(48, 90);
            let r = centered(w, 6, area);
            f.render_widget(Clear, r);
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::raw("  Name "),
                    Span::styled(
                        format!(" {value:<width$}", width = (w as usize).saturating_sub(12)),
                        Style::new().fg(Color::White).bg(Color::Rgb(40, 60, 80)),
                    ),
                ]),
                Line::from(""),
                Line::from("  Enter create · Esc cancel").fg(Color::DarkGray),
            ];
            f.render_widget(Paragraph::new(lines).block(dialog_block(title)), r);
            f.set_cursor_position((r.x + 1 + 8 + value.chars().count() as u16, r.y + 2));
        }
        Dialog::Confirm { title, lines, .. } => {
            let h = lines.len() as u16 + 2;
            let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(20) as u16 + 6;
            let r = centered(w.max(40), h, area);
            f.render_widget(Clear, r);
            let text: Vec<Line> = lines.iter().map(|l| Line::from(format!(" {l}"))).collect();
            f.render_widget(Paragraph::new(text).block(dialog_block(title)), r);
        }
        Dialog::Message { title, text, .. } => {
            let lines: Vec<Line> = text.lines().map(|l| Line::from(format!(" {l}"))).collect();
            let w = text.lines().map(|l| l.chars().count()).max().unwrap_or(20) as u16 + 6;
            let r = centered(w.max(40), lines.len() as u16 + 2, area);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(lines).block(dialog_block(title)), r);
        }
        Dialog::Help => {
            let r = centered(80, 44, area);
            f.render_widget(Clear, r);
            f.render_widget(
                Paragraph::new(help_lines()).block(dialog_block("Keys (any key closes)")),
                r,
            );
        }
    }
}

fn help_lines() -> Vec<Line<'static>> {
    let sec = |s: &'static str| Line::from(Span::raw(s).bold().fg(ACCENT));
    let row = |k: &'static str, d: &'static str| {
        Line::from(vec![Span::raw(format!("  {k:<16}")).bold(), Span::raw(d)])
    };
    vec![
        sec("Everywhere"),
        row("Tab / Shift-Tab", "next / previous panel"),
        row(":", "type in the console"),
        row("C", "log in (when offline)"),
        row("p", "polling: manual → 10 min → 2 min → 30 s → 5 s"),
        row("n", "new-sample sound on/off"),
        row("L", "include logs in downloads on/off"),
        row("t", "show/hide samples named \"test…\" (test-shot-only samples are dimmed)"),
        row("X", "cancel queued downloads"),
        row("E", "open the Local panel's folder in the file explorer"),
        row("O", "open amanece's folder (config.toml, cache, amanece.log)"),
        row("q", "quit"),
        sec("Local (left)"),
        row("Enter / →", "open folder        Backspace / ←  parent"),
        row("N / F7", "new folder here    o  open with default program"),
        row("Supr / Del", "move the selected file or folder to the Recycle Bin (asks)"),
        row("b", "back to the launch folder    r  refresh"),
        sec("ALBA (beamtimes and samples, like folders)"),
        row("Enter / →", "open a beamtime (hides the others) / go to a sample's stats"),
        row("Backspace / ←", "back to the beamtime list"),
        row("s", "rescan sample      S  fetch stats for the whole beamtime"),
        row("r", "refresh listing"),
        sec("Stats"),
        row("Space", "mark solve for download / tick a frames run"),
        row("a", "mark all solves with HKL files     ★ marks the best solve"),
        row("◆ green", "ALBA sample already downloaded (from this beamtime) here"),
        row("◆ yellow", "only a local folder with the same name exists here"),
        row("d", "download marked solves into xia2chem_r3/, ednaproc_r3/ … in the Local folder"),
        row("F", "download ticked raw-frame runs (asks first; progress bar above the status bar)"),
        row("", "HKL/log downloads skip ahead of frames and start at once"),
        sec("Console (a terminal in the Local panel's folder)"),
        row("Enter", "run; a bare program name gets the selected file"),
        row("", "while a program runs (SADABS…), Enter sends your answer"),
        row("", "xprep → xprep XDS_ASCII.HKL, shelxt → shelxt <name>"),
        row("↑ / ↓", "history        PgUp / PgDn  scroll"),
        row("Ctrl-C", "interrupt (twice: kill)    Esc  leave console"),
        sec("Mouse"),
        row("click", "focus a panel, select a row; double-click opens"),
        row("", "click a Stats checkbox to mark; wheel scrolls; click status badges"),
        row("cd <dir>", "change folder   clear  wipe the log"),
        Line::from("  Windows GUI programs (xprep, shelxle, olex2…) start directly in their own window.")
            .fg(Color::DarkGray),
    ]
}

pub mod splash {
    //! Easter egg: the "Pasión en el bancal" scene of *Amanece, que no es poco* (released on
    //! 17 January 1989), narrated and typed out over a still drawn in coloured half blocks,
    //! like a videogame dialog.
    //! Pops up on 17 January, or when the hidden corner of the screen is clicked.

    use ratatui::Frame;
    use ratatui::layout::{Alignment, Rect};
    use ratatui::style::{Color, Modifier, Style, Stylize};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    /// The still: one terminal row per line (`splash.txt`) and the colour of each cell as
    /// raw RGB bytes (`splash.bin`), both made by asciify-engine from a frame. The popup is
    /// drawn from the colours; the characters only matter for art without them.
    /// Any size will do: it is resampled to the popup (see `Still`).
    const ART: &str = include_str!("splash.txt");
    const ART_RGB: &[u8] = include_bytes!("splash.bin");

    enum Beat {
        /// What happens on screen: no speaker.
        Narration(&'static str),
        Say(&'static str, &'static str),
    }

    use Beat::{Narration, Say};

    const SCRIPT: &[Beat] = &[
        Narration("Lejos de la feria, en el bancal, Elena y Mariano se besan y abrazan apasionadamente."),
        Narration("Mariano, el brotado, ha crecido ya mucho."),
        Narration("Elena se le echa encima e intenta quitarle apasionadamente la ropa..."),
        Say("Mariano", "Ten cuidado, mujer, que me estoy torciendo el pie."),
        Narration("Mariano está a punto de caerse."),
        Narration("Elena tira de su pantalón hasta desenterrarle el pie derecho, que todavía es un muñón terroso..."),
        Say("Mariano", "¡Aaargh!"),
        Say("Elena", "¡¿Qué te he hecho?!"),
        Narration("Mariano, muy enfadado, mira su pierna con dolor."),
        Say("Mariano", "Mírale tú qué gracia. Cojito para toda la vida."),
        Narration("Elena, avergonzada, se tapa la cara con las manos..."),
        Say("Mariano", "¡Me has arrancado de cuajo cuando aún estaba verde!"),
        Narration("En honor a José Luis Cuerda (1947 - 2020)"),
    ];

    const CHAR_TIME: Duration = Duration::from_millis(45);
    /// Height of the dialog box laid over the bottom of the still (borders included).
    const BOX_H: u16 = 4;
    /// Largest popup, in terminal cells (borders included).
    const MAX_W: u16 = 120;
    const MAX_H: u16 = 34;
    /// Width of the clickable top-right corner of the screen that opens the egg.
    pub const HOT_W: u16 = 3;

    pub struct Egg {
        line: usize,
        since: Instant,
    }

    impl Egg {
        pub fn new() -> Egg {
            Egg { line: 0, since: Instant::now() }
        }

        fn text(&self) -> &'static str {
            match SCRIPT[self.line] {
                Narration(t) | Say(_, t) => t,
            }
        }

        fn typing_time(&self) -> Duration {
            CHAR_TIME * self.text().chars().count() as u32
        }

        fn typed(&self) -> usize {
            (self.since.elapsed().as_millis() / CHAR_TIME.as_millis()) as usize
        }

        fn line_done(&self) -> bool {
            self.since.elapsed() >= self.typing_time()
        }

        fn is_last(&self) -> bool {
            self.line + 1 == SCRIPT.len()
        }

        /// Enter, a click: finish the line being typed, else show the next one. Nothing moves
        /// on without it. `false` once the scene is over.
        pub fn advance(&mut self) -> bool {
            if !self.line_done() {
                let now = Instant::now();
                self.since = now.checked_sub(self.typing_time()).unwrap_or(now);
            } else if self.is_last() {
                return false;
            } else {
                self.line += 1;
                self.since = Instant::now();
            }
            true
        }
    }

    pub fn draw(f: &mut Frame, egg: &Egg) {
        let area = f.area();
        let w = area.width.saturating_sub(2).min(MAX_W);
        let h = area.height.saturating_sub(2).min(MAX_H);
        if w < 24 || h < BOX_H + 4 {
            return;
        }
        let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
        f.render_widget(Clear, r);
        let block = Block::bordered()
            .border_type(BorderType::Double)
            .border_style(Style::new().fg(Color::Yellow))
            .style(Style::new().bg(Color::Black))
            .title(Line::from(" Amanece, que no es poco · Pasión en el bancal ").bold().fg(Color::Yellow))
            .title_bottom(Line::from(" Enter / clic: siguiente · Esc: cerrar ").fg(Color::DarkGray).right_aligned());
        let inner = block.inner(r);
        f.render_widget(block, r);

        f.render_widget(Paragraph::new(still().render(inner.width, inner.height)), inner);

        // The dialog box over the bottom of the still.
        let box_w = inner.width.saturating_sub(4).min(100);
        let b = Rect::new(
            inner.x + (inner.width - box_w) / 2,
            inner.bottom().saturating_sub(BOX_H + 1),
            box_w,
            BOX_H,
        );
        f.render_widget(Clear, b);
        let mut dialog = Block::bordered()
            .border_type(BorderType::Rounded)
            .style(Style::new().bg(Color::Black));
        let narration = matches!(SCRIPT[egg.line], Narration(_));
        dialog = match SCRIPT[egg.line] {
            Narration(_) => dialog.border_style(Style::new().fg(Color::DarkGray)),
            Say(who, _) => {
                let speaker = Style::new().fg(speaker_color(who)).add_modifier(Modifier::BOLD);
                dialog
                    .border_style(Style::new().fg(Color::White))
                    .title(Line::from(vec![Span::raw(" "), Span::styled(who, speaker), Span::raw(" ")]))
            }
        };
        let text_area = dialog.inner(b);
        f.render_widget(dialog, b);

        let mut left = egg.typed();
        let shown: Vec<Line> = wrap(egg.text(), text_area.width.saturating_sub(2) as usize)
            .into_iter()
            .map(|l| {
                let n = l.chars().count().min(left);
                left -= n;
                Line::from(format!(" {}", l.chars().take(n).collect::<String>()))
            })
            .collect();
        let style = if narration {
            Style::new().fg(Color::Gray).add_modifier(Modifier::ITALIC)
        } else {
            Style::new().fg(Color::White)
        };
        f.render_widget(Paragraph::new(shown).style(style), text_area);
        if egg.line_done() && egg.since.elapsed().as_millis() / 400 % 2 == 0 {
            let marker = if egg.is_last() { "■" } else { "▼" };
            f.render_widget(
                Paragraph::new(marker).alignment(Alignment::Right).style(Style::new().fg(Color::Yellow)),
                Rect::new(b.x + 1, b.bottom().saturating_sub(2), b.width.saturating_sub(3), 1),
            );
        }
    }

    fn speaker_color(who: &str) -> Color {
        match who {
            "Elena" => Color::LightMagenta,
            _ => Color::LightCyan,
        }
    }

    /// Greedy word wrap, done on the whole line so the words do not jump while it is typed.
    fn wrap(text: &str, width: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for word in text.split_whitespace() {
            match out.last_mut() {
                Some(l) if l.chars().count() + 1 + word.chars().count() <= width => {
                    l.push(' ');
                    l.push_str(word);
                }
                _ => out.push(word.to_string()),
            }
        }
        out
    }

    // ------------------------------------------------------------------ the still

    /// The ramp the art was made with (asciify's "detailed" set), light to dense.
    const ART_RAMP: &str = " .,:;-~=+*xX#%@";
    /// Fallback for art made with other characters: the usual ASCII-art order, light to dense.
    const DENSITY: &str = " .'`^\",:;Il!i><~+_-=?][}{1)(|/tfjrxnuvczXYUJCLQ0OZSmwqpdbkhao*#MW&8%B@$";
    /// Where the visible window of the still starts when it has to be cropped vertically:
    /// near the top, since the dialog box covers the bottom.
    const CROP_TOP: f32 = 0.15;

    struct Still {
        w: usize,
        h: usize,
        /// Brightness 0..1 per cell, contrast-stretched.
        cells: Vec<f32>,
        /// Colour per cell, 0..1; empty when the art has none.
        rgb: Vec<[f32; 3]>,
    }

    fn still() -> &'static Still {
        static STILL: OnceLock<Still> = OnceLock::new();
        STILL.get_or_init(|| Still::parse(ART, ART_RGB))
    }

    impl Still {
        fn parse(art: &str, rgb: &[u8]) -> Still {
            let rows: Vec<Vec<char>> = art.lines().map(|l| l.chars().collect()).collect();
            let w = rows.iter().map(Vec::len).max().unwrap_or(0).max(1);
            let h = rows.len().max(1);
            let place = |c: char, ramp: &str| ramp.find(c).map(|i| i as f32 / (ramp.len() - 1) as f32);
            let mut cells = vec![0.0; w * h];
            for (y, row) in rows.iter().enumerate() {
                for (x, c) in row.iter().enumerate() {
                    cells[y * w + x] = place(*c, ART_RAMP).or_else(|| place(*c, DENSITY)).unwrap_or(0.0);
                }
            }
            // Stretch so that art that only uses part of the ramp still has full contrast.
            let mut sorted = cells.clone();
            sorted.sort_by(f32::total_cmp);
            let lo = sorted[sorted.len() / 50];
            let hi = sorted[sorted.len() - 1 - sorted.len() / 50].max(lo + 0.01);
            for v in &mut cells {
                *v = ((*v - lo) / (hi - lo)).clamp(0.0, 1.0);
            }
            let rgb = if rgb.len() == w * h * 3 {
                rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2]].map(|v| f32::from(v) / 255.0)).collect()
            } else {
                Vec::new()
            };
            Still { w, h, cells, rgb }
        }

        /// The still scaled to fill `cols` x `rows` (cropping what does not fit), two pixels
        /// per cell: `▀` in the colour of the top half on the colour of the bottom one.
        fn render(&self, cols: u16, rows: u16) -> Vec<Line<'static>> {
            let (cols, rows) = (cols as usize, rows as usize);
            let (sw, sh) = (self.w as f32, self.h as f32);
            let scale = (cols as f32 / sw).max(rows as f32 / sh);
            let (win_w, win_h) = (cols as f32 / scale, rows as f32 / scale);
            let x0 = (sw - win_w) / 2.0;
            let y0 = (sh - win_h) * CROP_TOP;
            let (step_x, step_y) = (win_w / cols as f32, win_h / rows as f32);
            (0..rows)
                .map(|ty| {
                    let ya = y0 + ty as f32 * step_y;
                    let mid = ya + step_y / 2.0;
                    Line::from(
                        (0..cols)
                            .map(|tx| {
                                let xa = x0 + tx as f32 * step_x;
                                let top = self.pixel(xa, ya, xa + step_x, mid);
                                let bottom = self.pixel(xa, mid, xa + step_x, ya + step_y);
                                Span::styled("▀", Style::new().fg(top).bg(bottom))
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect()
        }

        fn pixel(&self, xa: f32, ya: f32, xb: f32, yb: f32) -> Color {
            let (v, rgb) = self.average(xa, ya, xb, yb);
            shade(v, rgb)
        }

        /// Mean brightness and colour of the source cells under the rectangle, each weighted
        /// by how much of it the rectangle covers (half a cell is often less than a source row).
        fn average(&self, xa: f32, ya: f32, xb: f32, yb: f32) -> (f32, Option<[f32; 3]>) {
            let x_from = (xa.floor().max(0.0) as usize).min(self.w - 1);
            let y_from = (ya.floor().max(0.0) as usize).min(self.h - 1);
            let x_to = (xb.ceil() as usize).clamp(x_from + 1, self.w);
            let y_to = (yb.ceil() as usize).clamp(y_from + 1, self.h);
            let cover = |a: f32, b: f32, i: usize| (b.min(i as f32 + 1.0) - a.max(i as f32)).max(0.0) + 1e-3;
            let (mut sum, mut weights, mut colour) = (0.0, 0.0, [0.0; 3]);
            for y in y_from..y_to {
                for x in x_from..x_to {
                    let i = y * self.w + x;
                    let wt = cover(xa, xb, x) * cover(ya, yb, y);
                    weights += wt;
                    sum += self.cells[i] * wt;
                    if let Some(c) = self.rgb.get(i) {
                        for k in 0..3 {
                            colour[k] += c[k] * wt;
                        }
                    }
                }
            }
            let rgb = (!self.rgb.is_empty()).then(|| colour.map(|c| c / weights));
            (sum / weights, rgb)
        }
    }

    /// The colour of one pixel. The stored colours already are the picture; averaging only
    /// washes them out a little, so they are pushed back from their grey.
    /// Art without colour gets a grey that is warm at the bright end.
    fn shade(v: f32, rgb: Option<[f32; 3]>) -> Color {
        let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0) as u8;
        match rgb {
            Some(c) => {
                let grey = 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
                let [r, g, b] = c.map(|x| byte(grey + (x - grey) * 1.25));
                Color::Rgb(r, g, b)
            }
            None => {
                let t = v.powf(0.85);
                Color::Rgb(byte(0.05 + t * 0.95), byte(0.05 + t * 0.85), byte(0.05 + t * 0.7))
            }
        }
    }

    // ------------------------------------------------------------------ the date

    /// 17 January, the day the film was released (local date).
    pub fn is_premiere_day() -> bool {
        month_day() == (1, 17)
    }

    #[cfg(windows)]
    fn month_day() -> (u32, u32) {
        use windows_sys::Win32::Foundation::SYSTEMTIME;
        use windows_sys::Win32::System::SystemInformation::GetLocalTime;
        let mut t = SYSTEMTIME::default();
        // SAFETY: GetLocalTime only writes the struct it is given.
        unsafe { GetLocalTime(&mut t) };
        (t.wMonth.into(), t.wDay.into())
    }

    #[cfg(not(windows))]
    fn month_day() -> (u32, u32) {
        month_day_utc(crate::model::now_secs())
    }

    /// (month, day) of a Unix time in UTC (Hinnant's civil-from-days).
    #[cfg_attr(windows, allow(dead_code))]
    fn month_day_utc(secs: u64) -> (u32, u32) {
        let z = (secs / 86_400) as i64 + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        (month as u32, day as u32)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        fn screen(egg: &Egg, w: u16, h: u16) -> String {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, egg)).unwrap();
            let buf = term.backend().buffer().clone();
            (0..h)
                .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        }

        #[test]
        fn premiere_date_from_unix_time() {
            // 1989-01-17 05:00 UTC, the day of the release
            assert_eq!(month_day_utc(600_998_400 + 5 * 3600), (1, 17));
            assert_eq!(month_day_utc(600_998_400 - 1), (1, 16));
            // 2024-02-29
            assert_eq!(month_day_utc(1_709_164_800), (2, 29));
        }

        #[test]
        fn wraps_without_splitting_words() {
            assert_eq!(wrap("uno dos tres cuatro", 8), ["uno dos", "tres", "cuatro"]);
        }

        #[test]
        fn every_beat_is_typed_then_waits_for_the_user() {
            let mut egg = Egg::new();
            for beat in SCRIPT {
                let (who, text) = match beat {
                    Narration(t) => (None, *t),
                    Say(w, t) => (Some(*w), *t),
                };
                assert!(!screen(&egg, 130, 36).contains(text), "{text} shows before it is typed");
                // enter finishes the line being typed ...
                assert!(egg.advance());
                let s = screen(&egg, 130, 36);
                assert!(s.contains(text));
                if let Some(w) = who {
                    assert!(s.contains(w));
                }
                // ... and time alone never moves on: only the next enter does
                egg.since = Instant::now().checked_sub(Duration::from_secs(600)).unwrap();
                assert!(screen(&egg, 130, 36).contains(text));
                if !egg.is_last() {
                    assert!(egg.advance());
                }
            }
            assert!(!egg.advance(), "after the last line the scene ends");
        }

        #[test]
        fn the_popup_is_wide_but_fits_the_terminal() {
            let egg = Egg::new();
            let popup_width = |s: String| {
                let line = s.lines().find(|l| l.contains('╔')).unwrap().to_string();
                let from = line.chars().position(|c| c == '╔').unwrap();
                let to = line.chars().position(|c| c == '╗').unwrap();
                to - from + 1
            };
            assert_eq!(popup_width(screen(&egg, 160, 45)), MAX_W as usize);
            assert_eq!(popup_width(screen(&egg, 100, 30)), 98);
        }

        #[test]
        fn the_still_fills_the_requested_size_from_any_art() {
            let art = Still::parse("@@@@\n@@@@\n....\n....", &[]);
            let out = art.render(10, 3);
            assert_eq!(out.len(), 3);
            assert!(out.iter().all(|l| l.spans.len() == 10));
            let mono = Still::parse("", &[]);
            assert_eq!(mono.render(5, 2).len(), 2);
        }

        #[test]
        fn each_cell_is_two_pixels_tall() {
            // bright row over dark row, shrunk to a single terminal row
            let out = Still::parse("@@@@\n....", &[]).render(2, 1);
            let cell = &out[0].spans[0];
            assert_eq!(cell.content, "▀");
            let red = |c: Option<Color>| match c {
                Some(Color::Rgb(r, _, _)) => r,
                other => panic!("not an RGB colour: {other:?}"),
            };
            assert!(red(cell.style.fg) > red(cell.style.bg));
        }

        #[test]
        fn small_terminal_does_not_panic() {
            let egg = Egg::new();
            for (w, h) in [(10, 5), (40, 12), (70, 20), (200, 60)] {
                screen(&egg, w, h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::model::{Experiment, RemoteFile, Run, SampleInfo, Solve, SolveKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn fixture_stats(name: &str, kind: SolveKind) -> crate::model::Stats {
        let xml = std::fs::read_to_string(format!("{}/docs/server_samples/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        crate::ispyb::parse(&xml, matches!(kind, SolveKind::Xia2 | SolveKind::Xia2Chem)).unwrap()
    }

    fn demo_app(base: &std::path::Path) -> App {
        let rt = Box::leak(Box::new(tokio::runtime::Runtime::new().unwrap()));
        let (tx, _rx) = std::sync::mpsc::channel();
        let remote = crate::remote::spawn(
            rt,
            crate::remote::Settings {
                host: "h".into(),
                port: 1,
                rate: 1.0,
                burst: 1,
                known_hosts: None,
                beamlines: vec!["bl13".into()],
                remote_root: None,
                parallel_downloads: 3,
            },
            tx.clone(),
        );
        let mut app = App::new(crate::config::Config::default(), None, base.to_path_buf(), remote, tx, None, true);
        let solve = |kind, xml: &str, hkl: &[(&str, u64)]| {
            let mut s = Solve::new(kind, format!("/r/{}", kind.folder()), 1);
            s.status = Status::Done;
            s.stats = Some(fixture_stats(xml, kind));
            s.hkl = hkl.iter().map(|(n, sz)| RemoteFile::new(format!("/r/{n}"), Some(*sz))).collect();
            s
        };
        let info = SampleInfo {
            name: "JSG285b".into(),
            scanned_at: 0,
            runs: vec![
                Run { prefix: "manually-mounted".into(), run: 2, characterisation: true, solves: vec![] },
                Run {
                    prefix: "manually-mounted".into(),
                    run: 3,
                    characterisation: false,
                    solves: vec![
                        solve(SolveKind::Xia2Chem, "xia2chem_ispyb.xml", &[("shelxt.hkl", 5_599_465), ("shelxt.ins", 514)]),
                        solve(SolveKind::EdnaProc, "ednaproc_ispyb.xml", &[("ep_XDS_ASCII.HKL", 10_554_107)]),
                        solve(SolveKind::AutoProcNoAnom, "autoproc_noanom_ispyb.xml", &[("XDS_ASCII.HKL", 20_879_412)]),
                    ],
                },
            ],
        };
        let mut exp = Experiment {
            date: "20251211".into(),
            samples: Some(vec!["JSG285".into(), "JSG285-2".into(), "JSG285b".into(), "KS347test".into()]),
            ..Experiment::default()
        };
        exp.infos.insert("JSG285b".into(), info);
        app.cache.set_dates(vec!["20251211".into(), "20251106".into()]);
        app.cache.experiments.insert("20251211".into(), exp);
        app.alba_date = Some("20251211".into());
        app.alba_sel = app
            .alba_rows()
            .iter()
            .position(|r| matches!(r, AlbaRow::Sample { name, .. } if name == "JSG285b"))
            .unwrap();
        app.focus = Focus::Stats;
        app.conn = Conn::Connected("u2024000000".into());
        app
    }

    fn render(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn click(app: &mut App, x: u16, y: u16) {
        use ratatui::crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        app.handle(crate::event::AppEvent::Input(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })));
    }

    /// Screen position of the first occurrence of `needle`.
    fn find(screen: &str, needle: &str) -> (u16, u16) {
        for (y, line) in screen.lines().enumerate() {
            if let Some(i) = line.find(needle) {
                return (line[..i].chars().count() as u16, y as u16);
            }
        }
        panic!("{needle} not on screen");
    }

    #[test]
    fn login_remember_box_renders_and_toggles_by_click() {
        let base = std::env::temp_dir().join(format!("amanece_ui_login_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        app.dialog = Some(crate::app::Dialog::Login(crate::app::Login {
            user: "u2025".into(),
            pass: String::new(),
            field: 1,
            remember: false,
            busy: false,
            error: None,
        }));
        let screen = render(&app, 120, 40);
        println!("{screen}");
        assert!(screen.contains("[ ] Remember username") && screen.contains("Nothing is saved to disk."));
        let (x, y) = find(&screen, "[ ] Remember");
        click(&mut app, x + 1, y);
        let screen = render(&app, 120, 40);
        println!("{screen}");
        assert!(screen.contains("[x] Remember username  Space toggles"));
        assert!(screen.contains("Only the username is saved, never the password."));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn hidden_corner_button_opens_the_easter_egg() {
        let base = std::env::temp_dir().join(format!("amanece_egg_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        let (w, h) = (160, 45);
        assert!(!render(&app, w, h).contains("Pasión en el bancal"));
        click(&mut app, w - 2, 0);
        assert!(render(&app, w, h).contains("Pasión en el bancal"));
        // a click finishes the typing, another moves on, and Esc closes it
        click(&mut app, 5, 5);
        click(&mut app, 5, 5);
        click(&mut app, 5, 5);
        assert!(render(&app, w, h).contains("Mariano, el brotado"));
        app.handle(crate::event::AppEvent::Input(ratatui::crossterm::event::Event::Key(
            ratatui::crossterm::event::KeyEvent::from(ratatui::crossterm::event::KeyCode::Esc),
        )));
        assert!(!render(&app, w, h).contains("Pasión en el bancal"));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn mouse_selects_marks_and_toggles() {
        let base = std::env::temp_dir().join(format!("amanece_mouse_{}", std::process::id()));
        std::fs::create_dir_all(base.join("JSG285b")).unwrap();
        let mut app = demo_app(&base);
        let screen = render(&app, 170, 42);

        // click a sample row in the ALBA panel
        let (x, y) = find(&screen, "JSG285-2");
        click(&mut app, x, y);
        assert_eq!(app.focus, Focus::Alba);
        assert_eq!(app.selected_sample().map(|s| s.1).as_deref(), Some("JSG285-2"));

        // back to JSG285b, then click its xia2chem checkbox in the Stats panel
        let (x, y) = find(&screen, "JSG285b    ");
        click(&mut app, x, y);
        let screen = render(&app, 170, 42);
        let (_, y) = find(&screen, "★ xia2chem");
        let (cx, _) = find(&screen, "[ ] ★");
        click(&mut app, cx + 1, y);
        assert_eq!(app.focus, Focus::Stats);
        assert!(app.marks.iter().any(|k| k.ends_with("#xia2chem")));

        // double-click a folder in the Local panel opens it
        let (x, y) = find(&screen, "JSG285b/");
        click(&mut app, x, y);
        click(&mut app, x, y);
        assert!(app.local.cwd.ends_with("JSG285b"));

        // status-bar badge
        let screen = render(&app, 170, 42);
        let (x, y) = find(&screen, "sound on");
        click(&mut app, x, y);
        assert!(!app.sound);
        std::fs::remove_dir_all(&base).ok();
    }

    #[cfg(windows)]
    #[test]
    fn console_shows_a_running_programs_prompt() {
        let base = std::env::temp_dir().join(format!("amanece_term_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        let _ = render(&app, 170, 42);
        app.sync_layout();
        let (tx, _rx) = std::sync::mpsc::channel();
        // `^` keeps the typed command from matching the prompt the program prints
        app.console.input = "cmd /C set /p X=Exp^ert mode (Y or N) [N]:".into();
        app.console.submit(&base, None, |_| false, &tx);
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let mut screen = String::new();
        while std::time::Instant::now() < end {
            screen = render(&app, 170, 42);
            if screen.contains("Expert mode (Y or N) [N]:") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        println!("{}", screen.lines().skip(screen.lines().count().saturating_sub(15)).collect::<Vec<_>>().join("
"));
        assert!(screen.contains("Expert mode (Y or N) [N]:"), "{screen}");
        assert!(screen.contains("running: type the answer"));
        app.console.kill();
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn scans_never_remove_rows_and_selection_follows_the_sample() {
        use crate::event::AppEvent;
        use crate::remote::RemoteEvent;
        let base = std::env::temp_dir().join(format!("amanece_rows_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        let selected = |app: &App| app.selected_sample().map(|s| s.1);
        assert_eq!(selected(&app).as_deref(), Some("JSG285b"));
        let rows = app.alba_rows().len();

        // JSG285 turns out to hold only test shots: it stays in the list, dimmed
        let shots = SampleInfo {
            name: "JSG285".into(),
            scanned_at: 0,
            runs: vec![Run { prefix: "p".into(), run: 1, characterisation: true, solves: vec![] }],
        };
        app.handle(AppEvent::Remote(RemoteEvent::SampleScanned { date: "20251211".into(), info: shots }));
        assert_eq!(app.alba_rows().len(), rows);
        assert_eq!(selected(&app).as_deref(), Some("JSG285b"));
        // a dataset nobody processed
        let raw = SampleInfo {
            name: "JSG285-2".into(),
            scanned_at: 0,
            runs: vec![Run { prefix: "d".into(), run: 1, characterisation: false, solves: vec![] }],
        };
        app.handle(AppEvent::Remote(RemoteEvent::SampleScanned { date: "20251211".into(), info: raw }));
        let screen = render(&app, 170, 42);
        assert!(screen.contains("test shots") && screen.contains("not processed"));

        // a new sample listed above the highlighted one: the highlight stays on JSG285b
        let names = ["AAA1", "JSG285", "JSG285-2", "JSG285b", "KS347test"].map(String::from).to_vec();
        app.handle(AppEvent::Remote(RemoteEvent::Samples {
            date: "20251211".into(),
            names,
            flat: false,
            from_poll: true,
        }));
        assert_eq!(app.alba_rows().len(), rows + 1);
        assert_eq!(selected(&app).as_deref(), Some("JSG285b"));
        std::fs::remove_dir_all(&base).ok();
    }

    fn style_at(app: &App, w: u16, h: u16, needle: &str, glyph: &str) -> Option<Color> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = app.hit.borrow().alba.rows;
        let xs = rows.x..(rows.x + rows.width).min(w);
        for y in rows.y..rows.y + rows.height {
            let line: String = xs.clone().map(|x| buf[(x, y)].symbol().to_string()).collect();
            if line.contains(needle) {
                return xs.clone().find(|&x| buf[(x, y)].symbol() == glyph).map(|x| buf[(x, y)].fg);
            }
        }
        None
    }

    #[test]
    fn diamonds_tell_downloaded_from_same_name() {
        let base = std::env::temp_dir().join(format!("amanece_dia_{}", std::process::id()));
        // JSG285b: downloaded from this beamtime; JSG285-2: just a folder with that name
        let solve = base.join("JSG285b").join("xia2chem_r3");
        let mut s = crate::download::Sources::default();
        s.files.insert(
            "shelxt.hkl".into(),
            crate::download::Source {
                date: "20251211".into(),
                sample: "JSG285b".into(),
                remote: "/r/shelxt.hkl".into(),
                what: "xia2chem".into(),
                run: 3,
                downloaded_at: 0,
            },
        );
        crate::download::write_sources(&solve, &s).unwrap();
        std::fs::create_dir_all(base.join("JSG285-2")).unwrap();
        let mut app = demo_app(&base);
        app.local.refresh();
        let _ = render(&app, 170, 42);
        assert_eq!(style_at(&app, 170, 42, "JSG285b ", "◆"), Some(Color::Green));
        assert_eq!(style_at(&app, 170, 42, "JSG285-2", "◆"), Some(Color::Yellow));
        assert_eq!(style_at(&app, 170, 42, "JSG285 ", "◆"), None);
        // inside the sample folder itself: still green
        app.local.cd(base.join("JSG285b"));
        let _ = render(&app, 170, 42);
        assert_eq!(style_at(&app, 170, 42, "JSG285b ", "◆"), Some(Color::Green));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn frames_get_a_progress_bar_and_two_console_lines() {
        use crate::event::AppEvent;
        use crate::remote::{DownloadEvent, RemoteEvent};
        let base = std::env::temp_dir().join(format!("amanece_bar_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        let files: Vec<RemoteFile> = (1..=3)
            .map(|i| RemoteFile::new(format!("/r/manually-mounted_3_000{i}.cbf"), Some(100)))
            .collect();
        let lines_before = app.console.lines.len();
        app.enqueue_frames("20251211", "JSG285b", files);
        assert_eq!(app.downloads.groups.len(), 1);
        let mut ids: Vec<u64> = app.downloads.job_group.keys().copied().collect();
        ids.sort();
        let ev = |d| AppEvent::Remote(RemoteEvent::Download(d));
        let label = |i: u64| format!("frames_r3/manually-mounted_3_000{i}.cbf");
        app.handle(ev(DownloadEvent::Started { id: ids[0], label: label(1), total: Some(100) }));
        app.handle(ev(DownloadEvent::Progress { id: ids[0], done: 50, total: Some(100) }));
        let screen = render(&app, 170, 42);
        assert!(screen.contains("frames JSG285b/frames_r3"), "{screen}");
        assert!(screen.contains("0/3 files") && screen.contains("█"));
        app.handle(ev(DownloadEvent::Finished { id: ids[0], label: label(1), bytes: 100, skipped: false, verified: false }));
        app.handle(ev(DownloadEvent::Started { id: ids[1], label: label(2), total: Some(100) }));
        app.handle(ev(DownloadEvent::Finished { id: ids[1], label: label(2), bytes: 100, skipped: false, verified: false }));
        app.handle(ev(DownloadEvent::Started { id: ids[2], label: label(3), total: Some(100) }));
        app.handle(ev(DownloadEvent::Failed { id: ids[2], label: label(3), message: "boom".into() }));
        assert!(app.downloads.groups.is_empty());
        let new: Vec<&str> = app.console.lines.iter().skip(lines_before).map(|l| l.text.as_str()).collect();
        assert_eq!(new.len(), 2, "{new:?}");
        assert!(new[0].contains("started, 3 files"));
        assert!(new[1].contains("2/3 downloaded, 1 failed") && new[1].contains("boom"));
        // the bar is gone once the dataset is done
        let screen = render(&app, 170, 42);
        assert!(!screen.contains("█") && !screen.contains("files ·"), "{screen}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn frames_say_when_every_checksum_matched() {
        use crate::event::AppEvent;
        use crate::remote::{DownloadEvent, RemoteEvent};
        let base = std::env::temp_dir().join(format!("amanece_md5_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        let ev = |d| AppEvent::Remote(RemoteEvent::Download(d));
        let label = |i: u64| format!("frames_r3/manually-mounted_3_000{i}.cbf");
        let run = |app: &mut App, verified: [bool; 2]| {
            let files: Vec<RemoteFile> = (1..=2)
                .map(|i| RemoteFile::new(format!("/r/manually-mounted_3_000{i}.cbf"), Some(100)))
                .collect();
            app.enqueue_frames("20251211", "JSG285b", files);
            let mut ids: Vec<u64> = app.downloads.job_group.keys().copied().collect();
            ids.sort();
            for (k, &id) in ids.iter().enumerate() {
                let (label, verified) = (label(k as u64 + 1), verified[k]);
                // the second is already here: skipped, but still checked
                let skipped = k == 1;
                app.handle(ev(DownloadEvent::Finished { id, label, bytes: 100, skipped, verified }));
            }
            app.console.lines.back().map(|l| l.text.clone()).unwrap_or_default()
        };
        let all = run(&mut app, [true, true]);
        assert!(all.contains("done, 2 files") && all.ends_with(", all MD5 verified"), "{all}");
        let some = run(&mut app, [true, false]);
        assert!(some.ends_with(", 1 MD5 verified"), "{some}");
        let none = run(&mut app, [false, false]);
        assert!(!none.contains("MD5"), "{none}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn local_shows_downloads_while_frames_keep_the_queue_busy() {
        use crate::event::AppEvent;
        use crate::remote::{DownloadEvent, RemoteEvent};
        let base = std::env::temp_dir().join(format!("amanece_lref_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut app = demo_app(&base);
        let names = |app: &App| app.local.entries.iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        let files = vec![RemoteFile::new("/r/manually-mounted_3_0001.cbf".to_string(), Some(100))];
        app.enqueue_frames("20251211", "JSG285b", files);
        // the frames folder shows as soon as it's queued
        assert!(names(&app).contains(&"JSG285b".to_string()), "{:?}", names(&app));
        // an HKL of another sample lands while the frames are still going
        std::fs::create_dir_all(base.join("KS347/autoproc_anom_r1")).unwrap();
        std::fs::write(base.join("KS347/autoproc_anom_r1/XDS_ASCII.HKL"), "x").unwrap();
        app.downloads.queued += 1;
        let ev = |d| AppEvent::Remote(RemoteEvent::Download(d));
        let label = "autoproc_anom_r1/XDS_ASCII.HKL".to_string();
        app.handle(ev(DownloadEvent::Started { id: 9999, label: label.clone(), total: Some(1) }));
        app.handle(ev(DownloadEvent::Finished { id: 9999, label, bytes: 1, skipped: false, verified: false }));
        assert!(app.downloads.queued > 0 && !app.downloads.groups.is_empty());
        assert!(names(&app).contains(&"KS347".to_string()), "{:?}", names(&app));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn render_snapshots() {
        let base = std::env::temp_dir().join(format!("amanece_ui_{}", std::process::id()));
        std::fs::create_dir_all(base.join("JSG285b/xia2chem_r3")).unwrap();
        let app = demo_app(&base);
        let wide = render(&app, 170, 42);
        let narrow = render(&app, 110, 44);
        println!("{wide}\n\n{narrow}");
        assert!(wide.contains("xia2chem") && wide.contains("P 4") && wide.contains("8.5"));
        assert!(narrow.contains("JSG285b"));
        // the best solve is starred
        assert!(wide.contains("★ xia2chem"));
        // test samples are hidden by default
        assert!(!wide.contains("KS347test"));
        // an opened beamtime hides the other beamtimes
        assert!(!wide.contains("20251106"));
        // key hints on the borders
        assert!(wide.contains("new folder") && wide.contains("d download"));
        // status bar: keys on the left, state on the right
        let bar = wide.lines().last().unwrap();
        assert!(bar.trim_start().starts_with("Tab panels"));
        assert!(bar.contains("poll manual") && bar.contains("sound on") && bar.contains("logs off"));
        std::fs::remove_dir_all(&base).ok();
    }
}
