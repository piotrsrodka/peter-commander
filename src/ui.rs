use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap};

use crate::app::{App, Dialog, SettingItem, Side};
use crate::menu::{FN_KEYS, MENU_BAR};
use crate::pane::Pane;
use crate::preview::{self, PreviewContent};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // menu bar
            Constraint::Min(3),    // panes
            Constraint::Length(1), // command line
            Constraint::Length(1), // status message
            Constraint::Length(1), // F-key bar
        ])
        .split(frame.area());

    draw_menu_bar(frame, root[0], app);

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[1]);

    app.pane_visible_lines = panes[0].height.saturating_sub(2) as usize;
    app.left_pane_area = panes[0];
    app.right_pane_area = panes[1];

    if app.quick_view {
        match app.active {
            Side::Left => {
                app.preview_visible_lines = panes[1].height.saturating_sub(2) as usize;
                app.preview_visible_width = panes[1].width.saturating_sub(2) as usize;
                draw_pane(
                    frame,
                    panes[0],
                    &app.left,
                    !app.preview_focus,
                    &mut app.left_list_state,
                );
                draw_preview_pane(frame, panes[1], &app.left, app.preview_focus, app.preview_scroll);
            }
            Side::Right => {
                app.preview_visible_lines = panes[0].height.saturating_sub(2) as usize;
                app.preview_visible_width = panes[0].width.saturating_sub(2) as usize;
                draw_preview_pane(frame, panes[0], &app.right, app.preview_focus, app.preview_scroll);
                draw_pane(
                    frame,
                    panes[1],
                    &app.right,
                    !app.preview_focus,
                    &mut app.right_list_state,
                );
            }
        }
    } else {
        draw_pane(
            frame,
            panes[0],
            &app.left,
            app.active == Side::Left,
            &mut app.left_list_state,
        );
        draw_pane(
            frame,
            panes[1],
            &app.right,
            app.active == Side::Right,
            &mut app.right_list_state,
        );
    }

    draw_command_line(frame, root[2], app);
    draw_status_message(frame, root[3], &app.status_message);
    draw_fn_key_bar(frame, root[4], app);

    if app.menu_open {
        draw_menu_dropdown(frame, root[0], app);
    }

    match &app.dialog {
        Dialog::Confirm { kind, name, .. } => {
            let hint = if kind.default_yes() { "[Y/n]" } else { "[y/N]" };
            draw_confirm_dialog(frame, &format!("{} '{}'? {}", kind.verb(), name, hint));
        }
        Dialog::TextInput { kind, input } => {
            draw_text_input_dialog(frame, kind.prompt(), input);
        }
        Dialog::Rename { input, .. } => {
            draw_text_input_dialog(frame, "Rename to:", input);
        }
        Dialog::ConfirmQuit => {
            draw_confirm_dialog(frame, "Quit PeterCommander? [Y/n]");
        }
        Dialog::Settings { selected, .. } => {
            draw_settings_dialog(frame, app, *selected);
        }
        Dialog::None => {}
    }

    if app.help_open {
        draw_help(frame, app.help_page);
    }
}

const HELP_PAGE_1: &[&str] = &[
    "F1        Help          Show this screen",
    "F2        Rename        Rename selection (move within same dir)",
    "F3        View          Page selected file with $PAGER",
    "F4        Edit          Edit selected file with $EDITOR",
    "Shift+F4  New File      Create a new empty file",
    "F5        Copy          Copy selection to the other pane",
    "F6        Move          Move selection to the other pane",
    "F7        MkDir         Create a new directory",
    "F8        Delete        Delete selection (asks to confirm)",
    "F9        Menu          Open the pulldown menu",
    "F10       Quit          Quit PeterCommander (asks to confirm)",
    "",
    "Alt+F1    Left = Right   Point left pane at right pane's dir",
    "Alt+F2    Right = Left   Point right pane at left pane's dir",
    "Alt+F/O/C Menu           Jump to the File/Options/Command menu",
    "Ctrl+O    Terminal       Reveal the terminal/scrollback under panels",
    "Ctrl+Q    Quit           Same as F10, in case your terminal eats F10",
    "",
    "Tab       Switch the active pane",
    "Up/Down   Move the selection",
    "Home/End  Jump to the top/bottom of the listing",
    "PgUp/PgDn Move the selection by one screenful",
];

