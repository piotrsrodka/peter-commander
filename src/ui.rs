use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph, Widget, Wrap,
};

use crate::app::{App, Dialog, DialogKind, SettingItem, Side};
use crate::logging;
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
                    app.classic_style,
                );
                draw_preview_pane(
                    frame,
                    panes[1],
                    &app.left,
                    app.preview_focus,
                    app.preview_scroll,
                    app.classic_style,
                );
            }
            Side::Right => {
                app.preview_visible_lines = panes[0].height.saturating_sub(2) as usize;
                app.preview_visible_width = panes[0].width.saturating_sub(2) as usize;
                draw_preview_pane(
                    frame,
                    panes[0],
                    &app.right,
                    app.preview_focus,
                    app.preview_scroll,
                    app.classic_style,
                );
                draw_pane(
                    frame,
                    panes[1],
                    &app.right,
                    !app.preview_focus,
                    &mut app.right_list_state,
                    app.classic_style,
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
            app.classic_style,
        );
        draw_pane(
            frame,
            panes[1],
            &app.right,
            app.active == Side::Right,
            &mut app.right_list_state,
            app.classic_style,
        );
    }

    draw_command_line(frame, root[2], app);
    draw_fn_key_bar(frame, root[3], app);

    if app.menu_open {
        draw_menu_dropdown(frame, root[0], app);
    }

    match &app.dialog {
        Dialog::Confirm { kind, name, .. } => {
            let destination = matches!(kind, DialogKind::Copy | DialogKind::Move)
                .then(|| app.inactive_pane_ref().cwd.display().to_string());
            draw_confirm_dialog(
                frame,
                app.classic_style,
                *kind,
                name,
                destination.as_deref(),
                app.dialog_cancel_focused,
            );
        }
        Dialog::TextInput { kind, input } => {
            draw_input_dialog(
                frame,
                app.classic_style,
                kind.title(),
                kind.prompt(),
                input,
                app.dialog_cancel_focused,
            );
        }
        Dialog::Rename { input, src } => {
            let old_name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            draw_input_dialog(
                frame,
                app.classic_style,
                "Rename",
                &format!("Rename \"{old_name}\" to:"),
                input,
                app.dialog_cancel_focused,
            );
        }
        Dialog::ConfirmQuit => {
            draw_quit_dialog(frame, app.classic_style, app.dialog_cancel_focused);
        }
        Dialog::Settings { selected, .. } => {
            draw_settings_dialog(frame, app, *selected);
        }
        Dialog::None => {}
    }

    if app.help_open {
        draw_help(frame, app.help_page);
    }

    if app.logs_open {
        draw_logs(frame);
    }

    if let Some(message) = &app.error_dialog {
        draw_error_dialog(frame, message);
    }
}

