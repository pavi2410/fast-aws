//! Drawing. Everything here reads [`App`] and draws; no state changes.

use fast_aws_core::profiles::Auth;
use fast_aws_core::{AwsError, ColumnWidth, ErrorKind, Scope, Tone};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Paragraph, Row, Table, TableState, Wrap};

use crate::app::{App, IdentityState, Overlay, PickerEntry, REGIONS, Screen};
use crate::detail::{self, DetailState, DetailView};
use crate::list::LoadStatus;

const ACCENT: Color = Color::Cyan;
const DIM: Style = Style::new().fg(Color::DarkGray);
const SPARK: Style = Style::new().fg(Color::Cyan);
const KEY: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);

fn tone(t: Tone) -> Style {
    match t {
        Tone::Normal => Style::new(),
        Tone::Good => Style::new().fg(Color::Green),
        Tone::Warn => Style::new().fg(Color::Yellow),
        Tone::Bad => Style::new().fg(Color::Red),
        Tone::Dim => DIM,
    }
}

pub fn draw(frame: &mut Frame, app: &App) {
    let [header, title, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, header, app);
    match &app.screen {
        Screen::List => {
            draw_list_title(frame, title, app);
            match app.blocking_error().filter(|_| app.list.total() == 0) {
                // The error pane has its own frame.
                Some(err) => draw_error(frame, body, app, err),
                None => {
                    let inner = framed(frame, body, app);
                    draw_table(frame, inner, app)
                }
            }
        }
        Screen::Detail(d) => {
            draw_detail_title(frame, title, d);
            let inner = framed(frame, body, app);
            draw_detail(frame, inner, app, d);
        }
    }
    draw_footer(frame, footer, app);

    match &app.overlay {
        Overlay::None | Overlay::Filter | Overlay::Prompt(..) => {}
        Overlay::Palette(input, at) => draw_palette(frame, body, app, input, *at),
        Overlay::Profiles(at) => draw_profiles(frame, body, app, *at),
        Overlay::Regions(at) => draw_regions(frame, body, app, *at),
        Overlay::Relations(at) => draw_relations(frame, body, app, *at),
        Overlay::Help => draw_help(frame, body),
        Overlay::Settings(at) => draw_settings(frame, body, app, *at),
    }
}

fn draw_header(frame: &mut Frame, area: Rect, app: &App) {
    let sep = || Span::styled(" │ ", DIM);
    let account = match &app.identity {
        IdentityState::Loading => Span::styled("checking credentials…", DIM),
        IdentityState::Ready(id) => Span::raw(id.account.clone()),
        IdentityState::Failed(e) => {
            let what = match e.kind {
                ErrorKind::SessionExpired => "⚠ session expired",
                ErrorKind::Network => "⚠ AWS unreachable",
                ErrorKind::Throttled => "⚠ throttled",
                _ => "⚠ credentials",
            };
            Span::styled(what, Style::new().fg(Color::Red))
        }
    };
    let session = match app.current_profile().map(|p| &p.auth) {
        Some(Auth::SsoSession(s)) => Some(Span::styled(format!(" (sso {s})"), DIM)),
        _ => None,
    };
    let mut spans = vec![
        Span::styled(
            " fast-aws ",
            Style::new().fg(Color::Black).bg(ACCENT).bold(),
        ),
        Span::raw(" "),
        Span::styled(app.profile.clone(), Style::new().bold()),
    ];
    spans.extend(session);
    spans.extend([sep(), account, sep(), Span::raw(app.region.clone())]);
    if app.guard.prod {
        let text = if app.guard.unlocked {
            " PROD · unlocked "
        } else {
            " PROD "
        };
        spans.extend([
            Span::raw(" "),
            Span::styled(text, Style::new().fg(Color::White).bg(Color::Red).bold()),
        ]);
    }
    if app.guard.read_only {
        spans.extend([
            Span::raw(" "),
            Span::styled(
                " read-only ",
                Style::new().fg(Color::Black).bg(Color::Green),
            ),
        ]);
    }
    let right = Line::from(vec![
        Span::styled(
            format!(
                "  {} ",
                fast_aws_core::fmt::time::zone_label(app.display.utc)
            ),
            DIM,
        ),
        Span::styled(" ? ", KEY),
        Span::styled("help ", DIM),
    ]);
    draw_split(frame, area, Line::from(spans), right);
}

