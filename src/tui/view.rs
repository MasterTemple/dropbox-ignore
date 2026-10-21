//! Drawing the TUI.

use std::path::Path;

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Clear, HighlightSpacing, List, ListItem, Padding, Paragraph, Wrap,
    },
};
use unicode_width::UnicodeWidthStr;

use super::app::{App, Entry, Modal, Row, expandable, verdict_for};
use crate::{
    dropbox::SyncStatus,
    model::{Node, State, daemon_mark, sync_mark, sync_text},
    plan::Action,
    rules::{Rule, Verdict},
    ui::{self, IconSet, Kind, Mark},
};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::DarkGray;
const SELECTED_BG: Color = Color::Indexed(236);

fn color(mark: Mark) -> Color {
    match mark {
        Mark::Synced | Mark::Success | Mark::Tracked => Color::Green,
        Mark::Syncing | Mark::Restored => Color::Cyan,
        Mark::Unsyncable | Mark::Error => Color::Red,
        Mark::Excluded => Color::Blue,
        Mark::Ignored | Mark::Outside | Mark::Unknown => MUTED,
        Mark::IgnoredManual => Color::Magenta,
        Mark::Pending | Mark::Warning => Color::Yellow,
    }
}

fn glyph(mark: Mark) -> Span<'static> {
    Span::styled(mark.glyph(), color(mark))
}

fn muted(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), MUTED)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);

    let (tree_area, details_area) = split_body(body, app.show_details);
    app.tree_area = tree_area;
    draw_tree(frame, app, tree_area);
    if let Some(area) = details_area {
        draw_details(frame, app, area);
    }
    draw_footer(frame, app, footer);

    match &app.modal {
        Some(Modal::Help) => draw_help(frame),
        Some(Modal::Confirm { title, lines, .. }) => draw_confirm(frame, title, lines),
        Some(Modal::Plan { .. }) => draw_plan(frame, app),
        None => {}
    }
}

fn split_body(body: Rect, show_details: bool) -> (Rect, Option<Rect>) {
    if !show_details {
        (body, None)
    } else if body.width >= 96 {
        let [tree, details] =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(42)]).areas(body);
        (tree, Some(details))
    } else {
        let [tree, details] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(10)]).areas(body);
        (tree, Some(details))
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let right = match &app.daemon_status {
        Some(status) => {
            let mark = daemon_mark(status);
            Line::from(vec![
                muted("Dropbox "),
                glyph(mark),
                Span::styled(format!(" {status} "), color(mark)),
            ])
        }
        None => Line::from(vec![
            muted("Dropbox "),
            glyph(Mark::Unknown),
            muted(" no daemon "),
        ]),
    };
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(right.width() as u16),
    ])
    .areas(area);
    frame.render_widget(Paragraph::new(right), right_area);

    // keep the end of the path visible when it doesn't fit
    let path = ui::tilde(&app.root.path);
    let room = (left_area.width as usize).saturating_sub(6);
    let path = if path.width() > room {
        let tail: String = path
            .chars()
            .rev()
            .take(room.saturating_sub(1))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        path
    };
    let left = Line::from(vec![
        Span::styled(" di ", Style::new().bg(ACCENT).fg(Color::Black).bold()),
        Span::raw(" "),
        Span::styled(path, Style::new().bold()),
    ]);
    frame.render_widget(Paragraph::new(left), left_area);
}

fn panel(title: &str, focused: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            Style::new().fg(MUTED)
        })
        .title(Line::from(format!(" {title} ")).bold())
}

fn draw_tree(frame: &mut Frame, app: &mut App, area: Rect) {
    let base = app.root.path.clone();
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| ListItem::new(tree_line(row, &base)))
        .collect();

    let block = panel("Tree", app.modal.is_none()).title_bottom(counts_line(app).right_aligned());
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(SELECTED_BG).add_modifier(Modifier::BOLD))
        .highlight_symbol(Line::from(Span::styled("▍", ACCENT)))
        .highlight_spacing(HighlightSpacing::Always)
        .scroll_padding(3);
    frame.render_stateful_widget(list, area, &mut app.list);
}

struct Guides {
    branch: &'static str,
    last: &'static str,
    pipe: &'static str,
    blank: &'static str,
    open: &'static str,
    closed: &'static str,
}