/// A blocking "in your face" popup for `App::set_error` — unlike a routine
/// confirmation, an error demands a keypress to dismiss (any key, handled in
/// `main.rs`) so it can't be missed the way the log-only status line can.
fn draw_error_dialog(frame: &mut Frame, message: &str) {
    let area = frame.area();
    let inner_width = area.width.saturating_sub(4).clamp(20, 64) as usize;
    let hint = "Press any key to continue";
    let wrapped = wrap_message(message, inner_width);
    let content_width = wrapped
        .iter()
        .map(|line| line.len())
        .max()
        .unwrap_or(0)
        .max(hint.len());
    let width = (content_width as u16 + 4).min(area.width);
    let height = wrapped.len() as u16 + 4;
    let dialog_area = Rect {
        x: (area.width.saturating_sub(width)) / 2,
        y: (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let block = Block::default()
        .title(" Error ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    let mut lines: Vec<Line> = wrapped
        .into_iter()
        .map(|line| {
            Line::from(Span::styled(
                line,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).block(block);

    draw_shadow_for(frame, dialog_area);
    frame.render_widget(Clear, dialog_area);
    frame.render_widget(paragraph, dialog_area);
}

/// Greedy word-wrap to at most `width` columns per line — good enough for
/// short error messages, no need for the full richness of a text-shaping
/// crate here.
fn wrap_message(message: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in message.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
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
    "F9 > Command > Show Logs shows recent status/error messages —",
    "there's no dedicated status line any more, they're logged instead.",
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

/// Color palette for the big double-bordered dialog frame shared by every
/// modal (Copy/Move/Delete confirm, Rename, MkDir/New file, Quit). The
/// *shape* — double border, sectioned content, a `[ Button ]` row — is the
/// same in both modes; only the colors swap: `classic_style` uses the fixed
/// retro gray/teal DOS palette, otherwise everything follows the terminal's
/// own theme colors.
struct DialogPalette {
    bg: Color,
    fg: Color,
    border_fg: Color,
    field_bg: Color,
    field_fg: Color,
    button_default_bg: Color,
    button_default_fg: Color,
}

fn dialog_palette(classic_style: bool) -> DialogPalette {
    if classic_style {
        DialogPalette {
            bg: classic::DIALOG_BG,
            fg: classic::DIALOG_FG,
            border_fg: classic::DIALOG_BORDER_FG,
            field_bg: classic::HIGHLIGHT_BG,
            field_fg: classic::HIGHLIGHT_FG,
            button_default_bg: classic::HIGHLIGHT_BG,
            button_default_fg: classic::HIGHLIGHT_FG,
        }
    } else {
        DialogPalette {
            bg: Color::Black,
            fg: Color::White,
            border_fg: Color::Cyan,
            field_bg: Color::Cyan,
            field_fg: Color::Black,
            button_default_bg: Color::Cyan,
            button_default_fg: Color::Black,
        }
    }
}

/// One line of dialog content. `Field` is padded to the dialog's full inner
/// width when rendered so its background fills the whole row, like the
/// highlighted destination/input bar in classic Norton Commander dialogs;
/// `Text`/`Centered` are rendered as-is (`Centered` for the button row).
enum DialogLine<'a> {
    Text(Line<'a>),
    Field { content: Vec<Span<'a>>, fill_bg: Color },
    Centered(Line<'a>),
}

impl DialogLine<'_> {
    fn natural_width(&self) -> usize {
        match self {
            DialogLine::Text(line) | DialogLine::Centered(line) => line.width(),
            DialogLine::Field { content, .. } => {
                content.iter().map(Span::width).sum::<usize>() + 2 * DIALOG_PAD
            }
        }
    }
}

fn button_row<'a>(buttons: &[(&'a str, bool)], pal: &DialogPalette) -> DialogLine<'a> {
    let mut spans = Vec::new();
    for (idx, (label, is_default)) in buttons.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("   "));
        }
        let style = if *is_default {
            Style::default()
                .bg(pal.button_default_bg)
                .fg(pal.button_default_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(pal.fg)
        };
        spans.push(Span::styled(format!("[ {label} ]"), style));
    }
    DialogLine::Centered(Line::from(spans))
}

/// Renders `sections` stacked inside a double-bordered box, each section
/// separated by a full-width single-line divider that tees into the border
/// (`╟──────╢`) — the "big, proud" classic dialog look the whole family of
/// modals shares.
/// 1-column margin of dialog background kept between the border and the
/// content on every side (except the divider rows, which deliberately span
/// edge-to-edge to tee into the border).
const DIALOG_PAD: usize = 1;

/// A second margin of dialog background *outside* the border too, between
/// it and whatever's behind the dialog — so the border never sits flush
/// against the panes/shadow. Wider on the sides than top/bottom, matching
/// the reference look.
const OUTER_PAD_X: u16 = 2;
const OUTER_PAD_Y: u16 = 1;

fn draw_dialog_frame(
    frame: &mut Frame,
    classic_style: bool,
    title: &str,
    sections: Vec<Vec<DialogLine>>,
    min_half_screen: bool,
) {
    let pal = dialog_palette(classic_style);

    let content_width = sections
        .iter()
        .flat_map(|section| section.iter())
        .map(DialogLine::natural_width)
        .max()
        .unwrap_or(0)
        .max(title.chars().count() + 4);
    let box_width = content_width as u16 + 2 * DIALOG_PAD as u16 + 2;
    // Never narrower than half the screen, however short the content is —
    // applied to the full outer footprint, border pad included. Quit opts
    // out, staying sized to its (short) content instead.
    let min_width = if min_half_screen {
        frame.area().width / 2
    } else {
        0
    };
    let total_width = (box_width + 2 * OUTER_PAD_X)
        .max(min_width)
        .min(frame.area().width);
    let width = total_width.saturating_sub(2 * OUTER_PAD_X);
    let inner_width = width.saturating_sub(2) as usize;

    // Block's own left/right border glyph is drawn independently for every
    // row, including this one, so the divider's content can't include `╠`/
    // `╣` itself (that would double up with the border's `║`) — it's plain
    // `═` here, and the two edge cells get patched to `╠`/`╣` after the
    // paragraph is rendered, to actually tee into the border.
    let divider = Line::from(Span::styled(
        "─".repeat(inner_width),
        Style::default().fg(pal.border_fg),
    ));

    let divider_count = sections.len().saturating_sub(1);
    let content_height: usize = sections.iter().map(Vec::len).sum();
    let box_height = (content_height + divider_count + 2) as u16;
    let total_height = (box_height + 2 * OUTER_PAD_Y).min(frame.area().height);
    let height = total_height.saturating_sub(2 * OUTER_PAD_Y);

    let outer_area = Rect {
        x: (frame.area().width.saturating_sub(total_width)) / 2,
        y: (frame.area().height.saturating_sub(total_height)) / 2,
        width: total_width,
        height: total_height,
    };
    let area = Rect {
        x: outer_area.x + OUTER_PAD_X,
        y: outer_area.y + OUTER_PAD_Y,
        width,
        height,
    };

    let block = Block::default()
        .title_top(Line::from(format!(" {title} ")).centered())
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(pal.border_fg))
        .style(Style::default().bg(pal.bg).fg(pal.fg));

    let mut lines: Vec<Line> = Vec::new();
    let mut divider_rows: Vec<u16> = Vec::new();
    for (idx, section) in sections.into_iter().enumerate() {
        if idx > 0 {
            divider_rows.push(lines.len() as u16);
            lines.push(divider.clone());
        }
        for dialog_line in section {
            lines.push(match dialog_line {
                DialogLine::Text(mut line) => {
                    line.spans.insert(
                        0,
                        Span::styled(" ".repeat(DIALOG_PAD), Style::default().bg(pal.bg)),
                    );
                    line
                }
                DialogLine::Centered(line) => line.centered(),
                DialogLine::Field { content, fill_bg } => {
                    let content_width: usize = content.iter().map(Span::width).sum();
                    let mut spans =
                        vec![Span::styled(" ".repeat(DIALOG_PAD), Style::default().bg(pal.bg))];
                    spans.extend(content);
                    let used = DIALOG_PAD + content_width;
                    let bar_end = inner_width.saturating_sub(DIALOG_PAD);
                    if bar_end > used {
                        spans.push(Span::styled(
                            " ".repeat(bar_end - used),
                            Style::default().bg(fill_bg),
                        ));
                    }
                    Line::from(spans)
                }
            });
        }
    }

    let paragraph = Paragraph::new(lines).block(block);

    draw_shadow_for(frame, outer_area);
    frame.render_widget(Clear, outer_area);
    frame.render_widget(
        Block::default().style(Style::default().bg(pal.bg)),
        outer_area,
    );
    frame.render_widget(paragraph, area);

    // Patch the border column on each divider row into a single-line-into-
    // double-border tee (`╟`/`╢`) now that the block has drawn its own `║`
    // there — content-row index `row` sits at `area.y + 1 + row` since the
    // top border consumes row 0.
    let buf = frame.buffer_mut();
    let border_style = Style::default().fg(pal.border_fg).bg(pal.bg);
    for row in divider_rows {
        let y = area.y + 1 + row;
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol("╟").set_style(border_style);
        }
        if let Some(cell) = buf.cell_mut((area.x + width.saturating_sub(1), y)) {
            cell.set_symbol("╢").set_style(border_style);
        }
    }
}