const HELP_PAGE_2: &[&str] = &[
    "Type anywhere to fill the command line below the panes;",
    "Enter runs it in the active pane's directory, or opens",
    "the selected entry if the command line is empty.",
    "Esc clears the command line. Quit with F10 (or F9 > Command > Quit).",
    "",
    "F9 > Options > Settings opens the settings screen: Up/Down",
    "to move, Space to toggle a checkbox, Enter to save, Esc to cancel.",
    "",
    "Mouse: scroll the active pane/preview, click the menu bar or an",
    "F-key tile.",
];

const HELP_FOOTER: &str = "Left/Right: switch page   Any other key: close";

fn draw_help(frame: &mut Frame, page: usize) {
    let lines_for_page = if page == 0 { HELP_PAGE_1 } else { HELP_PAGE_2 };

    // Sized from both pages combined (not just the one currently shown),
    // so the window stays the same size when switching pages instead of
    // resizing around whichever page happens to be shorter.
    let width = HELP_PAGE_1
        .iter()
        .chain(HELP_PAGE_2)
        .chain([&HELP_FOOTER])
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(20) as u16
        + 4;
    let width = width.min(frame.area().width);
    let content_height = HELP_PAGE_1.len().max(HELP_PAGE_2.len()) as u16;
    let height = (content_height + 4).min(frame.area().height);

    let area = Rect {
        x: (frame.area().width.saturating_sub(width)) / 2,
        y: (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let mut lines: Vec<Line> = lines_for_page
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(Color::White))))
        .collect();
    // Pad the shorter page so the footer lands on the same row on both
    // pages, not just the box being the same overall size.
    lines.resize(content_height as usize, Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        HELP_FOOTER,
        Style::default().fg(Color::DarkGray),
    )));

    let block = Block::default()
        .title(format!("Help — Keybindings (Page {}/2)", page + 1))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn draw_text_input_dialog(frame: &mut Frame, prompt: &str, input: &str) {
    let width = (prompt.len().max(input.len() + 2) as u16 + 4).min(frame.area().width);
    let height = 4;
    let area = Rect {
        x: (frame.area().width.saturating_sub(width)) / 2,
        y: (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let block = Block::default()
        .title(prompt)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(input, Style::default().fg(Color::White)),
        Span::styled(
            "_",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]))
    .block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn draw_confirm_dialog(frame: &mut Frame, message: &str) {
    let width = (message.len() as u16 + 4).min(frame.area().width);
    let height = 3;
    let area = Rect {
        x: (frame.area().width.saturating_sub(width)) / 2,
        y: (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let paragraph = Paragraph::new(Line::from(Span::styled(
        message,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )))
    .block(block);

    draw_shadow_for(frame, area);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

const SETTINGS_HINT: &str = " Space: toggle   Enter: save   Esc: cancel";

fn draw_settings_dialog(frame: &mut Frame, app: &App, selected: usize) {
    let items: Vec<ListItem> = SettingItem::ALL
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let checkbox = if app.setting_value(*item) {
                "[x]"
            } else {
                "[ ]"
            };
            let style = if idx == selected {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {checkbox} {}", item.label()),
                style,
            )))
        })
        .chain(std::iter::once(ListItem::new(Line::from(Span::styled(
            SETTINGS_HINT,
            Style::default().fg(Color::DarkGray),
        )))))
        .collect();

    let hint_len = SETTINGS_HINT.chars().count();
    let width = SettingItem::ALL
        .iter()
        .map(|item| item.label().chars().count() + 6)
        .chain(std::iter::once(hint_len + 2))
        .max()
        .unwrap_or(20) as u16;
    let width = width.min(frame.area().width);
    let height = (SettingItem::ALL.len() as u16 + 3).min(frame.area().height);

    let area = Rect {
        x: (frame.area().width.saturating_sub(width)) / 2,
        y: (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let block = Block::default()
        .title("Settings")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items).block(block), area);
}