fn guides() -> Guides {
    match ui::icons() {
        IconSet::Ascii => Guides {
            branch: "|- ",
            last: "`- ",
            pipe: "|  ",
            blank: "   ",
            open: "v ",
            closed: "> ",
        },
        _ => Guides {
            branch: "├─ ",
            last: "└─ ",
            pipe: "│  ",
            blank: "   ",
            open: "▾ ",
            closed: "▸ ",
        },
    }
}

fn tree_line(row: &Row, base: &Path) -> Line<'static> {
    let g = guides();
    let mut spans = Vec::new();
    if row.depth > 0 {
        let mut prefix: String = row
            .ancestors_last
            .iter()
            .map(|&last| if last { g.blank } else { g.pipe })
            .collect();
        prefix.push_str(if row.last { g.last } else { g.branch });
        spans.push(muted(prefix));
    }

    let node = match &row.entry {
        Entry::Node(node) => node,
        Entry::Note(text, mark) => {
            spans.push(Span::raw("  "));
            spans.push(glyph(*mark));
            spans.push(Span::styled(
                format!(" {text}"),
                Style::new().fg(color(*mark)).italic(),
            ));
            return Line::from(spans);
        }
    };

    spans.push(Span::styled(
        if row.expanded {
            g.open
        } else if expandable(node) && row.depth > 0 {
            g.closed
        } else {
            "  "
        },
        MUTED,
    ));
    spans.push(glyph(node.state.mark()));
    spans.push(Span::raw(" "));
    spans.push(name_span(node));

    let mut width = ui::entry_name_width(&node.name, &node.kind);
    if let Kind::Symlink(target) = &node.kind {
        let target = target
            .as_deref()
            .map(ui::tilde)
            .unwrap_or_else(|| "?".into());
        let suffix = format!(" {} {target}", ui::arrow());
        width += suffix.width();
        spans.push(muted(suffix));
    }

    let label = label_spans(&node.state, base);
    if !label.is_empty() {
        spans.push(Span::raw(" ".repeat(row.column.saturating_sub(width) + 2)));
        spans.extend(label);
    }
    Line::from(spans)
}

fn name_span(node: &Node) -> Span<'static> {
    let icon = node.kind.icon(&node.name);
    let (text, style) = match &node.kind {
        Kind::Dir => (
            format!("{icon}{}/", node.name),
            Style::new().fg(Color::Blue).bold(),
        ),
        Kind::Symlink(_) => (format!("{icon}{}", node.name), Style::new().fg(Color::Cyan)),
        _ if node.name == crate::rules::FILE_NAME => (
            format!("{icon}{}", node.name),
            Style::new().fg(Color::Yellow).bold(),
        ),
        _ => (format!("{icon}{}", node.name), Style::new()),
    };
    if node.state.is_tracked() {
        Span::styled(text, style)
    } else {
        Span::styled(text, MUTED)
    }
}

fn rule_spans(rule: &Rule, base: &Path) -> Vec<Span<'static>> {
    vec![
        Span::styled(rule.pattern.clone(), Style::new().fg(Color::Gray).italic()),
        muted(format!(" in {}", ui::pretty_path(&rule.source, Some(base)))),
    ]
}

fn label_spans(state: &State, base: &Path) -> Vec<Span<'static>> {
    let mark = state.mark();
    let tag = |text: &str| Span::styled(text.to_string(), color(mark));
    match state {
        State::Tracked(SyncStatus::UpToDate | SyncStatus::Unknown) => Vec::new(),
        State::Tracked(status) => vec![tag(&sync_text(status))],
        State::Pending(rule) => {
            let mut spans = vec![tag("pending ignore"), Span::raw(" ")];
            spans.extend(rule_spans(rule, base));
            spans
        }
        State::Ignored(Some(rule)) => {
            let mut spans = vec![tag("ignored"), Span::raw(" ")];
            spans.extend(rule_spans(rule, base));
            spans
        }
        State::Ignored(None) => vec![tag("ignored manually")],
        State::InsideIgnored(dir) => vec![
            tag("untracked"),
            muted(format!(" inside {}", ui::tilde(dir))),
        ],
        State::Excluded => vec![tag("online only (excluded)")],
        State::Outside => vec![tag("not in Dropbox")],
    }
}