/// Copy/Move/Delete confirmation — Copy and Move also show the destination
/// (the other pane's directory, not editable) as a highlighted line.
fn draw_confirm_dialog(
    frame: &mut Frame,
    classic_style: bool,
    kind: DialogKind,
    name: &str,
    destination: Option<&str>,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let verb = kind.verb();

    let message = match destination {
        Some(_) => format!("{verb} \"{name}\" to"),
        None => format!("{verb} \"{name}\"?"),
    };
    let mut sections = vec![vec![DialogLine::Text(Line::from(Span::styled(
        message,
        Style::default().fg(pal.fg),
    )))]];

    if let Some(dest) = destination {
        sections.push(vec![DialogLine::Field {
            content: vec![Span::styled(
                dest.to_string(),
                Style::default().bg(pal.field_bg).fg(pal.field_fg),
            )],
            fill_bg: pal.field_bg,
        }]);
    }

    sections.push(vec![button_row(
        &[(verb, !cancel_focused), ("Cancel", cancel_focused)],
        &pal,
    )]);

    draw_dialog_frame(frame, classic_style, verb, sections, true);
}

fn draw_quit_dialog(frame: &mut Frame, classic_style: bool, cancel_focused: bool) {
    let pal = dialog_palette(classic_style);
    let sections = vec![
        vec![DialogLine::Text(Line::from(Span::styled(
            "Quit PeterCommander?",
            Style::default().fg(pal.fg),
        )))],
        vec![button_row(
            &[("Quit", !cancel_focused), ("Cancel", cancel_focused)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, "Quit", sections, false);
}

/// Rename and MkDir/New file: a label line, a highlighted editable-field
/// line showing the text typed so far (with a blinking cursor), and a
/// button row.
fn draw_input_dialog(
    frame: &mut Frame,
    classic_style: bool,
    title: &str,
    prompt: &str,
    input: &str,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let sections = vec![
        vec![DialogLine::Text(Line::from(Span::styled(
            prompt.to_string(),
            Style::default().fg(pal.fg),
        )))],
        vec![DialogLine::Field {
            content: vec![
                Span::styled(
                    input.to_string(),
                    Style::default().bg(pal.field_bg).fg(pal.field_fg),
                ),
                Span::styled(
                    "_",
                    Style::default()
                        .bg(pal.field_bg)
                        .fg(pal.field_fg)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
            ],
            fill_bg: pal.field_bg,
        }],
        vec![button_row(
            &[("OK", !cancel_focused), ("Cancel", cancel_focused)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, title, sections, true);
}

const SETTINGS_HINT: &str = " \u{2190}/\u{2192}/Space: toggle   Enter: save   Esc: cancel";

/// Each setting is shown as a two-way switch — "left label [ ]----[x] right
/// label" — rather than a single generic checkbox, so both what's on and
/// what's off are named in positive language instead of one hard-to-phrase
/// boolean. The left column is padded to the widest left label so the
/// switches themselves line up in a column.
const SWITCH_TRACK: &str = "----";

fn draw_settings_dialog(frame: &mut Frame, app: &App, selected: usize) {
    let left_col_width = SettingItem::ALL
        .iter()
        .map(|item| item.left_label().chars().count())
        .max()
        .unwrap_or(0);

    let items: Vec<ListItem> = SettingItem::ALL
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let (left_box, right_box) = if app.setting_value(*item) {
                ("[ ]", "[x]")
            } else {
                ("[x]", "[ ]")
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
                format!(
                    " {:<left_col_width$} {left_box}{SWITCH_TRACK}{right_box} {}",
                    item.left_label(),
                    item.right_label(),
                ),
                style,
            )))
        })
        .chain(std::iter::once(ListItem::new(Line::from(Span::styled(
            SETTINGS_HINT,
            Style::default().fg(Color::DarkGray),
        )))))
        .collect();

    let hint_len = SETTINGS_HINT.chars().count();
    let switch_width = 3 + SWITCH_TRACK.len() + 3; // "[ ]" + track + "[x]"
    let width = SettingItem::ALL
        .iter()
        .map(|item| 1 + left_col_width + 1 + switch_width + 1 + item.right_label().chars().count() + 2)
        .chain(std::iter::once(hint_len + 2))
        .max()
        .unwrap_or(20) as u16;
    // +2 for a 1-column/1-row padding between the border and the content,
    // on top of the border itself.
    let width = (width + 2).min(frame.area().width);
    let height = (SettingItem::ALL.len() as u16 + 3 + 2).min(frame.area().height);

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
        .style(Style::default().bg(Color::Black).fg(Color::White))
        .padding(Padding::uniform(1));

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
        let style = match (app.classic_style, is_selected) {
            (true, true) => Style::default()
                .fg(classic::MENU_BAR_SELECTED_FG)
                .bg(classic::MENU_BAR_SELECTED_BG)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(classic::MENU_BAR_FG).bg(classic::MENU_BAR_BG),
            (false, true) => Style::default()
                .fg(Color::White)
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
            (false, false) => Style::default().fg(Color::Black).bg(Color::Gray),
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
    let bar_bg = if app.classic_style {
        classic::MENU_BAR_BG
    } else {
        Color::Gray
    };
    let bar = Paragraph::new(Line::from(spans)).style(Style::default().bg(bar_bg));
    frame.render_widget(bar, area);
}

/// A drop-shadow overlay. Two earlier attempts taught what doesn't work
/// here: `Modifier::DIM` only dims a cell's foreground per the ANSI spec,
/// not its background, and most of the shadow falls on blank/background
/// cells; and named colors like `Color::DarkGray` are whatever the
/// terminal's color scheme remaps them to (some dark themes map it close
/// to black, same trap as the old hardcoded `Color::White` text bug).
///
/// Where the cell underneath is a known `Color::Rgb` (true in classic
/// mode, since every classic color is an explicit RGB constant), this
/// darkens that exact color instead of replacing it — real "see-through"
/// shadow rather than a flat gray patch. For anything else (a named/Reset
/// color, i.e. following the terminal's own theme, where the actual RGB
/// isn't knowable) it falls back to a fixed dark gray fill — reverse video
/// was tried here too, but looked inconsistent against Omarchy's dynamic
/// theming, so a flat fill is the more predictable choice for that case.
struct Shadow;

/// Multiplies each RGB channel by this factor to darken it for the shadow.
const SHADOW_DARKEN_FACTOR: f32 = 0.35;
/// Flat fallback for cells whose color isn't a known RGB to darken.
const SHADOW_FALLBACK: Color = Color::Rgb(30, 30, 30);

fn darken(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return SHADOW_FALLBACK;
    };
    Color::Rgb(
        (r as f32 * SHADOW_DARKEN_FACTOR) as u8,
        (g as f32 * SHADOW_DARKEN_FACTOR) as u8,
        (b as f32 * SHADOW_DARKEN_FACTOR) as u8,
    )
}

impl Widget for Shadow {
    fn render(self, area: Rect, buf: &mut Buffer) {
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(darken(cell.bg));
                    cell.set_fg(darken(cell.fg));
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
            let style = match (app.classic_style, is_selected, enabled) {
                (true, true, _) => Style::default()
                    .fg(classic::MENU_SELECTED_ITEM_FG)
                    .bg(classic::MENU_SELECTED_ITEM_BG)
                    .add_modifier(Modifier::BOLD),
                (true, false, true) => {
                    Style::default().fg(classic::MENU_ITEM_FG).bg(classic::MENU_BG)
                }
                (true, false, false) => {
                    Style::default().fg(classic::MENU_DISABLED_FG).bg(classic::MENU_BG)
                }
                (false, true, true) => Style::default()
                    .fg(Color::White)
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
                (false, true, false) => Style::default().fg(Color::DarkGray).bg(Color::Blue),
                (false, false, true) => Style::default().fg(Color::Black).bg(Color::Gray),
                (false, false, false) => Style::default().fg(Color::DarkGray).bg(Color::Gray),
            };
            // Classic NC shows the shortcut in white regardless of the
            // (yellow/muted) label color, but only for a plain, unselected
            // row — a selected/highlighted row is one uniform reverse-video
            // color for the whole line, same as everywhere else.
            let shortcut_style = if app.classic_style && !is_selected && enabled {
                Style::default().fg(Color::Rgb(255, 255, 255)).bg(classic::MENU_BG)
            } else {
                style
            };
            let (label_part, shortcut_part) = match action.shortcut() {
                Some(shortcut) => (
                    format!("{:<label_width$}", action.label(), label_width = row_text_width - shortcut.len()),
                    format!("{shortcut:>shortcut_width$}", shortcut_width = shortcut.len()),
                ),
                None => (format!("{:<row_text_width$}", action.label()), String::new()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(" ", style),
                Span::styled(label_part, style),
                Span::styled(shortcut_part, shortcut_style),
                Span::styled(" ", style),
            ]))
        })
        .collect();

    let block = if app.classic_style {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(classic::MENU_BORDER_FG).add_modifier(Modifier::BOLD))
            .style(Style::default().bg(classic::MENU_BG).fg(classic::MENU_ITEM_FG))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Gray).fg(Color::Black))
    };

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
    let cwd = match app.active {
        Side::Left => &app.left.cwd,
        Side::Right => &app.right.cwd,
    };
    // Once the row's background is forced to black below, the typed text
    // can't be left as Color::Reset (the terminal's own default foreground)
    // — on a light terminal theme that default is a dark color, which
    // would go invisible on a now-black background. Same lesson as the
    // earlier white-on-white pane text bug.
    // 0xAFA8AF sampled from the reference screenshot's prompt text — the
    // classic DOS "light gray" (palette color 7), not pure white.
    let typed_text_fg = if app.classic_style {
        Color::Rgb(0xAF, 0xA8, 0xAF)
    } else {
        Color::Reset
    };
    // Indented 1 column, matching the F-key bar below it — as a real
    // painted leading space rather than shrinking the render area, so
    // that column gets the row's own background instead of leaving it
    // untouched (showing whatever was underneath, e.g. the terminal's own
    // background peeking through as a stray-colored notch).
    let indent_style = if app.classic_style {
        Style::default().bg(classic::PROMPT_BG)
    } else {
        Style::default()
    };
    // The reference screenshot doesn't highlight the cwd at all — the
    // whole prompt is one uniform gray. Named `Color::Green` is also
    // exactly the kind of color a dynamically-themed terminal (e.g.
    // Omarchy) can remap to something else entirely — it showed up as
    // yellow, not green, the same lesson as every other named-color bug
    // this session.
    let cwd_fg = if app.classic_style {
        typed_text_fg
    } else {
        Color::Green
    };
    let mut spans = Vec::new();
    // The 1-column indent looks right against the terminal's own
    // background, but looks off-balance once the row has its own solid
    // blue/black backgrounds (classic mode) — skip it there.
    if !app.classic_style {
        spans.push(Span::styled(" ", indent_style));
    }
    spans.push(Span::styled(
        format!("{}> ", cwd.display()),
        Style::default().fg(cwd_fg).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(&app.command_line, Style::default().fg(typed_text_fg)));
    let line = Line::from(spans);
    let paragraph = if app.classic_style {
        Paragraph::new(line).style(Style::default().bg(classic::PROMPT_BG))
    } else {
        Paragraph::new(line)
    };
    frame.render_widget(paragraph, area);
}

