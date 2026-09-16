use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::app::{App, Side};
use crate::pane::Pane;

pub fn draw(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(frame.area());

    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[0]);

    draw_pane(frame, panes[0], &app.left, app.active == Side::Left);
    draw_pane(frame, panes[1], &app.right, app.active == Side::Right);
    draw_status_bar(frame, root[1]);
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
            let label = if entry.is_dir {
                format!("{}/", entry.name)
            } else {
                format!("{:<30} {:>10}", entry.name, entry.size)
            };
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

fn draw_status_bar(frame: &mut Frame, area: Rect) {
    let help = Paragraph::new(Line::from(vec![Span::styled(
        " Tab: switch pane  ↑/↓: move  Enter: open  q: quit ",
        Style::default().fg(Color::Black).bg(Color::Gray),
    )]));
    frame.render_widget(help, area);
}