fn draw_menu_bar(frame: &mut Frame, area: Rect, app: &mut App) {
    app.menu_bar_tiles.clear();
    let mut x = area.x + 1; // the leading raw space
    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (idx, category) in MENU_BAR.iter().enumerate() {
        let is_selected = app.menu_open && idx == app.menu_category;
        let style = if is_selected {
            Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Black).bg(Color::Gray)
        };
        let tile_width = category.title.len() as u16 + 2;
        app.menu_bar_tiles.push((
            Rect {
                x,
                y: area.y,
                width: tile_width,
                height: 1,
            },
            idx,
        ));
        x += tile_width;
        spans.push(Span::styled(format!(" {} ", category.title), style));
    }
    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Gray));
    frame.render_widget(bar, area);
}

/// A drop-shadow overlay. Two failed attempts taught what doesn't work
/// here: `Modifier::DIM` only dims a cell's foreground per the ANSI spec,
/// not its background, and most of the shadow falls on blank/background
/// cells; and named colors like `Color::DarkGray` are whatever the
/// terminal's color scheme remaps them to (some dark themes map it close
/// to black, same trap as the old hardcoded `Color::White` text bug). A
/// fixed `Color::Rgb` sidesteps theme remapping entirely — this exact gray
/// renders the same regardless of the terminal's palette.
struct Shadow;

impl Widget for Shadow {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(Color::Rgb(90, 90, 90));
                    cell.modifier.insert(Modifier::DIM);
                }
            }
        }
    }
}

/// Draws a `Shadow` offset from `area` — call this before clearing/drawing
/// into `area` itself, so only the shadow's bottom/right sliver ends up
/// visible once the real content is drawn on top of it. The right side is
/// offset by 2 columns rather than 1 (matching classic Norton Commander):
/// terminal cells are roughly twice as tall as they are wide, so a 1-row
/// bottom shadow needs 2 columns on the right to look proportional instead
/// of lopsided.
fn draw_shadow_for(frame: &mut Frame, area: Rect) {
    let shadow_area = Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width + 1,
        height: area.height,
    };
    frame.render_widget(Shadow, shadow_area);
}

fn draw_menu_dropdown(frame: &mut Frame, menu_bar_area: Rect, app: &mut App) {
    // Compute the x offset of the selected category so the dropdown appears under it.
    let mut x = menu_bar_area.x + 1;
    for category in &MENU_BAR[..app.menu_category] {
        x += category.title.len() as u16 + 2;
    }

    let category = &MENU_BAR[app.menu_category];
    let row_text_width = category
        .items
        .iter()
        .map(|action| {
            let shortcut_width = action.shortcut().map_or(0, |s| s.len() + 3);
            action.label().len() + shortcut_width
        })
        .max()
        .unwrap_or(4);
    let width = (row_text_width as u16) + 4;
    let height = category.items.len() as u16 + 2;

    let area = Rect {
        x: x.min(frame.area().width.saturating_sub(width)),
        y: menu_bar_area.y + 1,
        width,
        height,
    };

    // Item rows sit 1 cell in from the block's border on every side.
    app.menu_item_tiles = (0..category.items.len())
        .map(|idx| {
            (
                Rect {
                    x: area.x + 1,
                    y: area.y + 1 + idx as u16,
                    width: area.width.saturating_sub(2),
                    height: 1,
                },
                idx,
            )
        })
        .collect();

    let items: Vec<ListItem> = category
        .items
        .iter()
        .enumerate()
        .map(|(idx, action)| {
            let is_selected = idx == app.menu_item;
            let enabled = app.action_enabled(*action);
            let style = match (is_selected, enabled) {
                (true, true) => Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                (true, false) => Style::default().fg(Color::DarkGray).bg(Color::Blue),
                (false, true) => Style::default().fg(Color::Black).bg(Color::Gray),
                (false, false) => Style::default().fg(Color::DarkGray).bg(Color::Gray),
            };
            let text = match action.shortcut() {
                Some(shortcut) => {
                    format!(
                        "{:<label_width$}{shortcut:>shortcut_width$}",
                        action.label(),
                        label_width = row_text_width - shortcut.len(),
                        shortcut_width = shortcut.len(),
                    )
                }
                None => format!("{:<row_text_width$}", action.label()),
            };
            ListItem::new(Line::from(Span::styled(format!(" {text} "), style)))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Gray).fg(Color::Black));

    draw_shadow_for(frame, area);

    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items).block(block), area);
}