fn format_modified(modified: Option<std::time::SystemTime>) -> String {
    match modified {
        Some(time) => DateTime::<Local>::from(time)
            .format("%d-%m-%y %H:%M")
            .to_string(),
        None => String::new(),
    }
}

/// Fixed colors for the "Classic retro Norton Commander" setting — real
/// RGB rather than named ANSI colors, so a dynamically-themed terminal
/// (e.g. Omarchy's per-wallpaper palette) can't remap them away from the
/// intended look, the same lesson learned from the menu shadow.
mod classic {
    use ratatui::style::Color;

    pub const BG: Color = Color::Rgb(0, 0, 170);
    pub const DIR_FG: Color = Color::Rgb(255, 255, 255);
    // 0x50FFFF from the reference screenshot — a light turquoise, used for
    // both plain file text and the active pane's border.
    pub const FILE_FG: Color = Color::Rgb(0x50, 0xFF, 0xFF);
    pub const ACTIVE_BORDER_FG: Color = FILE_FG;
    pub const INACTIVE_BORDER_FG: Color = Color::Rgb(0, 170, 170);
    pub const HIGHLIGHT_BG: Color = Color::Rgb(0, 170, 170);
    pub const HIGHLIGHT_FG: Color = Color::Rgb(0, 0, 0);