fn counts_line(app: &App) -> Line<'static> {
    let mut counts = [0u64; 8];
    let order = [
        Mark::Synced,
        Mark::Tracked,
        Mark::Syncing,
        Mark::Unsyncable,
        Mark::Excluded,
        Mark::Pending,
        Mark::Ignored,
        Mark::IgnoredManual,
    ];
    for row in app.rows.iter().skip(1) {
        if let Some(node) = row.node()
            && let Some(i) = order.iter().position(|m| *m == node.state.mark())
        {
            counts[i] += 1;
        }
    }
    let mut spans = vec![Span::raw(" ")];
    for (mark, n) in order.iter().zip(counts) {
        if n > 0 {
            spans.push(glyph(*mark));
            spans.push(Span::styled(format!(" {n}  "), color(*mark)));
        }
    }
    Line::from(spans)
}

fn draw_details(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = panel("Details", false).padding(Padding::horizontal(1));
    let Some(node) = app.selected_node().cloned() else {
        frame.render_widget(block, area);
        return;
    };
    let base = app.root.path.clone();
    let key = |k: &str| Span::styled(format!("{k:<9} "), MUTED);
    let mut title = node.clone();
    if node.path == base {
        title.name = node
            .path
            .file_name()
            .map_or_else(|| "/".into(), |n| n.to_string_lossy().into_owned());
    }
    let mut lines = vec![
        Line::from(vec![
            glyph(node.state.mark()),
            Span::raw(" "),
            name_span(&title).bold(),
        ]),
        Line::from(muted(node.kind.noun())),
        Line::from(muted(if node.path == base {
            ui::tilde(&node.path)
        } else {
            format!("./{}", ui::pretty_path(&node.path, Some(&base)))
        })),
        Line::default(),
    ];

    let mark = node.state.mark();
    let tracking = match &node.state {
        State::Tracked(_) => "tracked".to_string(),
        State::Pending(_) => "tracked; a rule wants it ignored".into(),
        State::Ignored(Some(_)) => "ignored by rule".into(),
        State::Ignored(None) => "ignored manually (no rule)".into(),
        State::InsideIgnored(dir) => format!("untracked, inside {}", ui::tilde(dir)),
        State::Excluded => "online only: excluded by selective sync".into(),
        State::Outside => "not inside Dropbox".into(),
    };
    let tracking_mark = if node.state.is_tracked() && mark != Mark::Pending {
        Mark::Tracked
    } else {
        mark
    };
    lines.push(Line::from(vec![
        key("Tracking"),
        glyph(tracking_mark),
        Span::styled(format!(" {tracking}"), color(tracking_mark)),
    ]));

    if node.state.is_tracked() {
        let status = match &node.state {
            State::Tracked(status) => status.clone(),
            _ => SyncStatus::Unknown,
        };
        if status != SyncStatus::Unknown || !app.dropbox.daemon_connected() {
            let m = sync_mark(&status);
            let text = if status == SyncStatus::Unknown {
                "unknown".into()
            } else {
                sync_text(&status)
            };
            lines.push(Line::from(vec![
                key("Sync"),
                glyph(m),
                Span::styled(format!(" {text}"), color(m)),
            ]));
        }
    }

    let verdict = verdict_for(app, &node);
    let mut rule_line = vec![key("Rule")];
    match &verdict {
        Verdict::Unmatched => rule_line.push(muted("none")),
        Verdict::Ignore(rule) | Verdict::Keep(rule) => {
            rule_line.extend(rule_spans(rule, &base));
            if matches!(verdict, Verdict::Keep(_)) {
                rule_line.push(muted(" (keep)"));
            }
        }
    }
    lines.push(Line::from(rule_line));

    if let Ok(meta) = std::fs::symlink_metadata(&node.path)
        && meta.is_file()
    {
        lines.push(Line::from(vec![
            key("Size"),
            Span::raw(ui::human_bytes(meta.len())),
        ]));
    }

    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Actions",
        Style::new().fg(MUTED).bold(),
    )));
    let is_root = node.path == app.root.path;
    let mut actions: Vec<(&str, &str)> = Vec::new();
    if expandable(&node) && !is_root {
        actions.push(("→", "open folder"));
    }
    if !is_root {
        match &node.state {
            State::Tracked(_) => {
                actions.push(("i", "ignore now"));
                actions.push(("a", "add rule + ignore"));
                if node.kind.is_dir() {
                    actions.push(("x", "exclude (online only)"));
                }
            }
            State::Excluded => actions.push(("x", "download again")),
            State::Pending(_) => {
                actions.push(("c", "apply rules (check)"));
                actions.push(("i", "ignore just this"));
            }
            State::Ignored(Some(_)) => actions.push(("u", "unignore (until next check)")),
            State::Ignored(None) => {
                actions.push(("u", "unignore"));
                actions.push(("a", "add a rule for it"));
            }
            _ => {}
        }
    }
    if !matches!(
        node.state,
        State::Ignored(_) | State::InsideIgnored(_) | State::Outside
    ) {
        actions.push(("o", "open on dropbox.com"));
    }
    actions.push(("e", "edit .dropboxignore"));
    for (k, text) in actions {
        lines.push(Line::from(vec![
            Span::styled(format!(" {k:<2} "), Style::new().fg(ACCENT).bold()),
            Span::raw(text),
        ]));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(query) = &app.search {
        Line::from(vec![
            Span::styled(" / ", Style::new().fg(ACCENT).bold()),
            Span::raw(query.clone()),
            Span::styled(" ", Style::new().bg(Color::Gray)),
            muted("   enter keep · esc cancel"),
        ])
    } else if let Some(flash) = &app.flash {
        Line::from(vec![
            Span::raw(" "),
            glyph(flash.mark),
            Span::styled(format!(" {}", flash.text), color(flash.mark)),
        ])
    } else {
        let hints = [
            ("↑↓", "move"),
            ("←→", "fold"),
            ("i", "ignore"),
            ("u", "unignore"),
            ("a", "add rule"),
            ("x", "exclude"),
            ("c", "check"),
            ("e", "edit"),
            ("/", "find"),
            ("?", "help"),
            ("q", "quit"),
        ];
        let mut spans = vec![Span::raw(" ")];
        for (k, text) in hints {
            spans.push(Span::styled(k, Style::new().fg(ACCENT).bold()));
            spans.push(muted(format!(" {text}   ")));
        }
        Line::from(spans)
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn modal_area(frame: &mut Frame, width: u16, height: u16) -> Rect {
    let area = frame.area();
    let rect = area.centered(
        Constraint::Length(width.min(area.width.saturating_sub(2))),
        Constraint::Length(height.min(area.height.saturating_sub(2))),
    );
    frame.render_widget(Clear, rect);
    rect
}

fn modal_block(title: &str) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(format!(" {title} ")).bold().centered())
}

