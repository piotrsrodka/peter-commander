use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, Dialog, SettingItem, Side};
use crate::menu::{FN_KEYS, MENU_BAR};
use crate::pane::Pane;

pub fn draw(frame: &mut Frame, app: &App) {
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

    draw_pane(frame, panes[0], &app.left, app.active == Side::Left);
    draw_pane(frame, panes[1], &app.right, app.active == Side::Right);

    draw_command_line(frame, root[2], app);
    draw_status_message(frame, root[3], &app.status_message);
    draw_fn_key_bar(frame, root[4]);

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
        draw_help(frame);
    }
}

const HELP_LINES: &[&str] = &[
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
    "Ctrl+O    Terminal       Reveal the terminal/scrollback under panels",
    "",
    "Tab       Switch the active pane",
    "Up/Down   Move the selection",
    "",
    "Type anywhere to fill the command line below the panes;",
    "Enter runs it in the active pane's directory, or opens",
    "the selected entry if the command line is empty.",
    "Esc clears the command line. Quit with F10 (or F9 > Command > Quit).",
    "",
    "F9 > Options > Settings opens the settings screen: Up/Down",
    "to move, Space to toggle a checkbox, Enter to save, Esc to cancel.",
    "",
    "Press any key to close this help",
];

fn draw_help(frame: &mut Frame) {
    let width = HELP_LINES
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(20) as u16
        + 4;
    let width = width.min(frame.area().width);
    let height = (HELP_LINES.len() as u16 + 2).min(frame.area().height);

    let area = Rect {
        x: (frame.area().width.saturating_sub(width)) / 2,
        y: (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let lines: Vec<Line> = HELP_LINES
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(Color::White))))
        .collect();

    let block = Block::default()
        .title("Help — Keybindings")
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

fn draw_menu_bar(frame: &mut Frame, area: Rect, app: &App) {
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
        spans.push(Span::styled(format!(" {} ", category.title), style));
    }
    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Gray));
    frame.render_widget(bar, area);
}

fn draw_menu_dropdown(frame: &mut Frame, menu_bar_area: Rect, app: &App) {
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

    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items).block(block), area);
}

const NAME_COLUMN_WIDTH: usize = 30;

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
        Span::styled(&app.command_line, Style::default().fg(Color::White)),
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

fn draw_pane(frame: &mut Frame, area: Rect, pane: &Pane, is_active: bool) {
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

    let items: Vec<ListItem> = pane
        .entries
        .iter()
        .map(|entry| {
            let style = if entry.is_dir {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let date = format_modified(entry.modified);
            let size_label = if entry.is_dir {
                String::new()
            } else {
                entry.size.to_string()
            };
            let name = fit_name(&entry.name, NAME_COLUMN_WIDTH);
            let label = format!("{name} {size_label:>10} {date}");
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

    let mut state = ListState::default();
    if is_active {
        state.select(Some(pane.selected));
    }

    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_status_message(frame: &mut Frame, area: Rect, message: &str) {
    let paragraph = Paragraph::new(Line::from(Span::styled(
        format!(" {}", message),
        Style::default().fg(Color::Yellow),
    )));
    frame.render_widget(paragraph, area);
}

fn draw_fn_key_bar(frame: &mut Frame, area: Rect) {
    // Spread the tiles evenly across the full width instead of packing them
    // to the left; on a narrow terminal the tail simply gets clipped, same
    // as before.
    let tile_width = area.width / FN_KEYS.len() as u16;

    let mut spans = Vec::new();
    for fn_key in FN_KEYS {
        spans.push(Span::styled(
            fn_key.key,
            Style::default()
                .fg(Color::Yellow)
                .bg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
        let label_width = tile_width.saturating_sub(fn_key.key.len() as u16) as usize;
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
    fn exact_width_name_is_unchanged() {
        let result = fit_name("1234567890", 10);
        assert_eq!(result, "1234567890");
    }
}