/// Draws `left` and a right-aligned `right` on one row without overlapping; the
/// left side is cut short if the row is too narrow.
fn draw_split(frame: &mut Frame, area: Rect, left: Line, right: Line) {
    let [l, r] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right.width() as u16),
    ])
    .areas(area);
    frame.render_widget(left, l);
    frame.render_widget(right, r);
}

/// With `borders = true`, draws a frame around `area` and returns its inside.
fn framed(frame: &mut Frame, area: Rect, app: &App) -> Rect {
    if !app.display.borders {
        return area;
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(DIM);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

fn draw_list_title(frame: &mut Frame, area: Rect, app: &App) {
    let list = &app.list;
    let count = if list.filter.is_empty() {
        format!("({})", list.total())
    } else {
        format!("({}/{})", list.visible_rows().len(), list.total())
    };
    let mut spans = vec![
        Span::styled(
            format!(" {} ", app.breadcrumb().join(" › ")),
            Style::new().fg(ACCENT).bold(),
        ),
        Span::raw(count),
    ];
    if !list.filter.is_empty() || app.overlay == Overlay::Filter {
        spans.push(Span::styled(
            format!("  /{}", list.filter),
            Style::new().fg(Color::Yellow),
        ));
    }
    let cached = list.cached_at.map(|at| {
        let age = fast_aws_core::fmt::age(at.elapsed().as_secs() as i64, 0);
        format!("cached {age} ago")
    });
    let status = match (&list.status, cached) {
        (LoadStatus::Loading, _) if list.kind.follow_every().is_some() => Span::styled(
            format!("● live · {} events ", list.total()),
            Style::new().fg(Color::Green),
        ),
        (LoadStatus::Loading, Some(cached)) => {
            Span::styled(format!("{cached} · refreshing… "), DIM)
        }
        (LoadStatus::Done, Some(cached)) => Span::styled(format!("{cached} · r refreshes "), DIM),
        (status, _) => status_span(list, status),
    };
    draw_split(frame, area, Line::from(spans), Line::from(status));
}

fn status_span(list: &crate::list::ListView, status: &LoadStatus) -> Span<'static> {
    match status {
        LoadStatus::Loading if list.total() == 0 => Span::styled("loading… ", DIM),
        LoadStatus::Loading => Span::styled(format!("loading… {} so far ", list.total()), DIM),
        LoadStatus::Done => Span::raw(""),
        LoadStatus::More(_) => Span::styled(
            "more available · n loads more ",
            Style::new().fg(Color::Yellow),
        ),
        LoadStatus::Failed(e) if list.total() > 0 => Span::styled(
            format!("⚠ refresh failed: {} ", short(e)),
            Style::new().fg(Color::Red),
        ),
        LoadStatus::Failed(_) => Span::raw(""),
    }
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App) {
    let list = &app.list;
    let (shown, short) = shown_columns(list.kind.columns(), app.metrics_enabled(), area.width);
    let columns: Vec<_> = shown.iter().map(|(_, c)| *c).collect();
    let widths = column_widths(&columns, area.width);
    let header = Row::new(columns.iter().map(|c| Cell::from(c.title))).style(DIM.bold());
    let rows = list.visible_rows().map(|r| {
        // A fetched sparkline replaces its cell; one being fetched shows `…`.
        let spark = |i: usize, column: &fast_aws_core::Column| {
            let spec = r.metrics.iter().find(|m| m.column == i)?;
            Some(match app.sparkline(&r.id, i)? {
                Some(points) => {
                    let points = match column.short_title {
                        Some(_) if short => last_hours(points),
                        _ => points,
                    };
                    fast_aws_core::metrics::sparkline_in(
                        app.display.sparkline,
                        points,
                        spec.summary,
                        spec.unit,
                    )
                }
                None => "…".to_owned(),
            })
        };
        Row::new(shown.iter().map(|(i, column)| {
            let Some(c) = r.cells.get(*i) else {
                return Cell::from("");
            };
            match spark(*i, column) {
                // Cyan, not dim: it must stay readable on the highlighted row.
                Some(text) => Cell::from(text).style(SPARK),
                None => match c.time_ms {
                    Some(ms) => {
                        Cell::from(fast_aws_core::fmt::time::show_millis(ms, app.display.utc))
                            .style(tone(c.tone))
                    }
                    None => Cell::from(c.text.as_str()).style(tone(c.tone)),
                },
            }
        }))
    });
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(Color::DarkGray).bold())
        .highlight_symbol("▸")
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    let mut state = TableState::new().with_selected(list.selected_index());
    frame.render_stateful_widget(table, area, &mut state);

    if list.total() > 0 && list.visible_rows().len() == 0 {
        let msg = Paragraph::new(format!("nothing matches /{}", list.filter)).style(DIM);
        frame.render_widget(msg, area.inner(ratatui::layout::Margin::new(2, 2)));
    } else if list.status == LoadStatus::Done && list.total() == 0 {
        let place = match list.kind.scope() {
            Scope::Regional => app.region.clone(),
            Scope::Global => "this account".to_owned(),
            Scope::Pinned(region) => region.to_owned(),
        };
        let msg = Paragraph::new(format!("no {} in {place}", list.kind.title())).style(DIM);
        frame.render_widget(msg, area.inner(ratatui::layout::Margin::new(2, 2)));
    }
}