fn key_hint(keys: &[(&str, &str)]) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, text) in keys {
        spans.push(Span::styled(k.to_string(), Style::new().fg(ACCENT).bold()));
        spans.push(muted(format!(" {text}   ")));
    }
    Line::from(spans).centered()
}

fn draw_help(frame: &mut Frame) {
    let sections: [(&str, &[(&str, &str)]); 3] = [
        (
            "Navigate",
            &[
                ("↑ ↓  j k", "move"),
                ("→ l enter", "open folder"),
                ("← h", "close folder / go to parent"),
                ("space", "toggle folder"),
                ("H", "close all folders"),
                ("g G", "first / last"),
                ("PgUp PgDn  ^u ^d", "page"),
                ("/  n N", "find, next, previous"),
                ("mouse", "click, double-click, scroll"),
            ],
        ),
        (
            "Change",
            &[
                ("i", "ignore selected now"),
                ("u", "unignore selected"),
                ("a", "add rule for selected + ignore"),
                ("x", "exclude folder (online only) / restore"),
                ("e", "edit the folder's .dropboxignore"),
                ("c", "check: preview, then apply all rules"),
                ("o", "open selected on dropbox.com"),
            ],
        ),
        (
            "View",
            &[
                (".", "show / hide dotfiles"),
                ("D", "show / hide details"),
                ("r", "refresh from disk"),
                ("q esc", "quit"),
            ],
        ),
    ];

    let section = |title: &'static str, keys: &[(&'static str, &'static str)], width: usize| {
        let mut lines = vec![Line::from(Span::styled(
            title,
            Style::new().fg(MUTED).bold(),
        ))];
        for (k, text) in keys {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<width$}"), Style::new().fg(ACCENT).bold()),
                Span::raw(*text),
            ]));
        }
        lines.push(Line::default());
        lines
    };
    let [(nav_title, nav), (change_title, change), (view_title, view)] = sections;

    let mut left = section(nav_title, nav, 18);
    left.extend(section(view_title, view, 18));

    let mut right = section(change_title, change, 4);
    right.push(Line::from(Span::styled(
        "Legend",
        Style::new().fg(MUTED).bold(),
    )));
    for (mark, text) in [
        (Mark::Synced, "synced"),
        (Mark::Syncing, "syncing"),
        (Mark::Unsyncable, "can't sync"),
        (Mark::Excluded, "selective sync"),
        (Mark::Pending, "matches a rule, not ignored yet"),
        (Mark::Ignored, "ignored by rule"),
        (Mark::IgnoredManual, "ignored, no rule"),
    ] {
        right.push(Line::from(vec![
            Span::raw("  "),
            glyph(mark),
            Span::raw(format!(" {text}")),
        ]));
    }

    let height = left.len().max(right.len()) as u16;
    let area = modal_area(frame, 110, height + 4);
    let [content, hint] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
        .areas(area.inner(ratatui::layout::Margin::new(2, 1)));
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(content);
    frame.render_widget(modal_block("Keys"), area);
    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(Paragraph::new(right), right_area);
    frame.render_widget(Paragraph::new(key_hint(&[("esc", "close")])), hint);
}