/// Width of a formatted date/time like "20-09-26 15:57".
const DATE_COLUMN_WIDTH: usize = 14;
/// Width the size column is right-aligned to.
const SIZE_COLUMN_WIDTH: usize = 10;
/// Above this, the size column shows megabytes instead of a raw byte count
/// — a precise byte count stops being useful reading once it's this large.
const SIZE_COLLAPSE_THRESHOLD: u64 = 100 * 1024 * 1024;

/// Inserts a comma every 3 digits, e.g. `1234567` -> `"1,234,567"`.
fn with_thousands_separators(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, &byte) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(byte as char);
    }
    result
}

/// Formats a file size for the listing: a comma-grouped byte count below
/// `SIZE_COLLAPSE_THRESHOLD`, or a comma-grouped megabyte count above it —
/// an exact byte count stops being readable/useful once a file is that big.
fn format_size(bytes: u64) -> String {
    if bytes >= SIZE_COLLAPSE_THRESHOLD {
        format!("{} MB", with_thousands_separators(bytes / (1024 * 1024)))
    } else {
        with_thousands_separators(bytes)
    }
}
/// Never shrink the name column below this, even on a very narrow pane —
/// beyond this point there just isn't a sane layout, so let the line clip
/// instead of producing a useless sliver of a name column.
const MIN_NAME_COLUMN_WIDTH: usize = 8;

/// How wide the name column should be so the full row (name + size + date)
/// exactly fills `inner_width` (the pane's content width, borders already
/// excluded) — rather than a fixed width that wastes space on a wide pane
/// or overflows on a narrow one.
fn name_column_width(inner_width: u16) -> usize {
    // 3 spaces: one before the size column, one before the date, and one
    // trailing margin after it.
    let fixed_width = SIZE_COLUMN_WIDTH + DATE_COLUMN_WIDTH + 3;
    (inner_width as usize)
        .saturating_sub(fixed_width)
        .max(MIN_NAME_COLUMN_WIDTH)
}

/// Truncates a name to fit the name column, appending an ellipsis, instead
/// of letting a long name spill into the size/date columns.
fn fit_name(name: &str, width: usize) -> String {
    let char_count = name.chars().count();
    if char_count <= width {
        format!("{name:<width$}")
    } else {
        let truncated: String = name.chars().take(width.saturating_sub(1)).collect();
        format!("{truncated}\u{2026}")
    }
}