/// The least a flexible column (usually the name) gets when space is tight.
const MIN_FILL: u16 = 14;

/// Hours a narrowed sparkline shows: the most recent ones.
const SHORT_HOURS: usize = 6;

/// A narrowed sparkline's width: three characters, a space and the number.
const SHORT_SPARK: u16 = 10;

/// The columns drawn in `width` cells, each with its index into a row's cells,
/// and whether sparklines are narrowed. Sparkline columns are left out while
/// sparklines are off, and narrowed to the last [`SHORT_HOURS`] when the full
/// ones would squeeze the rest.
fn shown_columns(
    columns: &[fast_aws_core::Column],
    metrics: bool,
    width: u16,
) -> (Vec<(usize, fast_aws_core::Column)>, bool) {
    let full: Vec<_> = columns
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, c)| metrics || c.short_title.is_none())
        .collect();
    let fits = |shown: &[(usize, fast_aws_core::Column)]| {
        let columns: Vec<_> = shown.iter().map(|(_, c)| *c).collect();
        fits(&columns, width)
    };
    if !metrics || fits(&full) {
        return (full, false);
    }
    let narrowed = full
        .into_iter()
        .map(|(i, c)| match c.short_title {
            Some(title) => (
                i,
                fast_aws_core::Column {
                    title,
                    width: ColumnWidth::Fixed(SHORT_SPARK),
                    ..c
                },
            ),
            None => (i, c),
        })
        .collect();
    (narrowed, true)
}

/// The most recent [`SHORT_HOURS`] of hourly points.
fn last_hours(points: &[f64]) -> &[f64] {
    &points[points.len().saturating_sub(SHORT_HOURS)..]
}

/// Whether fixed columns get their full width with every flexible one keeping
/// at least [`MIN_FILL`].
fn fits(columns: &[fast_aws_core::Column], width: u16) -> bool {
    const SPACING: u16 = 2;
    const HIGHLIGHT: u16 = 1;
    let fixed: u16 = columns
        .iter()
        .filter_map(|c| match c.width {
            ColumnWidth::Fixed(n) => Some(n),
            ColumnWidth::Fill(_) => None,
        })
        .sum();
    let fills = columns
        .iter()
        .filter(|c| matches!(c.width, ColumnWidth::Fill(_)))
        .count() as u16;
    let spacing = SPACING * columns.len().saturating_sub(1) as u16 + HIGHLIGHT;
    fixed + spacing + MIN_FILL * fills <= width
}

