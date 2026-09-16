use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

use crate::app::{App, Dialog, Side};
use crate::menu::{FN_KEYS, MENU_BAR};
use crate::pane::Pane;

pub fn draw(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // menu bar
            Constraint::Min(3),    // panes
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

    draw_status_message(frame, root[2], &app.status_message);
    draw_fn_key_bar(frame, root[3]);

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
        Dialog::None => {}
    }
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
    let width = category
        .items
        .iter()
        .map(|a| a.label().len())
        .max()
        .unwrap_or(4) as u16
        + 4;
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
            let style = if idx == app.menu_item {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Black).bg(Color::Gray)
            };
            ListItem::new(Line::from(Span::styled(
                format!(" {} ", action.label()),
                style,
            )))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(Color::Gray).fg(Color::Black));

    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items).block(block), area);
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

    let title = pane.cwd.to_string_lossy().to_string();

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
                "<DIR>".to_string()
            } else {
                entry.size.to_string()
            };
            let label = format!("{:<30} {:>10} {}", entry.name, size_label, date);
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    state.select(Some(pane.selected));

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
