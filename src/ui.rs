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
    let [main, console, status] = Layout::vertical([
        Constraint::Min(8),
        Constraint::Length(console_h),
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
    draw_status(f, app, status);
    if let Some(d) = &app.dialog {
        draw_dialog(f, app, d);
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
        let mut s = format!("↓ {} queued", d.queued);
        if let Some(a) = &d.active {
            let secs = a.started.elapsed().as_secs_f64().max(0.1);
            let speed = human_bytes((a.done as f64 / secs) as u64);
            let pct = a
                .total
                .filter(|t| *t > 0)
                .map(|t| format!(" {}%", a.done * 100 / t))
                .unwrap_or_default();
            let name = a.label.rsplit('/').next().unwrap_or(&a.label);
            s.push_str(&format!(" · {name}{pct} {speed}/s"));
        }
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
            let mut lines = vec![
                Line::from(format!("  {}:{}", app.cfg.host, app.cfg.port)).fg(Color::DarkGray),
                Line::from(""),
                field("Username", l.user.clone(), l.field == 0),
                field("Password", "•".repeat(l.pass.chars().count()), l.field == 1),
                Line::from(""),
            ];
            if l.busy {
                lines.push(Line::from("  connecting…").yellow());
            } else if let Some(e) = &l.error {
                lines.push(Line::from(format!("  {e}")).red());
            } else {
                lines.push(Line::from(""));
            }
            lines.push(Line::from(""));
            lines.push(Line::from("  Enter connect · Tab switch field · Esc work offline").fg(Color::DarkGray));
            lines.push(Line::from("  Nothing is saved to disk.").fg(Color::DarkGray));
            f.render_widget(
                Paragraph::new(lines).block(dialog_block("Connect to ALBA")),
                r,
            );
            if !l.busy {
                let (y, len) = if l.field == 0 {
                    (r.y + 3, l.user.chars().count())
                } else {
                    (r.y + 4, l.pass.chars().count())
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
            let r = centered(80, 43, area);
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
        row("F", "download ticked raw-frame runs (asks first)"),
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
    fn mouse_selects_marks_and_toggles() {
        let base = std::env::temp_dir().join(format!("alba_watch_mouse_{}", std::process::id()));
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
        let base = std::env::temp_dir().join(format!("alba_watch_term_{}", std::process::id()));
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
        let base = std::env::temp_dir().join(format!("alba_watch_rows_{}", std::process::id()));
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
        let base = std::env::temp_dir().join(format!("alba_watch_dia_{}", std::process::id()));
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
    fn render_snapshots() {
        let base = std::env::temp_dir().join(format!("alba_watch_ui_{}", std::process::id()));
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