/// Column constraints for `width` cells. Normally fixed columns get their
/// width and flexible ones share the rest; when that would leave a flexible
/// column under [`MIN_FILL`], fixed columns become maximums so they shrink
/// instead of the name disappearing. Sparklines keep their width: cut, they
/// lose the number at their end.
fn column_widths(columns: &[fast_aws_core::Column], width: u16) -> Vec<Constraint> {
    let tight = !fits(columns, width);
    columns
        .iter()
        .map(|c| match (c.width, tight) {
            (ColumnWidth::Fixed(n), false) => Constraint::Length(n),
            (ColumnWidth::Fixed(n), true) if c.short_title.is_some() => Constraint::Length(n),
            (ColumnWidth::Fixed(n), true) => Constraint::Max(n),
            (ColumnWidth::Fill(w), false) => Constraint::Fill(w),
            (ColumnWidth::Fill(_), true) => Constraint::Min(MIN_FILL),
        })
        .collect()
}

/// A one-line summary of an error, for title bars.
fn short(e: &AwsError) -> String {
    e.code.clone().unwrap_or_else(|| format!("{:?}", e.kind))
}

fn error_text<'a>(app: &App, err: &'a AwsError) -> Text<'a> {
    let (title, advice): (&str, String) = match err.kind {
        ErrorKind::SessionExpired => (
            match app.current_profile().map(|p| &p.auth) {
                Some(Auth::ConsoleLogin) => "Login session expired",
                _ => "SSO session expired",
            },
            match app.current_profile().and_then(|p| p.login_command()) {
                Some(cmd) => format!("Press L to run `{}`.", cmd.join(" ")),
                None => "Log in again with the AWS CLI.".into(),
            },
        ),
        ErrorKind::AccessDenied => (
            "Access denied",
            format!(
                "Profile `{}` is missing a permission this view needs.",
                app.profile
            ),
        ),
        ErrorKind::Credentials => (
            "Credentials problem",
            format!("Check how profile `{}` gets its credentials.", app.profile),
        ),
        ErrorKind::Throttled => (
            "Throttled by AWS",
            "Wait a moment, then press r to retry.".into(),
        ),
        ErrorKind::Endpoint => (
            "Region not available",
            format!(
                "{} isn't usable from this account. Press R to pick another region.",
                app.region
            ),
        ),
        ErrorKind::Network => (
            "Can't reach AWS",
            "Check the network, then press r to retry.".into(),
        ),
        ErrorKind::NotFound => (
            "Not found",
            "It may have just been deleted. Press r to reload.".into(),
        ),
        ErrorKind::Other => ("Request failed", "Press r to retry.".into()),
    };
    let mut lines = vec![
        Line::styled(title, Style::new().fg(Color::Red).bold()),
        Line::raw(""),
        Line::raw(advice),
        Line::raw(""),
    ];
    if let Some(code) = &err.code {
        lines.push(Line::from(vec![
            Span::styled("code: ", DIM),
            Span::raw(code.as_str()),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("detail: ", DIM),
        Span::raw(err.message.as_str()),
    ]));
    Text::from(lines)
}

fn draw_error(frame: &mut Frame, area: Rect, app: &App, err: &AwsError) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::Red))
        .padding(ratatui::widgets::Padding::horizontal(1));
    let p = Paragraph::new(error_text(app, err))
        .wrap(Wrap { trim: false })
        .block(block);
    frame.render_widget(p, area.inner(ratatui::layout::Margin::new(1, 0)));
}

fn draw_detail_title(frame: &mut Frame, area: Rect, d: &DetailView) {
    let mode = if d.raw { "raw" } else { "fields" };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", d.title), Style::new().fg(ACCENT).bold()),
        Span::styled("› ", DIM),
        Span::styled(d.id.clone(), Style::new().bold()),
        Span::styled(format!("  [{mode}]"), DIM),
    ]);
    frame.render_widget(line, area);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &App, d: &DetailView) {
    let text = match &d.state {
        DetailState::Loading => Text::styled(" loading…", DIM),
        DetailState::Failed(err) => return draw_error(frame, area, app, err),
        DetailState::Ready(detail) if d.raw => Text::raw(detail.raw.clone()),
        DetailState::Ready(detail) => detail::layout(&detail.fields)
            .into_iter()
            .map(|l| {
                let mut spans = vec![Span::raw("  ".repeat(l.indent + 1))];
                if l.bullet {
                    spans.push(Span::styled("- ", DIM));
                }
                if let Some(k) = l.key {
                    spans.push(Span::styled(k, Style::new().fg(ACCENT)));
                    spans.push(Span::styled(":", DIM));
                    if l.value.is_some() {
                        spans.push(Span::raw(" "));
                    }
                }
                // Timestamps are stored in UTC and shown in the display zone.
                spans.extend(l.value.map(|v| {
                    Span::raw(
                        fast_aws_core::fmt::time::show_if_timestamp(&v, app.display.utc)
                            .unwrap_or(v),
                    )
                }));
                Line::from(spans)
            })
            .collect(),
    };
    frame.render_widget(Paragraph::new(text).scroll((d.scroll, 0)), area);
}

