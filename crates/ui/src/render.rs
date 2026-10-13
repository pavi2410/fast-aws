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
                Some(err) => draw_error(frame, body, app, err),
                None => draw_table(frame, body, app),
            }
        }
        Screen::Detail(d) => {
            draw_detail_title(frame, title, d);
            draw_detail(frame, body, app, d);
        }
    }
    draw_footer(frame, footer, app);

    match &app.overlay {
        Overlay::None | Overlay::Filter => {}
        Overlay::Palette(input, at) => draw_palette(frame, body, app, input, *at),
        Overlay::Profiles(at) => draw_profiles(frame, body, app, *at),
        Overlay::Regions(at) => draw_regions(frame, body, app, *at),
        Overlay::Help => draw_help(frame, body),
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
    let right = Line::from(vec![Span::styled("  ? ", KEY), Span::styled("help ", DIM)]);
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
    let status = match &list.status {
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
    };
    draw_split(frame, area, Line::from(spans), Line::from(status));
}

fn draw_table(frame: &mut Frame, area: Rect, app: &App) {
    let list = &app.list;
    let columns = list.kind.columns();
    let widths: Vec<Constraint> = columns
        .iter()
        .map(|c| match c.width {
            ColumnWidth::Fixed(n) => Constraint::Length(n),
            ColumnWidth::Fill(w) => Constraint::Fill(w),
        })
        .collect();
    let header = Row::new(columns.iter().map(|c| Cell::from(c.title))).style(DIM.bold());
    let rows = list.visible_rows().map(|r| {
        Row::new(
            r.cells
                .iter()
                .map(|c| Cell::from(c.text.as_str()).style(tone(c.tone))),
        )
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
                spans.extend(l.value.map(Span::raw));
                Line::from(spans)
            })
            .collect(),
    };
    frame.render_widget(Paragraph::new(text).scroll((d.scroll, 0)), area);
}

fn hints(pairs: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (key, label) in pairs {
        spans.push(Span::styled(*key, KEY));
        spans.push(Span::styled(format!(" {label}  "), DIM));
    }
    Line::from(spans)
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match (&app.overlay, &app.flash) {
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
            Screen::List if !app.back.is_empty() => hints(&[
                ("enter", "open"),
                ("d", "describe"),
                ("/", "filter"),
                ("r", "refresh"),
                ("esc", "back"),
                (":", "jump"),
            ]),
            Screen::List => hints(&[
                (":", "jump"),
                ("/", "filter"),
                ("enter", "open"),
                ("d", "describe"),
                ("r", "refresh"),
                ("P", "profile"),
                ("R", "region"),
                ("q", "quit"),
            ]),
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

fn draw_help(frame: &mut Frame, area: Rect) {
    const KEYS: &[(&str, &str)] = &[
        (":", "jump to a resource type (:q quits)"),
        ("/", "filter the list"),
        ("j/k ↑/↓", "move · PgUp/PgDn · Home/End"),
        ("enter", "open: drill in, or describe"),
        ("d", "describe the selected item"),
        ("tab", "fields / raw (in describe)"),
        ("r", "refresh"),
        ("n", "load more (lists stop after 5 pages)"),
        ("P / R", "switch profile / region"),
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
    use crate::app::tests::app;
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
        let mut app = app();
        press(&mut app, KeyCode::Char(':'));
        for c in "thing".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
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
        let mut app = app();
        ready(&mut app);
        press(&mut app, KeyCode::Char(':'));
        for c in "thing".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        app.handle_msg(Msg::Page {
            generation: 2,
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
        let mut terminal = Terminal::new(TestBackend::new(90, 3)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        insta::assert_snapshot!(terminal.backend().to_string());
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