    // Menu bar and dropdown — same turquoise background as the dropdown,
    // not black, with mostly-white text (a single accelerator letter is
    // yellow in the reference screenshot; not implemented yet).
    pub const MENU_BG: Color = Color::Rgb(0, 170, 170);
    pub const MENU_BAR_BG: Color = MENU_BG;
    pub const MENU_BAR_FG: Color = Color::Rgb(255, 255, 255);
    pub const MENU_BAR_SELECTED_BG: Color = Color::Rgb(255, 255, 255);
    pub const MENU_BAR_SELECTED_FG: Color = Color::Rgb(0, 0, 0);
    pub const MENU_ITEM_FG: Color = Color::Rgb(255, 255, 255);
    pub const MENU_DISABLED_FG: Color = Color::Rgb(0, 85, 85);
    pub const MENU_SELECTED_ITEM_BG: Color = Color::Rgb(0, 0, 0);
    pub const MENU_SELECTED_ITEM_FG: Color = Color::Rgb(255, 255, 255);
    pub const MENU_BORDER_FG: Color = Color::Rgb(255, 255, 255);

    // F-key bar.
    // Sampled from the reference: the F-key number is the same light gray
    // as the prompt text, not yellow.
    pub const FN_KEY_NUM_FG: Color = Color::Rgb(0xAF, 0xA8, 0xAF);
    pub const FN_KEY_NUM_BG: Color = Color::Rgb(0, 0, 0);
    pub const FN_KEY_LABEL_FG: Color = Color::Rgb(0, 0, 0);
    pub const FN_KEY_LABEL_BG: Color = Color::Rgb(0, 170, 170);