fn hints(pairs: &[(&'static str, &'static str)]) -> Line<'static> {
    hints_with(pairs, &[])
}

/// Key hints, after the selected row's own keys (`S simulate`).
fn hints_with(
    pairs: &[(&'static str, &'static str)],
    row_keys: &[(String, &'static str)],
) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, label) in row_keys {
        spans.push(Span::styled(
            key.clone(),
            KEY.add_modifier(Modifier::REVERSED),
        ));
        spans.push(Span::styled(format!(" {label}  "), DIM));
    }
    for (key, label) in pairs {
        spans.push(Span::styled(*key, KEY));
        spans.push(Span::styled(format!(" {label}  "), DIM));
    }
    Line::from(spans)
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let row_keys: Vec<(String, &'static str)> = app
        .list
        .selected_row()
        .map(|r| {
            r.queries
                .iter()
                .map(|q| (q.key.to_string(), q.label))
                .collect()
        })
        .unwrap_or_default();
    let line = match (&app.overlay, &app.flash) {
        (Overlay::Prompt(query, input), _) => Line::from(vec![
            Span::styled(format!(" {}: ", query.prompt), KEY),
            Span::raw(input.clone()),
            Span::styled("▏", Style::new().fg(ACCENT)),
            Span::styled("   enter run · esc cancel", DIM),
        ]),
        (Overlay::Palette(input, _), _) => Line::from(vec![
            Span::styled(" :", KEY),
            Span::raw(input.clone()),
            Span::styled("▏", Style::new().fg(ACCENT)),
            Span::styled("   ↑/↓ choose · enter open · esc cancel", DIM),
        ]),
        (Overlay::Filter, _) => Line::from(vec![
            Span::styled(" /", KEY),
            Span::raw(app.list.filter.clone()),
            Span::styled("▏", Style::new().fg(ACCENT)),
            Span::styled("   enter keep · esc clear", DIM),
        ]),
        (_, Some(flash)) => Line::styled(format!(" {flash}"), Style::new().fg(Color::Yellow)),
        _ if app.login_would_help() => hints(&[("L", "log in"), ("P", "profile"), ("q", "quit")]),
        _ => match app.screen {
            Screen::List if !app.back.is_empty() => hints_with(
                &[
                    ("enter", "open"),
                    ("d", "describe"),
                    ("/", "filter"),
                    ("r", "refresh"),
                    ("esc", "back"),
                    (":", "jump"),
                ],
                &row_keys,
            ),
            Screen::List => hints_with(
                &[
                    (":", "jump"),
                    ("/", "filter"),
                    ("enter", "open"),
                    ("d", "describe"),
                    ("g", "go to"),
                    ("r", "refresh"),
                    ("P", "profile"),
                    ("R", "region"),
                    ("q", "quit"),
                ],
                &row_keys,
            ),
            Screen::Detail(_) => hints(&[
                ("j/k", "scroll"),
                ("tab", "fields/raw"),
                ("r", "refresh"),
                ("esc", "back"),
            ]),
        },
    };
    frame.render_widget(line, area);
}

/// A bordered box centred in `area`, cleared, with `title`.
fn popup(frame: &mut Frame, area: Rect, width: u16, height: u16, title: &str) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    let rect = Rect::new(
        area.x + (area.width - w) / 2,
        area.y + (area.height - h) / 2,
        w,
        h,
    );
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(format!(" {title} "));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn picker_line(text: String, selected: bool) -> Line<'static> {
    if selected {
        Line::styled(format!("▸ {text}"), Style::new().bg(Color::DarkGray).bold())
    } else {
        Line::raw(format!("  {text}"))
    }
}