fn draw_command_line(frame: &mut Frame, area: Rect, app: &App) {
    // Indented 1 column, matching the F-key bar below it.
    let area = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };
    let cwd = match app.active {
        Side::Left => &app.left.cwd,
        Side::Right => &app.right.cwd,
    };
    let line = Line::from(vec![
        Span::styled(
            format!("{}> ", cwd.display()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(&app.command_line, Style::default().fg(Color::Reset)),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn format_modified(modified: Option<std::time::SystemTime>) -> String {
    match modified {
        Some(time) => DateTime::<Local>::from(time)
            .format("%d-%m-%y %H:%M")
            .to_string(),
        None => String::new(),
    }
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &Pane,
    is_active: bool,
    list_state: &mut ListState,
) {
    let border_style = if is_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = match pane.selected_entry() {
        Some(entry) => pane.cwd.join(&entry.name).to_string_lossy().to_string(),
        None => pane.cwd.to_string_lossy().to_string(),
    };

    // -2 for the pane's own left/right border.
    let name_width = name_column_width(area.width.saturating_sub(2));

    let items: Vec<ListItem> = pane
        .entries
        .iter()
        .map(|entry| {
            let style = if entry.is_dir {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else if entry.is_executable {
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            } else {
                // Not White: this renders straight onto the terminal's own
                // background with no contrasting box behind it, so it must
                // follow the terminal's default foreground instead of
                // assuming a dark theme (a hardcoded white was invisible on
                // light-background terminals).
                Style::default().fg(Color::Reset)
            };
            let date = format_modified(entry.modified);
            let size_label = if entry.is_dir {
                String::new()
            } else {
                format_size(entry.size)
            };
            let name = fit_name(&entry.name, name_width);
            let label = format!("{name} {size_label:>SIZE_COLUMN_WIDTH$} {date} ");
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let highlight_style = if is_active {
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let list = List::new(items).block(block).highlight_style(highlight_style);

    // Always keep the selection tracked (not just while active) so the
    // persisted `list_state`'s scroll offset stays correct for this pane
    // even while the other pane has focus; `highlight_style` above is what
    // actually hides the highlight when inactive.
    list_state.select(Some(pane.selected));

    frame.render_stateful_widget(list, area, list_state);
}

/// Quick-view: renders a live preview of `source`'s selected entry, in
/// place of the opposite pane's own listing (Ctrl+Q). `focused` is whether
/// Tab has moved scroll focus onto this pane; `scroll` is how many lines of
/// a text preview are skipped from the top.
fn draw_preview_pane(frame: &mut Frame, area: Rect, source: &Pane, focused: bool, scroll: usize) {
    let title = match source.selected_entry() {
        Some(entry) if entry.name != ".." => {
            source.cwd.join(&entry.name).to_string_lossy().to_string()
        }
        _ => source.cwd.to_string_lossy().to_string(),
    };

    let border_style = if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(format!("Preview: {title}"))
        .borders(Borders::ALL)
        .border_style(border_style);

    // For everything except Text, the scroll offset is applied by
    // skipping whole (never-wrapped) entries up front. Text is different:
    // it's word-wrapped, so a scroll offset in *logical* lines would be
    // wrong once a long line spans multiple visual rows — instead all
    // lines are kept and `paragraph_scroll` is handed to Ratatui's own
    // `.scroll()`, which operates in post-wrap visual rows.
    let mut paragraph_scroll: u16 = 0;
    let lines: Vec<Line> = match preview::build_preview(source) {
        PreviewContent::Empty => Vec::new(),
        PreviewContent::Directory(entries) => entries
            .into_iter()
            .skip(scroll)
            .map(|entry| {
                let style = if entry.is_dir {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Reset)
                };
                Line::from(Span::styled(entry.name, style))
            })
            .collect(),
        PreviewContent::Binary(size) => vec![Line::from(Span::styled(
            format!("<Binary file> ({size} bytes)"),
            Style::default().fg(Color::DarkGray),
        ))],
        PreviewContent::Error(err) => vec![Line::from(Span::styled(
            format!("Cannot preview: {err}"),
            Style::default().fg(Color::Red),
        ))],
        PreviewContent::Text(text_lines) => {
            paragraph_scroll = scroll as u16;
            text_lines.into_iter().map(Line::from).collect()
        }
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((paragraph_scroll, 0));
    frame.render_widget(paragraph, area);
}

fn draw_status_message(frame: &mut Frame, area: Rect, message: &str) {
    let paragraph = Paragraph::new(Line::from(Span::styled(
        format!(" {}", message),
        Style::default().fg(Color::Yellow),
    )));
    frame.render_widget(paragraph, area);
}

fn draw_fn_key_bar(frame: &mut Frame, area: Rect, app: &mut App) {
    // Indented by 1 column to match Norton Commander's look, rather than
    // starting flush against the screen edge.
    let area = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(1),
        ..area
    };

    // Spread the tiles evenly across the full width instead of packing them
    // to the left; on a narrow terminal the tail simply gets clipped, same
    // as before. A 1-column gap between tiles (not before F1) needs
    // reserving len-1 columns up front.
    let gaps = FN_KEYS.len() as u16 - 1;
    let usable = area.width.saturating_sub(gaps);
    let base_width = usable / FN_KEYS.len() as u16;
    let remainder = (usable % FN_KEYS.len() as u16) as usize;

    // Integer division always leaves 0-9 leftover columns; rather than
    // stranding them as blank padding after the last tile, hand one extra
    // column each to the `remainder` tiles with the longest label text —
    // those already have the least slack, so widening them first keeps the
    // row looking evenly balanced instead of the growth being arbitrary.
    let mut widen_first: Vec<usize> = (0..FN_KEYS.len()).collect();
    widen_first.sort_by_key(|&i| std::cmp::Reverse(FN_KEYS[i].label.len()));
    let mut tile_widths = vec![base_width; FN_KEYS.len()];
    for &i in widen_first.iter().take(remainder) {
        tile_widths[i] += 1;
    }

    app.fn_key_tiles.clear();
    let mut x = area.x;
    let mut spans = Vec::new();
    for (idx, fn_key) in FN_KEYS.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
            x += 1;
        }
        app.fn_key_tiles.push((
            Rect {
                x,
                y: area.y,
                width: tile_widths[idx],
                height: 1,
            },
            fn_key.action,
        ));
        x += tile_widths[idx];

        spans.push(Span::styled(
            fn_key.key,
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
        let label_width = tile_widths[idx].saturating_sub(fn_key.key.len() as u16) as usize;
        spans.push(Span::styled(
            format!("{:<label_width$}", fn_key.label),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ));
    }
    let bar = Paragraph::new(Line::from(spans));
    frame.render_widget(bar, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_separators_are_inserted_every_3_digits() {
        assert_eq!(with_thousands_separators(0), "0");
        assert_eq!(with_thousands_separators(42), "42");
        assert_eq!(with_thousands_separators(999), "999");
        assert_eq!(with_thousands_separators(1000), "1,000");
        assert_eq!(with_thousands_separators(1_234_567), "1,234,567");
    }

    #[test]
    fn size_below_threshold_shows_a_comma_grouped_byte_count() {
        assert_eq!(format_size(0), "0");
        assert_eq!(format_size(1_234_567), "1,234,567");
        assert_eq!(format_size(SIZE_COLLAPSE_THRESHOLD - 1), "104,857,599");
    }

    #[test]
    fn size_at_or_above_threshold_collapses_to_megabytes() {
        assert_eq!(format_size(SIZE_COLLAPSE_THRESHOLD), "100 MB");
        assert_eq!(format_size(250 * 1024 * 1024), "250 MB");
        assert_eq!(format_size(2_000 * 1024 * 1024), "2,000 MB");
    }

    #[test]
    fn short_name_is_padded_not_truncated() {
        let result = fit_name("short.txt", 10);
        assert_eq!(result, "short.txt ");
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn long_name_is_truncated_with_ellipsis() {
        let result = fit_name("this_is_a_very_long_filename.txt", 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('\u{2026}'));
        assert!(result.starts_with("this_is_a"));
    }

    #[test]
    fn menu_dropdown_shadow_survives_to_final_buffer() {
        use crate::app::App;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new().unwrap();
        app.open_menu();

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buf = terminal.backend().buffer();

        // Recompute the same area/shadow_area the real code computes, to
        // find a cell that should be shadow-only (outside the dropdown's
        // own area, inside shadow_area).
        let category = &MENU_BAR[0];
        let row_text_width = category
            .items
            .iter()
            .map(|action| {
                let shortcut_width = action.shortcut().map_or(0, |s| s.len() + 3);
                action.label().len() + shortcut_width
            })
            .max()
            .unwrap_or(4);
        let width = (row_text_width as u16) + 4;
        let height = category.items.len() as u16 + 2;
        let area = Rect {
            x: 1,
            y: 1,
            width,
            height,
        };
        let shadow_only_x = area.x + area.width; // one past the dropdown's right edge
        let shadow_only_y = area.y + 1;

        let cell = &buf[(shadow_only_x, shadow_only_y)];
        assert_eq!(cell.bg, Color::Rgb(90, 90, 90));
    }

    #[test]
    fn exact_width_name_is_unchanged() {
        let result = fit_name("1234567890", 10);
        assert_eq!(result, "1234567890");
    }
}