    // The command-line and status rows sit on plain black, not the pane's
    // blue — matching the original, where only the two panels are blue.
    pub const PROMPT_BG: Color = Color::Rgb(0, 0, 0);

    // Big double-bordered dialogs (Copy/Move/Delete, Rename, MkDir/New
    // file, Quit) — the classic DOS gray dialog box, not the panels' blue.
    pub const DIALOG_BG: Color = Color::Rgb(0xAF, 0xA8, 0xAF);
    pub const DIALOG_FG: Color = Color::Rgb(0, 0, 0);
    pub const DIALOG_BORDER_FG: Color = Color::Rgb(0, 0, 0);
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &Pane,
    is_active: bool,
    list_state: &mut ListState,
    classic_style: bool,
) {
    let border_style = if classic_style {
        let fg = if is_active {
            classic::ACTIVE_BORDER_FG
        } else {
            classic::INACTIVE_BORDER_FG
        };
        Style::default().fg(fg).add_modifier(Modifier::BOLD)
    } else if is_active {
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
                let fg = if classic_style { classic::DIR_FG } else { Color::Cyan };
                Style::default().fg(fg).add_modifier(Modifier::BOLD)
            } else if entry.is_executable {
                // Classic mode treats executables exactly like directories
                // (same white, bold) rather than a distinct green — the
                // size/date columns already tell them apart.
                let fg = if classic_style { classic::DIR_FG } else { Color::Green };
                Style::default().fg(fg).add_modifier(Modifier::BOLD)
            } else if classic_style {
                Style::default().fg(classic::FILE_FG)
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
                if classic_style { "<DIR>".to_string() } else { String::new() }
            } else {
                format_size(entry.size)
            };
            let name = fit_name(&entry.name, name_width);
            let label = format!("{name} {size_label:>SIZE_COLUMN_WIDTH$} {date} ");
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    let mut block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);
    if classic_style {
        block = block.style(Style::default().bg(classic::BG));
    }

    // Only the active pane shows a highlight at all (see the note below on
    // `list_state.select`), independent of which palette is in use.
    let highlight_style = if !is_active {
        Style::default()
    } else if classic_style {
        Style::default()
            .bg(classic::HIGHLIGHT_BG)
            .fg(classic::HIGHLIGHT_FG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
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
fn draw_preview_pane(
    frame: &mut Frame,
    area: Rect,
    source: &Pane,
    focused: bool,
    scroll: usize,
    classic_style: bool,
) {
    let title = match source.selected_entry() {
        Some(entry) if entry.name != ".." => {
            source.cwd.join(&entry.name).to_string_lossy().to_string()
        }
        _ => source.cwd.to_string_lossy().to_string(),
    };

    let border_style = if classic_style {
        let fg = if focused {
            classic::ACTIVE_BORDER_FG
        } else {
            classic::INACTIVE_BORDER_FG
        };
        Style::default().fg(fg).add_modifier(Modifier::BOLD)
    } else if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut block = Block::default()
        .title(format!("Preview: {title}"))
        .borders(Borders::ALL)
        .border_style(border_style);
    if classic_style {
        block = block
            .style(Style::default().bg(classic::BG).fg(classic::FILE_FG));
    }

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
                    let fg = if classic_style { classic::DIR_FG } else { Color::Cyan };
                    Style::default().fg(fg).add_modifier(Modifier::BOLD)
                } else if classic_style {
                    Style::default().fg(classic::FILE_FG)
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

/// Command > Show Logs: reads the tail of the log file fresh each draw
/// (simple, and the file is small enough that this is cheap) and shows as
/// many of the most recent lines as fit.
fn draw_logs(frame: &mut Frame) {
    let width = frame.area().width.saturating_sub(4).max(20);
    let height = frame.area().height.saturating_sub(4).max(3);
    let area = Rect {
        x: (frame.area().width.saturating_sub(width)) / 2,
        y: (frame.area().height.saturating_sub(height)) / 2,
        width,
        height,
    };

    let visible_rows = height.saturating_sub(2) as usize; // minus the block's borders
    let all_lines = std::fs::read_to_string(logging::log_path()).unwrap_or_default();
    let mut lines: Vec<&str> = all_lines.lines().collect();
    let total = lines.len();
    if total > visible_rows {
        lines = lines.split_off(total - visible_rows);
    }
    let shown: Vec<Line> = if lines.is_empty() {
        vec![Line::from(Span::styled(
            "(no log entries yet)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        lines
            .iter()
            .map(|line| Line::from(Span::styled(*line, Style::default().fg(Color::White))))
            .collect()
    };

    let block = Block::default()
        .title(format!("Logs — last {} lines — press any key to close", shown.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .style(Style::default().bg(Color::Black).fg(Color::White));

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(shown).block(block), area);
}

fn draw_fn_key_bar(frame: &mut Frame, area: Rect, app: &mut App) {
    // Same reasoning as draw_command_line: the 1-column indent and the
    // gaps between tiles are real painted spaces (styled with the row's
    // background), not just skipped/unshrunk area — otherwise those
    // columns show whatever was underneath instead of matching the row.
    let gap_style = if app.classic_style {
        Style::default().bg(classic::PROMPT_BG)
    } else {
        Style::default()
    };
    // Same as draw_command_line: the leading indent looks off-balance once
    // the row has its own solid background (classic mode), so skip it there.
    let indent = if app.classic_style { 0u16 } else { 1u16 };

    // Spread the tiles evenly across the full width instead of packing them
    // to the left; on a narrow terminal the tail simply gets clipped, same
    // as before. A 1-column gap between tiles (not before F1) needs
    // reserving len-1 columns up front.
    let gaps = FN_KEYS.len() as u16 - 1;
    let usable = area.width.saturating_sub(indent + gaps);
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
    let mut x = area.x + indent;
    let mut spans = if indent > 0 {
        vec![Span::styled(" ", gap_style)]
    } else {
        Vec::new()
    };
    for (idx, fn_key) in FN_KEYS.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" ", gap_style));
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

        let (num_fg, num_bg, label_fg, label_bg) = if app.classic_style {
            (
                classic::FN_KEY_NUM_FG,
                classic::FN_KEY_NUM_BG,
                classic::FN_KEY_LABEL_FG,
                classic::FN_KEY_LABEL_BG,
            )
        } else {
            (Color::Yellow, Color::Black, Color::Black, Color::Cyan)
        };
        spans.push(Span::styled(
            fn_key.key,
            Style::default().fg(num_fg).bg(num_bg).add_modifier(Modifier::BOLD),
        ));
        let label_width = tile_widths[idx].saturating_sub(fn_key.key.len() as u16) as usize;
        spans.push(Span::styled(
            format!("{:<label_width$}", fn_key.label),
            Style::default().fg(label_fg).bg(label_bg),
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
        // Deterministic regardless of whatever's actually saved on disk —
        // App::new() loads real persisted settings, and classic_style
        // changes what color the shadow darkens to.
        app.classic_style = false;
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
        // Non-classic mode: the cell underneath is a named/Reset color, so
        // the shadow falls back to a flat dark gray fill.
        assert_eq!(cell.bg, Color::Rgb(30, 30, 30));
    }

    #[test]
    fn exact_width_name_is_unchanged() {
        let result = fit_name("1234567890", 10);
        assert_eq!(result, "1234567890");
    }
}