/// Scrolls a list of `len` lines so `at` stays visible in `height` rows.
fn scroll_to(at: usize, height: u16) -> u16 {
    (at + 1).saturating_sub(usize::from(height)) as u16
}

fn draw_palette(frame: &mut Frame, area: Rect, app: &App, input: &str, at: usize) {
    let title_width = app
        .registry
        .palette()
        .map(|k| k.title().chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = app
        .registry
        .search(input)
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let aliases = format!(":{}", k.aliases().join("  :"));
            let line = Line::from(vec![
                Span::raw(if i == at { "▸ " } else { "  " }),
                // Titles are padded to the longest one so the aliases line up.
                Span::styled(
                    format!("{:<width$}  ", k.title(), width = title_width),
                    Style::new().bold(),
                ),
                Span::styled(aliases, DIM),
            ]);
            if i == at {
                line.style(Style::new().bg(Color::DarkGray))
            } else {
                line
            }
        })
        .collect();
    let height = lines.len().clamp(1, 14) as u16 + 2;
    let inner = popup(frame, area, 80, height, "jump to");
    if lines.is_empty() {
        frame.render_widget(Paragraph::new(Text::styled("no match", DIM)), inner);
    } else {
        frame.render_widget(
            Paragraph::new(lines).scroll((scroll_to(at, inner.height), 0)),
            inner,
        );
    }
}

fn draw_profiles(frame: &mut Frame, area: Rect, app: &App, at: usize) {
    let lines: Vec<Line> = app
        .profile_entries()
        .into_iter()
        .enumerate()
        .map(|(i, e)| match e {
            PickerEntry::Heading(h) => Line::styled(h, DIM.add_modifier(Modifier::ITALIC)),
            PickerEntry::Profile(name) => {
                let current = if name == app.profile { " ●" } else { "" };
                picker_line(format!("{name}{current}"), i == at)
            }
        })
        .collect();
    let inner = popup(frame, area, 44, lines.len() as u16 + 2, "profile");
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_to(at, inner.height), 0)),
        inner,
    );
}

fn draw_relations(frame: &mut Frame, area: Rect, app: &App, at: usize) {
    let relations = &app.relations_menu;
    let lines: Vec<Line> = relations
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let title = app
                .registry
                .get(r.kind)
                .map(|k| k.title())
                .unwrap_or(r.kind);
            let line = picker_line(r.label.clone(), i == at);
            let mut spans = line.spans;
            spans.push(Span::styled(format!("  {title}"), DIM));
            Line::from(spans).style(line.style)
        })
        .collect();
    let inner = popup(frame, area, 96, lines.len() as u16 + 2, "go to");
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_to(at, inner.height), 0)),
        inner,
    );
}

fn draw_regions(frame: &mut Frame, area: Rect, app: &App, at: usize) {
    let lines: Vec<Line> = REGIONS
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let current = if *r == app.region { " ●" } else { "" };
            picker_line(format!("{r}{current}"), i == at)
        })
        .collect();
    let inner = popup(frame, area, 30, 14, "region");
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll_to(at, inner.height), 0)),
        inner,
    );
}

fn draw_settings(frame: &mut Frame, area: Rect, app: &App, at: usize) {
    let lines: Vec<Line> = crate::app::SETTINGS
        .iter()
        .enumerate()
        .map(|(i, (setting, label))| {
            picker_line(
                format!("{label:<36}{}", app.setting_value(*setting)),
                i == at,
            )
        })
        .collect();
    let inner = popup(
        frame,
        area,
        56,
        lines.len() as u16 + 3,
        "settings · enter changes · esc closes",
    );
    let note = Line::styled("  for this session; set them for good in config.toml", DIM);
    frame.render_widget(
        Paragraph::new(lines.into_iter().chain([note]).collect::<Vec<_>>()),
        inner,
    );
}