fn draw_confirm(frame: &mut Frame, title: &str, lines: &[String]) {
    let width = lines.iter().map(|l| l.width()).max().unwrap_or(20).max(36) as u16 + 6;
    let area = modal_area(frame, width, lines.len() as u16 + 5);
    let [content, hint] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
        .areas(area.inner(ratatui::layout::Margin::new(2, 1)));
    frame.render_widget(modal_block(title), area);
    let text: Vec<Line> = lines.iter().map(|l| Line::raw(l.clone())).collect();
    frame.render_widget(Paragraph::new(text), content);
    frame.render_widget(
        Paragraph::new(key_hint(&[("y", "confirm"), ("n", "cancel")])),
        hint,
    );
}

fn draw_plan(frame: &mut Frame, app: &App) {
    let Some(Modal::Plan { plan, scroll }) = &app.modal else {
        return;
    };
    let base = &app.root.path;
    let area = modal_area(frame, 90, (plan.changes.len() as u16 + 7).clamp(9, 30));
    let [content, summary, hint] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(area.inner(ratatui::layout::Margin::new(2, 1)));
    frame.render_widget(modal_block("Apply .dropboxignore rules"), area);

    let display_name = |change: &crate::plan::Change| {
        let mut name = ui::pretty_path(&change.path, Some(base));
        if change.is_dir {
            name.push('/');
        }
        name
    };
    let column = plan
        .changes
        .iter()
        .map(|c| display_name(c).width())
        .max()
        .unwrap_or(0)
        .min(48);
    let lines: Vec<Line> = plan
        .changes
        .iter()
        .skip(*scroll)
        .map(|change| {
            let mark = change.action.mark();
            let kind = if change.is_dir { Kind::Dir } else { Kind::File };
            let name = display_name(change);
            let pad = column.saturating_sub(name.width()) + 2;
            let verb = match change.action {
                Action::Ignore => "ignore",
                Action::Unignore => "unignore",
                Action::Unmatched => "no rule",
            };
            let mut spans = vec![
                glyph(mark),
                Span::styled(format!(" {verb:<9}"), color(mark)),
                Span::styled(
                    name,
                    if kind.is_dir() {
                        Style::new().fg(Color::Blue).bold()
                    } else {
                        Style::new()
                    },
                ),
            ];
            if let Some(rule) = &change.rule {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.extend(rule_spans(rule, base));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), content);

    let ignore = plan.count(Action::Ignore);
    let unmatched = plan.count(Action::Unmatched);
    let mut text = vec![Span::styled(
        format!("{} to ignore", ui::plural(ignore, "item", "items")),
        color(Mark::Ignored),
    )];
    if ignore > 0 {
        text.push(muted(" · deleted from dropbox.com and other computers"));
    }
    if unmatched > 0 {
        text.push(muted(format!(
            " · {} ignored without a rule (left alone)",
            ui::thousands(unmatched)
        )));
    }
    frame.render_widget(
        Paragraph::new(vec![Line::default(), Line::from(text).centered()]),
        summary,
    );

    let keys: &[(&str, &str)] = if plan.is_noop() {
        &[("↑↓", "scroll"), ("esc", "close")]
    } else {
        &[("y", "apply"), ("↑↓", "scroll"), ("n", "cancel")]
    };
    frame.render_widget(Paragraph::new(key_hint(keys)), hint);
}