fn draw_help(frame: &mut Frame, area: Rect) {
    const KEYS: &[(&str, &str)] = &[
        (":", "jump to a resource type (:q quits)"),
        ("/", "filter the list"),
        ("j/k ↑/↓", "move · PgUp/PgDn · Home/End"),
        ("enter", "open: drill in, or describe"),
        ("g", "go to a related resource"),
        ("d", "describe the selected item"),
        ("tab", "fields / raw (in describe)"),
        ("r", "refresh"),
        ("n", "load more (lists stop after 5 pages)"),
        ("P / R", "switch profile / region"),
        ("Z", "times in local time / UTC"),
        (",", "settings: borders, sparklines, times"),
        ("L", "SSO login for this profile"),
        (":unlock", "allow actions on a prod profile (this session)"),
        ("esc / q", "back · q quits at the top level"),
        ("ctrl-c", "quit"),
    ];
    let lines: Vec<Line> = KEYS
        .iter()
        .map(|(k, v)| Line::from(vec![Span::styled(format!("{k:<11}"), KEY), Span::raw(*v)]))
        .collect();
    let inner = popup(
        frame,
        area,
        54,
        lines.len() as u16 + 2,
        "keys · any key closes",
    );
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Msg;
    use crate::app::tests::{app, empty_app};
    use fast_aws_core::{CallerIdentity, Detail, Page};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use serde_json::json;

    fn render(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(64, 12)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().to_string()
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn ready(app: &mut App) {
        app.handle_msg(Msg::Identity {
            generation: 1,
            result: Ok(CallerIdentity {
                account: "123456789012".into(),
                arn: "arn:aws:sts::123456789012:assumed-role/Dev/me".into(),
            }),
        });
    }

    #[test]
    fn list_view() {
        let mut app = app();
        ready(&mut app);
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn list_while_loading_and_filtered() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Char('w'));
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn sso_expired_pane() {
        let mut app = empty_app();
        app.handle_msg(Msg::Identity {
            generation: 1,
            result: Err(AwsError::new(
                ErrorKind::SessionExpired,
                None,
                "the SSO token has expired and cannot be refreshed",
            )),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn access_denied_pane() {
        let mut app = empty_app();
        ready(&mut app);
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Err(AwsError::new(
                ErrorKind::AccessDenied,
                Some("UnauthorizedOperation".into()),
                "You are not authorized to perform this operation.",
            )),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn detail_view() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char('d'));
        app.handle_msg(Msg::Detail {
            generation: 2,
            result: Ok(Detail {
                fields: json!({
                    "InstanceId": "a",
                    "State": "running",
                    "SecurityGroups": [{"GroupId": "sg-1", "GroupName": "web"}],
                    "Tags": {"Name": "web-1"},
                }),
                raw: "Instance { .. }".into(),
            }),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn profile_picker() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char('P'));
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn drilled_in_list_shows_a_breadcrumb() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Enter);
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Ok(Page {
                rows: vec![fast_aws_core::Row::new(
                    "p1",
                    vec!["bolt".into(), "p1".into()],
                )],
                next: None,
            }),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn paused_list_offers_more() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char('r'));
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Ok(Page {
                rows: vec![fast_aws_core::Row::new(
                    "a",
                    vec!["web-1".into(), "a".into()],
                )],
                next: Some("t2".into()),
            }),
        });
        app.handle_msg(Msg::Paused {
            generation: 2,
            next: "t2".into(),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn palette_lists_matches_with_a_highlight() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Char('t'));
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn header_badges_for_read_only_and_prod() {
        let mut app = app();
        ready(&mut app);
        app.set_safety(
            toml::from_str("[profiles.acme-dev]\nprod = true\nread_only = true").unwrap(),
            false,
        );
        app.display.utc = true;
        let mut terminal = Terminal::new(TestBackend::new(90, 3)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        insta::assert_snapshot!(terminal.backend().to_string());
    }

    #[test]
    fn sparklines_replace_their_cell() {
        let (mut app, effects) = crate::app::tests::app_with_metrics(true);
        ready(&mut app);
        let crate::app::Effect::LoadMetrics { scope, .. } = &effects[0] else {
            panic!()
        };
        app.handle_msg(Msg::Metrics {
            scope: scope.clone(),
            result: Ok(std::collections::HashMap::from([(
                ("a".to_owned(), 1),
                vec![0.0, 1.0, 4.0, 2.0],
            )])),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn the_footer_shows_the_selected_rows_own_keys() {
        let mut app = empty_app();
        ready(&mut app);
        app.handle_msg(Msg::Page {
            generation: 1,
            result: Ok(Page {
                rows: vec![
                    fast_aws_core::Row::new("r", vec!["role".into(), "r".into()]).query(
                        'S',
                        "simulate",
                        "actions",
                        "fake:part",
                        "r",
                    ),
                ],
                next: None,
            }),
        });
        let mut terminal = Terminal::new(TestBackend::new(110, 6)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        insta::assert_snapshot!(terminal.backend().to_string());
    }

    #[test]
    fn bordered_layout_with_bar_sparklines() {
        let (mut app, effects) = crate::app::tests::app_with_metrics(true);
        ready(&mut app);
        app.display = crate::app::Display {
            borders: true,
            sparkline: fast_aws_core::metrics::SparkStyle::Bars,
            utc: true,
        };
        let crate::app::Effect::LoadMetrics { scope, .. } = &effects[0] else {
            panic!()
        };
        app.handle_msg(Msg::Metrics {
            scope: scope.clone(),
            result: Ok(std::collections::HashMap::from([(
                ("a".to_owned(), 1),
                vec![0.0, 1.0, 4.0, 2.0],
            )])),
        });
        insta::assert_snapshot!(render(&app));
    }

    #[test]
    fn settings_panel() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char(','));
        let mut terminal = Terminal::new(TestBackend::new(70, 12)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        insta::assert_snapshot!(terminal.backend().to_string());
    }

    #[test]
    fn a_narrow_screen_keeps_the_name_column() {
        use fast_aws_core::Column;
        const WIDE: &[Column] = &[
            Column::fill("NAME", 1),
            Column::fixed("ID", 40),
            Column::fixed("STATE", 40),
        ];
        assert_eq!(column_widths(WIDE, 200)[0], Constraint::Fill(1));
        let tight = column_widths(WIDE, 60);
        assert_eq!(tight[0], Constraint::Min(MIN_FILL));
        assert_eq!(tight[1], Constraint::Max(40));
    }

    #[test]
    fn sparkline_columns_hide_while_off_and_narrow_when_tight() {
        use fast_aws_core::Column;
        const COLUMNS: &[Column] = &[
            Column::fill("NAME", 1),
            Column::fixed("ID", 40),
            Column::spark("CPU 24h (peak)", 20, "CPU 6h"),
        ];
        let titles = |metrics, width| {
            let (shown, short) = shown_columns(COLUMNS, metrics, width);
            let titles: Vec<_> = shown.iter().map(|(i, c)| (*i, c.title)).collect();
            (titles, short)
        };
        assert_eq!(titles(false, 200), (vec![(0, "NAME"), (1, "ID")], false));
        assert_eq!(
            titles(true, 200),
            (vec![(0, "NAME"), (1, "ID"), (2, "CPU 24h (peak)")], false)
        );
        assert_eq!(
            titles(true, 70),
            (vec![(0, "NAME"), (1, "ID"), (2, "CPU 6h")], true)
        );
        // Even tighter, other columns shrink; the sparkline keeps its width.
        let (shown, _) = shown_columns(COLUMNS, true, 60);
        let columns: Vec<_> = shown.iter().map(|(_, c)| *c).collect();
        let widths = column_widths(&columns, 60);
        assert_eq!(widths[1], Constraint::Max(40));
        assert_eq!(widths[2], Constraint::Length(SHORT_SPARK));
    }

    #[test]
    fn narrowed_sparklines_show_the_last_six_hours() {
        let points: Vec<f64> = (0..24).map(f64::from).collect();
        assert_eq!(last_hours(&points), [18.0, 19.0, 20.0, 21.0, 22.0, 23.0]);
        assert_eq!(last_hours(&points[..2]), [0.0, 1.0]);
        let text = fast_aws_core::metrics::sparkline(
            last_hours(&points),
            fast_aws_core::metrics::Summary::Peak,
            "%",
        );
        assert!(text.chars().count() as u16 <= SHORT_SPARK, "{text}");
    }

    #[test]
    fn empty_list_says_so() {
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char('r'));
        app.handle_msg(Msg::Page {
            generation: 2,
            result: Ok(Page::default()),
        });
        insta::assert_snapshot!(render(&app));
    }
}
