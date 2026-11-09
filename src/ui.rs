use std::path::PathBuf;

use chrono::{DateTime, Local};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Widget, Wrap,
};

use crate::app::{App, Dialog, DialogKind, SettingItem, Side};
use crate::bulk_rename::Plan;
use crate::editor::{self, Editor, Pos, ScreenRow};
use crate::image_preview::{ImagePreview, ImageState};
use crate::job::Job;
use crate::logging;
use crate::menu::{Action, FN_KEYS};
use crate::pane::{Pane, SortKey};
use crate::preview::{self, PreviewContent};
use crate::transfer::{TextField, Transfer};
use ratatui_image::Image;

pub fn draw(frame: &mut Frame, app: &mut App) {
    if app.editor.as_ref().is_some_and(|editor| editor.full_screen) {
        draw_full_screen_editor(frame, app);
    } else {
        draw_panels(frame, app);
    }
    draw_overlays(frame, app);
}

/// Whether anything is open over the panes (or the editor), which then
/// can't have the cursor or a picture showing through.
fn overlay_open(app: &App) -> bool {
    app.menu_open
        || app.help_open
        || app.logs_open
        || app.about_open
        || app.job_visible
        || app.error_dialog.is_some()
        || !matches!(app.dialog, Dialog::None)
}

/// F4 a second time: the editor on the whole screen, under the menu bar as
/// everywhere else — or, with the "Focus mode editor" setting, alone, the
/// menu bar showing only while F9/Alt+letter has it open, over the
/// editor's title.
fn draw_full_screen_editor(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    app.left_pane_area = Rect::default();
    app.right_pane_area = Rect::default();
    app.fn_key_tiles.clear();
    app.menu_bar_tiles.clear();
    app.menu_item_tiles.clear();
    let bar = Rect { height: 1, ..area };
    if app.editor_focus_mode {
        draw_editor_in(frame, area, app);
        if app.menu_open {
            draw_menu_bar(frame, bar, app);
        }
    } else {
        let below = Rect {
            y: area.y + bar.height.min(area.height),
            height: area.height.saturating_sub(1),
            ..area
        };
        draw_menu_bar(frame, bar, app);
        draw_editor_in(frame, below, app);
    }
    if app.menu_open {
        draw_menu_dropdown(frame, bar, app);
    }
}

/// `draw_editor` with what it needs from `app`, recording where it went for
/// the mouse.
fn draw_editor_in(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = !app.list_focus;
    let show_cursor = focused && !overlay_open(app);
    let classic_style = app.classic_style;
    if let Some(editor) = app.editor.as_mut() {
        app.editor_text_area =
            draw_editor(frame, area, editor, classic_style, focused, show_cursor);
        app.editor_area = area;
    }
}

fn draw_panels(frame: &mut Frame, app: &mut App) {
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

    // -2 for the pane's own top/bottom border, and 2 more for the divider +
    // status footer row when that's on (see `draw_pane`), so PgUp/PgDn page
    // by exactly what's actually visible in the file listing.
    let look = PaneLook::of(app);
    let footer_rows = if look.show_totals { 2 } else { 0 };
    let header_rows = if look.show_headers { 1 } else { 0 };
    app.pane_visible_lines = panes[0]
        .height
        .saturating_sub(2 + footer_rows + header_rows) as usize;
    app.left_pane_area = panes[0];
    app.right_pane_area = panes[1];

    if app.editor.is_some() {
        // The editor takes the other pane, like the quick view; the listing
        // beside it has the highlight only while it has the keyboard (Tab).
        let (list_area, editor_area) = match app.active {
            Side::Left => (panes[0], panes[1]),
            Side::Right => (panes[1], panes[0]),
        };
        let (pane, list_state) = match app.active {
            Side::Left => (&app.left, &mut app.left_list_state),
            Side::Right => (&app.right, &mut app.right_list_state),
        };
        draw_pane(frame, list_area, pane, app.list_focus, list_state, look);
        draw_editor_in(frame, editor_area, app);
    } else if app.quick_view {
        // Graphics sit on top of the text grid and don't mix with anything
        // drawn over them, so a picture steps aside for the duration.
        let covered = overlay_open(app);
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
                    look,
                );
                draw_preview_pane(
                    frame,
                    panes[1],
                    &app.left,
                    PreviewLook::of(app, covered),
                    &mut app.image_preview,
                    app.preview_find.as_ref(),
                );
            }
            Side::Right => {
                app.preview_visible_lines = panes[0].height.saturating_sub(2) as usize;
                app.preview_visible_width = panes[0].width.saturating_sub(2) as usize;
                draw_preview_pane(
                    frame,
                    panes[0],
                    &app.right,
                    PreviewLook::of(app, covered),
                    &mut app.image_preview,
                    app.preview_find.as_ref(),
                );
                draw_pane(
                    frame,
                    panes[1],
                    &app.right,
                    !app.preview_focus,
                    &mut app.right_list_state,
                    look,
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
            look,
        );
        draw_pane(
            frame,
            panes[1],
            &app.right,
            app.active == Side::Right,
            &mut app.right_list_state,
            look,
        );
    }

    draw_command_line(frame, root[2], app);
    draw_fn_key_bar(frame, root[3], app);

    if app.menu_open {
        draw_menu_dropdown(frame, root[0], app);
    }
}

/// Dialogs and boxes over everything else, in either layout.
fn draw_overlays(frame: &mut Frame, app: &mut App) {
    match &app.dialog {
        Dialog::Confirm { kind, items } => {
            draw_confirm_dialog(
                frame,
                app.classic_style,
                *kind,
                &item_summary(items),
                app.dialog_cancel_focused,
            );
        }
        Dialog::Transfer(transfer) => {
            draw_transfer_dialog(
                frame,
                app.classic_style,
                transfer,
                app.dialog_cancel_focused,
            );
        }
        Dialog::TextInput {
            kind,
            field,
            editing,
        } => {
            draw_input_dialog(
                frame,
                app.classic_style,
                kind.title(),
                kind.prompt(),
                field,
                *editing,
                app.dialog_cancel_focused,
            );
        }
        Dialog::Rename {
            field,
            editing,
            src,
        } => {
            let old_name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            draw_input_dialog(
                frame,
                app.classic_style,
                "Rename",
                &format!("Rename \"{old_name}\" to:"),
                field,
                *editing,
                app.dialog_cancel_focused,
            );
        }
        Dialog::ConfirmRenameSelected { dir, plan } => {
            draw_rename_selected_dialog(
                frame,
                app.classic_style,
                &dir.display().to_string(),
                plan,
                app.dialog_cancel_focused,
            );
        }
        Dialog::UnsavedChanges { focused, .. } => {
            let name = app
                .editor
                .as_ref()
                .map_or_else(|| "the file".to_string(), Editor::file_name);
            draw_unsaved_dialog(frame, app.classic_style, &name, *focused);
        }
        Dialog::ConfirmSaveAs { target } => {
            let name = target.file_name().map_or_else(
                || target.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            draw_save_over_dialog(frame, app.classic_style, &name, app.dialog_cancel_focused);
        }
        Dialog::ConfirmQuit => {
            draw_quit_dialog(
                frame,
                app.classic_style,
                app.job.as_ref(),
                app.dialog_cancel_focused,
            );
        }
        Dialog::Settings { selected, .. } => {
            draw_settings_dialog(frame, app, *selected);
        }
        Dialog::None => {}
    }

    if app.job_visible
        && let Some(job) = &app.job
    {
        draw_job_dialog(frame, app, job);
    }

    if app.help_open {
        draw_help(frame, app.classic_style, app.help_page);
    }

    if app.logs_open {
        draw_logs(frame, app.classic_style);
    }

    if app.about_open {
        draw_about(frame, app.classic_style);
    }

    if let Some(message) = &app.error_dialog {
        if app.error_dialog_is_notice {
            draw_notice_dialog(frame, app.classic_style, message);
        } else {
            draw_error_dialog(frame, app.classic_style, message);
        }
    }
}

/// A blocking "in your face" popup for `App::set_error` — unlike a routine
/// confirmation, an error demands a keypress to dismiss (any key, handled in
/// `main.rs`) so it can't be missed the way the log-only status line can.
/// Same double-bordered box as every other dialog, in the classic red of
/// Norton Commander's error boxes.
fn draw_error_dialog(frame: &mut Frame, classic_style: bool, message: &str) {
    draw_message_box(frame, &error_palette(classic_style), "Error", message);
}

/// `App::set_notice`: the error box's shape, in the ordinary dialog colors.
fn draw_notice_dialog(frame: &mut Frame, classic_style: bool, message: &str) {
    draw_message_box(frame, &dialog_palette(classic_style), "Note", message);
}

fn draw_message_box(frame: &mut Frame, pal: &DialogPalette, title: &str, message: &str) {
    let wrap_width = (frame.area().width as usize)
        .saturating_sub(16)
        .clamp(20, 64);
    let lines = wrap_message(message, wrap_width)
        .into_iter()
        .map(|line| {
            DialogLine::Text(Line::from(Span::styled(
                line,
                Style::default().fg(pal.fg).add_modifier(Modifier::BOLD),
            )))
        })
        .collect();
    let sections = vec![lines, vec![button_row(&[("OK", true)], pal)]];
    draw_dialog_box(frame, pal, title, sections, 0);
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

/// The F1 help pages: a title and its lines. Left/Right flip between them.
const HELP_PAGES: &[(&str, &[&str])] = &[
    (
        "Function keys",
        &[
            "F1        Help          This screen",
            "F2        Rename        Rename the selected entry",
            "F3        View          Quick view (or $PAGER, per Settings)",
            "F4        Edit          Edit the file (built-in, or $EDITOR)",
            "Shift+F4  New File      Create a new empty file",
            "F5        Copy          Copy selection/marked to the other pane",
            "F6        Move          Move selection/marked to the other pane",
            "F7        MkDir         Create a new directory",
            "F8        Delete        Move to trash (or delete, per Settings)",
            "Shift+F8  Delete        Delete permanently",
            "F9        Menu          Open the pulldown menu",
            "F10       Quit          Quit Peter Commander",
            "",
            "In windows with a name or path, typing edits it. Up enters the",
            "field (Left/Right move the cursor), Down goes to the buttons.",
            "F5/F6: a new name copies beside a same-named file.",
        ],
    ),
    (
        "Shortcuts and navigation",
        &[
            "Ctrl+F    Quick Search   Jump to a name by typing part of it",
            "Ctrl+B    Progress       Show a copy/move running in background",
            "Ctrl+O    Terminal       Reveal the terminal under the panels",
            "Ctrl+L    Show Logs      Recent status and error messages",
            "Ctrl+S    Settings       Open the settings",
            "Ctrl+Q    Quit           Same as F10",
            "Alt+F1    Left = Right   Point left pane at right pane's dir",
            "Alt+F2    Right = Left   Point right pane at left pane's dir",
            "Alt+F/O/C Menu           Open the File/Options/Command menu",
            "",
            "Tab       Switch the active pane",
            "Up/Down   Move the selection",
            "Home/End  Jump to the top/bottom of the listing",
            "PgUp/PgDn Move by one screenful",
            "Insert    Mark/unmark the entry and move down",
        ],
    ),
    (
        "Built-in editor",
        &[
            "F4 opens the file in the other pane; F4 again: full screen.",
            "Tab: keys to the list beside it (F4 edits, F3 views) and back.",
            "Esc clears a selection, or else closes the editor (also there).",
            "Closing and quitting ask first about unsaved changes.",
            "Ctrl+S        Save           Ctrl+Z/Y      Undo/redo",
            "F2            Rename the file  File menu    Save As...",
            "Ctrl+A        Select all     Ctrl+C/X/V    Copy/cut/paste",
            "Shift+arrows  Select         Ctrl+\u{2190}/\u{2192}      Move by word",
            "Home/End      Line start/end Ctrl+Home/End File start/end",
            "Tab (full sc.) 4 spaces      Shift+Tab     Remove 4 spaces",
            "Ctrl+Bksp/Del Delete a word  F9, Alt+E     Menu, Edit menu",
            "Ctrl+F        Find           F3/Shift+F3   Next/previous",
            "In Find, typing searches as you go (case doesn't matter);",
            "Enter keeps the match selected, Esc clears it; F3 goes on.",
            "Wrap Lines (Edit or Options) wraps long lines, F3's as well.",
            "Command > Open Editor: a new, untitled text. Line endings",
            "are kept (a stray CR shows as \u{240d}). Settings: External editor",
            "sends F4 to $EDITOR; Focus mode hides the bar full screen.",
        ],
    ),
    (
        "Tips",
        &[
            "Type anywhere to fill the command line; Enter runs it in the",
            "active pane's directory, or opens the selection if it's empty.",
            "Esc clears it, or closes F3's view. A paste never runs it:",
            "only its first line goes in, to check and run with Enter.",
            "",
            "Command > Sort by Name/Type/Size/Date sorts the active pane;",
            "picking the same sort again reverses it. Each pane keeps its",
            "own sort (shown top right, e.g. [Size\u{2193}]), saved on quit.",
            "",
            "File > Rename Selected (two or more marked) edits the names in",
            "$EDITOR, one per line, and asks before renaming.",
            "",
            "Copy/Move show a progress window: Esc or [ Background ] lets it",
            "run while you work; Ctrl+B shows it again.",
            "",
            "Tab onto the F3 preview: \u{2190}/\u{2192} scroll it sideways (unwrapped),",
            "Ctrl+F finds in it, F3/Shift+F3 next/previous, Esc clears.",
            "Mouse: scroll a pane or the preview, click the menu or a key.",
        ],
    ),
];

pub const HELP_PAGE_COUNT: usize = HELP_PAGES.len();

fn draw_help(frame: &mut Frame, classic_style: bool, page: usize) {
    let pal = dialog_palette(classic_style);
    let (_, body) = HELP_PAGES[page.min(HELP_PAGES.len() - 1)];

    // Every page gets the same height and width, so flipping pages doesn't
    // make the box jump around.
    let rows = HELP_PAGES
        .iter()
        .map(|(_, lines)| lines.len())
        .max()
        .unwrap_or(0);
    // The titles count too, so a long page title can't widen just its page.
    let widest = HELP_PAGES
        .iter()
        .flat_map(|(_, lines)| lines.iter())
        .map(|line| line.chars().count())
        .chain((0..HELP_PAGES.len()).map(|idx| help_title(idx).chars().count() + 4))
        .max()
        .unwrap_or(0);
    let min_width = (widest + 2 * DIALOG_PAD + 2) as u16 + 2 * OUTER_PAD_X;

    let mut lines: Vec<DialogLine> = body
        .iter()
        .map(|line| DialogLine::Text(Line::from(Span::styled(*line, Style::default().fg(pal.fg)))))
        .collect();
    while lines.len() < rows {
        lines.push(DialogLine::Text(Line::from("")));
    }

    let page_marks: String = (0..HELP_PAGES.len())
        .map(|idx| if idx == page { '\u{25cf}' } else { '\u{25cb}' })
        .map(|mark| format!("{mark} "))
        .collect();
    let footer = DialogLine::Centered(Line::from(vec![
        Span::styled("\u{2190} ", Style::default().fg(pal.fg)),
        Span::styled(
            page_marks,
            Style::default().fg(pal.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "\u{2192}   any other key closes",
            Style::default().fg(pal.fg),
        ),
    ]));

    // The program and version head every page, above the page's own lines.
    let heading = DialogLine::Centered(Line::from(Span::styled(
        format!("{PROGRAM_NAME} {}", env!("CARGO_PKG_VERSION")),
        Style::default().fg(pal.fg).add_modifier(Modifier::BOLD),
    )));

    draw_dialog_box(
        frame,
        &pal,
        &help_title(page),
        vec![vec![heading], lines, vec![footer]],
        min_width,
    );
}

/// The F1 box title for `page`, e.g. "Help — Tips (3/3)".
fn help_title(page: usize) -> String {
    let (title, _) = HELP_PAGES[page.min(HELP_PAGES.len() - 1)];
    format!("Help \u{2014} {title} ({}/{})", page + 1, HELP_PAGES.len())
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

/// Error boxes: the classic white-on-red Norton Commander alert, or — when
/// following the terminal theme — the usual dialog colors with a red frame.
fn error_palette(classic_style: bool) -> DialogPalette {
    let red = Color::Rgb(170, 0, 0);
    let white = Color::Rgb(255, 255, 255);
    if classic_style {
        DialogPalette {
            bg: red,
            fg: white,
            border_fg: white,
            field_bg: white,
            field_fg: red,
            button_default_bg: white,
            button_default_fg: red,
        }
    } else {
        DialogPalette {
            border_fg: Color::Red,
            button_default_bg: Color::Red,
            button_default_fg: Color::White,
            ..dialog_palette(false)
        }
    }
}

/// One line of dialog content. `Field` is padded to the dialog's full inner
/// width when rendered so its background fills the whole row, like the
/// highlighted destination/input bar in classic Norton Commander dialogs;
/// `Text`/`Centered` are rendered as-is (`Centered` for the button row).
enum DialogLine<'a> {
    Text(Line<'a>),
    Field {
        content: Vec<Span<'a>>,
        fill_bg: Color,
    },
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
    // Never narrower than half the screen, however short the content is —
    // applied to the full outer footprint, border pad included. Quit opts
    // out, staying sized to its (short) content instead.
    let min_width = if min_half_screen {
        frame.area().width / 2
    } else {
        0
    };
    draw_dialog_box(
        frame,
        &dialog_palette(classic_style),
        title,
        sections,
        min_width,
    );
}

/// `draw_dialog_frame` with an explicit palette (e.g. the red error box)
/// and minimum outer width.
fn draw_dialog_box(
    frame: &mut Frame,
    pal: &DialogPalette,
    title: &str,
    sections: Vec<Vec<DialogLine>>,
    min_width: u16,
) {
    let content_width = sections
        .iter()
        .flat_map(|section| section.iter())
        .map(DialogLine::natural_width)
        .max()
        .unwrap_or(0)
        .max(title.chars().count() + 4);
    let box_width = content_width as u16 + 2 * DIALOG_PAD as u16 + 2;
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
                    let mut spans = vec![Span::styled(
                        " ".repeat(DIALOG_PAD),
                        Style::default().bg(pal.bg),
                    )];
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

fn item_summary(items: &[(String, PathBuf)]) -> String {
    match items {
        [(name, _)] => format!("\"{name}\""),
        _ => format!("{} items", items.len()),
    }
}

/// Delete/Trash confirmation.
fn draw_confirm_dialog(
    frame: &mut Frame,
    classic_style: bool,
    kind: DialogKind,
    summary: &str,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let verb = kind.verb();

    let message = match kind {
        DialogKind::Trash | DialogKind::TrashByCopy => format!("Move {summary} to trash?"),
        DialogKind::Delete => format!("Permanently delete {summary}?"),
        _ => format!("{verb} {summary}?"),
    };
    let mut sections = vec![vec![DialogLine::Text(Line::from(Span::styled(
        message,
        Style::default().fg(pal.fg),
    )))]];

    let mut primary = verb;
    if kind == DialogKind::TrashByCopy {
        primary = "Copy to trash";
        let warn = |line: &'static str| {
            DialogLine::Text(Line::from(Span::styled(
                line,
                Style::default().fg(pal.fg).add_modifier(Modifier::BOLD),
            )))
        };
        let note = |line: &'static str| {
            DialogLine::Text(Line::from(Span::styled(line, Style::default().fg(pal.fg))))
        };
        sections.push(vec![
            warn("WARNING: no usable trash on this drive."),
            note("Everything will be COPIED into your home trash, then"),
            note("deleted here. Big files take a while; it runs with a"),
            note("progress window you can send to the background."),
            note("(Shift+F8 deletes permanently instead.)"),
        ]);
    }

    sections.push(vec![button_row(
        &[(primary, !cancel_focused), ("Cancel", cancel_focused)],
        &pal,
    )]);

    draw_dialog_frame(frame, classic_style, verb, sections, true);
}

/// How many cells of a dialog's width a `DialogLine::Field` can use, on a
/// screen `screen_width` wide: what's left inside the outer margin, the border
/// and the padding on each side.
fn dialog_field_width(screen_width: u16) -> usize {
    (screen_width as usize).saturating_sub(2 * OUTER_PAD_X as usize + 2 + 2 * DIALOG_PAD)
}

/// Which characters of a field fit in `max_cells` with the cursor (the cell
/// at `cursor`, or one past the end) among them, as a `start..end` range.
/// Text that doesn't fit gets `…` on each clipped side, so two cells are kept
/// for those, and some of what follows the cursor stays visible.
fn visible_chars(chars: &[char], cursor: usize, max_cells: usize) -> (usize, usize) {
    let width = |c: char| Span::raw(c.to_string()).width();
    let cursor_cells = chars.get(cursor).map_or(1, |&c| width(c).max(1));
    let total: usize =
        chars.iter().map(|&c| width(c)).sum::<usize>() + usize::from(cursor >= chars.len());
    if total <= max_cells {
        return (0, chars.len());
    }
    let budget = max_cells.saturating_sub(2).max(cursor_cells);
    let mut used = cursor_cells;
    let mut start = cursor;
    let mut end = (cursor + 1).min(chars.len());
    let right_context = used + budget / 4;
    while end < chars.len() && used + width(chars[end]) <= right_context {
        used += width(chars[end]);
        end += 1;
    }
    while start > 0 && used + width(chars[start - 1]) <= budget {
        start -= 1;
        used += width(chars[start]);
    }
    while end < chars.len() && used + width(chars[end]) <= budget {
        used += width(chars[end]);
        end += 1;
    }
    (start, end)
}

/// Which of the two buttons to highlight. Left/Right move the cursor while the
/// field has the focus, so none may look selectable then.
fn focused_buttons(editing: bool, cancel_focused: bool) -> (bool, bool) {
    (!editing && !cancel_focused, !editing && cancel_focused)
}

/// A dialog's text as a highlighted field. With the focus on it the cursor is
/// a solid block; otherwise, since typing still goes there, a blinking `_` at
/// the end, or an underlined character within the text.
fn text_field_line(
    field: &TextField,
    editing: bool,
    screen_width: u16,
    pal: &DialogPalette,
) -> DialogLine<'static> {
    let style = Style::default().bg(pal.field_bg).fg(pal.field_fg);
    let chars: Vec<char> = field.text().chars().collect();
    let cursor = field.cursor();
    let (start, end) = visible_chars(&chars, cursor, dialog_field_width(screen_width));
    let slice = |from: usize, to: usize| chars[from..to].iter().collect::<String>();

    let mut content = Vec::new();
    if start > 0 {
        content.push(Span::styled("…", style));
    }
    content.push(Span::styled(slice(start, cursor), style));
    content.push(match (chars.get(cursor), editing) {
        (Some(c), true) => Span::styled(c.to_string(), style.add_modifier(Modifier::REVERSED)),
        (None, true) => Span::styled(" ", style.add_modifier(Modifier::REVERSED)),
        (Some(c), false) => Span::styled(c.to_string(), style.add_modifier(Modifier::UNDERLINED)),
        (None, false) => Span::styled("_", style.add_modifier(Modifier::SLOW_BLINK)),
    });
    if end > cursor + 1 {
        content.push(Span::styled(slice(cursor + 1, end), style));
    }
    if end < chars.len() {
        content.push(Span::styled("…", style));
    }
    DialogLine::Field {
        content,
        fill_bg: pal.field_bg,
    }
}

/// Copy/Move: the question, the editable target, and the buttons — or the
/// overwrite prompt when that target has items in the way.
fn draw_transfer_dialog(
    frame: &mut Frame,
    classic_style: bool,
    transfer: &Transfer,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let field = text_field_line(&transfer.field, transfer.editing, frame.area().width, &pal);
    let (primary_focused, cancel_focused) = focused_buttons(transfer.editing, cancel_focused);
    if let Some(overwrite) = &transfer.overwrite {
        let comparison = match overwrite.conflicts.as_slice() {
            [name] => overwrite
                .items
                .iter()
                .find(|(item, _)| item == name)
                .and_then(|(_, src)| {
                    entry_summary(src).zip(entry_summary(&overwrite.dest_dir.join(name)))
                }),
            _ => None,
        };
        draw_overwrite_dialog(
            frame,
            classic_style,
            &overwrite.conflicts,
            field,
            comparison,
            button_row(
                &[("Overwrite", primary_focused), ("Cancel", cancel_focused)],
                &pal,
            ),
        );
        return;
    }
    let verb = transfer.kind.verb();
    let message = format!("{verb} {} to", item_summary(&transfer.items));
    let sections = vec![
        vec![DialogLine::Text(Line::from(Span::styled(
            message,
            Style::default().fg(pal.fg),
        )))],
        vec![field],
        vec![button_row(
            &[(verb, primary_focused), ("Cancel", cancel_focused)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, verb, sections, true);
}

/// Size and modified time of one side of an overwrite, as the listing shows
/// them.
struct EntrySummary {
    size: String,
    modified: Option<std::time::SystemTime>,
}

fn entry_summary(path: &std::path::Path) -> Option<EntrySummary> {
    let meta = std::fs::metadata(path)
        .or_else(|_| std::fs::symlink_metadata(path))
        .ok()?;
    let size = if meta.is_dir() {
        "<DIR>".to_string()
    } else {
        format_size(meta.len())
    };
    Some(EntrySummary {
        size,
        modified: meta.modified().ok(),
    })
}

/// How many conflicting names an overwrite prompt lists before summing up
/// the rest, so a big batch doesn't grow the dialog past the screen.
const OVERWRITE_LIST_LIMIT: usize = 3;

/// Copy/Move onto items that already exist: names what's in the way and,
/// for a single item, puts both versions' size and date side by side with
/// the newer one marked, so it's clear what would be replaced. The target
/// stays editable, for going elsewhere instead.
fn draw_overwrite_dialog(
    frame: &mut Frame,
    classic_style: bool,
    conflicts: &[String],
    field: DialogLine<'static>,
    comparison: Option<(EntrySummary, EntrySummary)>,
    buttons: DialogLine<'static>,
) {
    let pal = dialog_palette(classic_style);
    let text =
        |s: String| DialogLine::Text(Line::from(Span::styled(s, Style::default().fg(pal.fg))));

    let message = match conflicts {
        [name] => format!("\"{name}\" already exists: overwrite, or edit the path"),
        _ => format!(
            "{} items already exist: overwrite, or edit the path",
            conflicts.len()
        ),
    };
    let mut sections = vec![vec![text(message)], vec![field]];

    let details = match comparison {
        Some((new, existing)) => {
            let is_newer = |a: &EntrySummary, b: &EntrySummary| matches!((a.modified, b.modified), (Some(a), Some(b)) if a > b);
            let row = |label: &str, entry: &EntrySummary, newer: bool| {
                text(format!(
                    "{label} {:>12}  {}{}",
                    entry.size,
                    format_modified(entry.modified),
                    if newer { "  (newer)" } else { "" }
                ))
            };
            vec![
                row("New:     ", &new, is_newer(&new, &existing)),
                row("Existing:", &existing, is_newer(&existing, &new)),
            ]
        }
        None => {
            let mut lines: Vec<DialogLine> = conflicts
                .iter()
                .take(OVERWRITE_LIST_LIMIT)
                .map(|name| text(name.clone()))
                .collect();
            if conflicts.len() > OVERWRITE_LIST_LIMIT {
                lines.push(text(format!(
                    "…and {} more",
                    conflicts.len() - OVERWRITE_LIST_LIMIT
                )));
            }
            lines
        }
    };
    sections.push(details);

    sections.push(vec![buttons]);

    draw_dialog_frame(frame, classic_style, "Overwrite", sections, true);
}

fn draw_quit_dialog(
    frame: &mut Frame,
    classic_style: bool,
    job: Option<&Job>,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let mut message = vec![DialogLine::Text(Line::from(Span::styled(
        "Quit Peter Commander?",
        Style::default().fg(pal.fg),
    )))];
    if let Some(job) = job {
        let warning = if job.kind == DialogKind::TrashByCopy {
            "A move to trash is running: it will stop after the current item.".to_string()
        } else {
            format!(
                "A {} is still running and will be cancelled.",
                job.kind.verb().to_lowercase()
            )
        };
        message.push(DialogLine::Text(Line::from(Span::styled(
            warning,
            Style::default().fg(pal.fg).add_modifier(Modifier::BOLD),
        ))));
    }
    let sections = vec![
        message,
        vec![button_row(
            &[("Quit", !cancel_focused), ("Cancel", cancel_focused)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, "Quit", sections, false);
}

/// Saving onto a file that's there already: replace it?
fn draw_save_over_dialog(frame: &mut Frame, classic_style: bool, name: &str, cancel: bool) {
    let pal = dialog_palette(classic_style);
    let sections = vec![
        vec![DialogLine::Text(Line::from(Span::styled(
            format!("\"{name}\" already exists. Replace it?"),
            Style::default().fg(pal.fg),
        )))],
        vec![button_row(
            &[("Replace", !cancel), ("Cancel", cancel)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, "Save As", sections, false);
}

/// Closing the editor (or quitting) with unsaved changes, as Notepad asks.
fn draw_unsaved_dialog(frame: &mut Frame, classic_style: bool, name: &str, focused: usize) {
    let pal = dialog_palette(classic_style);
    let sections = vec![
        vec![DialogLine::Text(Line::from(Span::styled(
            format!("Save changes to \"{name}\"?"),
            Style::default().fg(pal.fg),
        )))],
        vec![button_row(
            &[
                ("Save", focused == 0),
                ("Don't Save", focused == 1),
                ("Cancel", focused == 2),
            ],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, "Unsaved Changes", sections, false);
}

/// How many `old → new` lines the bulk-rename prompt shows before summing
/// up the rest.
const BULK_RENAME_LIST_LIMIT: usize = 5;

/// Rename Selected confirmation: a sample of the renames and, when some new
/// names are already taken by files outside the batch, a warning listing
/// what would be replaced — the primary button then reads "Overwrite".
fn draw_rename_selected_dialog(
    frame: &mut Frame,
    classic_style: bool,
    dir: &str,
    plan: &Plan,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let text =
        |s: String| DialogLine::Text(Line::from(Span::styled(s, Style::default().fg(pal.fg))));
    let bold = |s: String| {
        DialogLine::Text(Line::from(Span::styled(
            s,
            Style::default().fg(pal.fg).add_modifier(Modifier::BOLD),
        )))
    };
    let summarize = |names: Vec<String>, limit: usize| {
        let total = names.len();
        let mut lines: Vec<DialogLine> = names.into_iter().take(limit).map(text).collect();
        if total > limit {
            lines.push(text(format!("…and {} more", total - limit)));
        }
        lines
    };

    let mut sections = vec![
        vec![text(format!("Rename {} item(s) in", plan.renames.len()))],
        vec![DialogLine::Field {
            content: vec![Span::styled(
                dir.to_string(),
                Style::default().bg(pal.field_bg).fg(pal.field_fg),
            )],
            fill_bg: pal.field_bg,
        }],
        summarize(
            plan.renames
                .iter()
                .map(|(old, new)| format!("{old} \u{2192} {new}"))
                .collect(),
            BULK_RENAME_LIST_LIMIT,
        ),
    ];
    if !plan.overwrites.is_empty() {
        let mut warning = vec![bold(format!(
            "WARNING: {} existing item(s) will be overwritten:",
            plan.overwrites.len()
        ))];
        warning.extend(summarize(plan.overwrites.clone(), OVERWRITE_LIST_LIMIT));
        sections.push(warning);
    }
    let primary = if plan.overwrites.is_empty() {
        "Rename"
    } else {
        "Overwrite"
    };
    sections.push(vec![button_row(
        &[(primary, !cancel_focused), ("Cancel", cancel_focused)],
        &pal,
    )]);
    draw_dialog_frame(frame, classic_style, "Rename Selected", sections, true);
}

/// Width of the progress window's bar, in cells.
const PROGRESS_BAR_WIDTH: usize = 40;

/// The Copy/Move progress window: which item, which file, a bar with the
/// percentage and byte counts, and [ Background ] [ Cancel ].
fn draw_job_dialog(frame: &mut Frame, app: &App, job: &Job) {
    let classic_style = app.classic_style;
    let cancel_focused = app.job_cancel_focused;
    let pal = dialog_palette(classic_style);
    let text =
        |s: String| DialogLine::Text(Line::from(Span::styled(s, Style::default().fg(pal.fg))));
    let progress = job.progress();
    let verb = job.kind.verb();
    let total_items = job.items.len();
    let trash = job.kind == DialogKind::TrashByCopy;

    let status = if job.is_cancelling() && trash {
        // The crate can't be stopped part-way through an item.
        let then = if app.quit_when_job_done {
            ", then quitting"
        } else {
            ""
        };
        format!("Stopping — finishing the current item first{then}…")
    } else if job.is_cancelling() {
        "Cancelling…".to_string()
    } else if progress.scanning {
        "Counting files…".to_string()
    } else if trash {
        format!(
            "Copying item {} of {total_items} into the trash at",
            progress.item.max(1)
        )
    } else {
        format!(
            "{} item {} of {total_items} to",
            if verb == "Move" { "Moving" } else { "Copying" },
            progress.item.max(1)
        )
    };
    let elapsed = job.started.elapsed().as_secs();
    let elapsed = format!("{}:{:02}", elapsed / 60, elapsed % 60);
    let fraction = job_fraction(&progress);
    // Floored, so it never claims 100% while anything is left.
    let filled = (fraction * PROGRESS_BAR_WIDTH as f64) as usize;
    let bar = format!(
        "{}{} {:>3}%",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(PROGRESS_BAR_WIDTH - filled),
        (fraction * 100.0) as u32
    );

    let sections = vec![
        vec![text(status)],
        vec![DialogLine::Field {
            content: vec![Span::styled(
                job.dest_dir.display().to_string(),
                Style::default().bg(pal.field_bg).fg(pal.field_fg),
            )],
            fill_bg: pal.field_bg,
        }],
        vec![
            text(
                fit_name(&progress.current, PROGRESS_BAR_WIDTH + 5)
                    .trim_end()
                    .to_string(),
            ),
            text(bar),
            text(format!(
                "{} of {}   {} of {} files   {elapsed}",
                human_bytes(progress.bytes_done),
                human_bytes(progress.bytes_total),
                progress.files_done,
                progress.files_total
            )),
        ],
        vec![button_row(
            &[("Background", !cancel_focused), ("Cancel", cancel_focused)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, verb, sections, true);
}

/// How far along a job is, 0.0–1.0: by bytes (an item that can't report
/// its own progress counting as half done — see `Progress::opaque_bytes`),
/// falling back to the file count when there are no bytes to speak of
/// (only empty files/symlinks),
/// and held just under 100% while any file is still left — so one big file
/// plus thousands of tiny ones doesn't claim to be done early.
fn job_fraction(progress: &crate::job::Progress) -> f64 {
    let ratio = |done: u64, total: u64| (done as f64 / total.max(1) as f64).min(1.0);
    let fraction = if progress.bytes_total > 0 {
        ratio(
            progress.bytes_done + progress.opaque_bytes / 2,
            progress.bytes_total,
        )
    } else {
        ratio(progress.files_done, progress.files_total)
    };
    if progress.files_done < progress.files_total {
        fraction.min(0.99)
    } else {
        fraction
    }
}

/// Short human-readable size for progress readouts, e.g. "1.5 GB".
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Rename and MkDir/New file: a label line, the editable field and a button
/// row.
fn draw_input_dialog(
    frame: &mut Frame,
    classic_style: bool,
    title: &str,
    prompt: &str,
    field: &TextField,
    editing: bool,
    cancel_focused: bool,
) {
    let pal = dialog_palette(classic_style);
    let (ok_focused, cancel_focused) = focused_buttons(editing, cancel_focused);
    let sections = vec![
        vec![DialogLine::Text(Line::from(Span::styled(
            prompt.to_string(),
            Style::default().fg(pal.fg),
        )))],
        vec![text_field_line(field, editing, frame.area().width, &pal)],
        vec![button_row(
            &[("OK", ok_focused), ("Cancel", cancel_focused)],
            &pal,
        )],
    ];
    draw_dialog_frame(frame, classic_style, title, sections, true);
}

const SETTINGS_HINT: &str =
    "\u{2191}/\u{2193} move   \u{2190}/\u{2192}/Space toggle   Enter save   Esc cancel";

/// Each setting is shown as a two-way switch — "left label [ ]----[x] right
/// label" — rather than a single generic checkbox, so both what's on and
/// what's off are named in positive language instead of one hard-to-phrase
/// boolean. The left column is padded to the widest left label so the
/// switches themselves line up in a column; the selected row is a full-width
/// highlighted bar, like the fields in the other dialogs.
const SWITCH_TRACK: &str = "----";

fn draw_settings_dialog(frame: &mut Frame, app: &App, selected: usize) {
    let pal = dialog_palette(app.classic_style);
    let left_col_width = SettingItem::ALL
        .iter()
        .map(|item| item.sides()[0].0.chars().count())
        .max()
        .unwrap_or(0);

    let right_col_width = SettingItem::ALL
        .iter()
        .map(|item| item.sides()[1].0.chars().count())
        .max()
        .unwrap_or(0);

    // Every row is a full-width field — highlighted for the selected one,
    // dialog-colored otherwise — so all rows measure the same and moving
    // the selection never resizes the box.
    let mut rows = vec![DialogLine::Text(Line::from(""))];
    rows.extend(SettingItem::ALL.iter().enumerate().map(|(idx, item)| {
        let [(left_label, left_value), (right_label, _)] = item.sides();
        let (left_box, right_box) = if app.setting_value(*item) == left_value {
            ("[x]", "[ ]")
        } else {
            ("[ ]", "[x]")
        };
        let text = format!(
            " {left_label:<left_col_width$} {left_box}{SWITCH_TRACK}{right_box} {right_label:<right_col_width$} ",
        );
        let (style, fill_bg) = if idx == selected {
            (
                Style::default()
                    .bg(pal.field_bg)
                    .fg(pal.field_fg)
                    .add_modifier(Modifier::BOLD),
                pal.field_bg,
            )
        } else {
            (Style::default().bg(pal.bg).fg(pal.fg), pal.bg)
        };
        DialogLine::Field {
            content: vec![Span::styled(text, style)],
            fill_bg,
        }
    }));
    rows.push(DialogLine::Text(Line::from("")));

    let sections = vec![
        rows,
        vec![DialogLine::Centered(Line::from(Span::styled(
            SETTINGS_HINT,
            Style::default().fg(pal.fg),
        )))],
        vec![button_row(&[("Save", true), ("Cancel", false)], &pal)],
    ];
    draw_dialog_frame(frame, app.classic_style, "Settings", sections, false);
}

fn draw_menu_bar(frame: &mut Frame, area: Rect, app: &mut App) {
    app.menu_bar_tiles.clear();
    let mut x = area.x + 1; // the leading raw space
    let mut spans = Vec::new();
    spans.push(Span::raw(" "));
    for (idx, category) in app.menu_bar().iter().enumerate() {
        let is_selected = app.menu_open && idx == app.menu_category;
        let style = match (app.classic_style, is_selected) {
            (true, true) => Style::default()
                .fg(classic::MENU_BAR_SELECTED_FG)
                .bg(classic::MENU_BAR_SELECTED_BG)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default()
                .fg(classic::MENU_BAR_FG)
                .bg(classic::MENU_BAR_BG),
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

    // A job sent to the background keeps a small readout at the right end
    // of the menu bar (Command > Show Progress, or Ctrl+B, brings the window back).
    if let Some(job) = &app.job
        && !app.job_visible
    {
        let percent = (job_fraction(&job.progress()) * 100.0) as u32;
        let label = format!(" {} {percent}%  Ctrl+B ", job.kind.verb());
        let width = (label.chars().count() as u16).min(area.width);
        let style = if app.classic_style {
            Style::default()
                .fg(classic::MENU_BAR_SELECTED_FG)
                .bg(classic::MENU_BAR_SELECTED_BG)
        } else {
            Style::default().fg(Color::White).bg(Color::Blue)
        };
        frame.render_widget(
            Paragraph::new(label).style(style),
            Rect {
                x: area.x + area.width - width,
                width,
                ..area
            },
        );
    }
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
    let menu_bar = app.menu_bar();
    for category in &menu_bar[..app.menu_category] {
        x += category.title.len() as u16 + 2;
    }

    let category = &menu_bar[app.menu_category];
    let row_text_width = category
        .items
        .iter()
        .map(|action| {
            let shortcut_width = app.menu_shortcut(*action).map_or(0, |s| s.len() + 3);
            app.menu_label(*action).chars().count() + shortcut_width
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
            if *action == Action::Separator {
                return ListItem::new(Line::from(Span::styled(
                    "\u{2500}".repeat(area.width.saturating_sub(2) as usize),
                    menu_border_style(app.classic_style),
                )));
            }
            let is_selected = idx == app.menu_item;
            let enabled = app.action_enabled(*action);
            let style = match (app.classic_style, is_selected, enabled) {
                (true, true, _) => Style::default()
                    .fg(classic::MENU_SELECTED_ITEM_FG)
                    .bg(classic::MENU_SELECTED_ITEM_BG)
                    .add_modifier(Modifier::BOLD),
                (true, false, true) => Style::default()
                    .fg(classic::MENU_ITEM_FG)
                    .bg(classic::MENU_BG),
                (true, false, false) => Style::default()
                    .fg(classic::MENU_DISABLED_FG)
                    .bg(classic::MENU_BG),
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
                Style::default()
                    .fg(Color::Rgb(255, 255, 255))
                    .bg(classic::MENU_BG)
            } else {
                style
            };
            let label = app.menu_label(*action);
            let (label_part, shortcut_part) = match app.menu_shortcut(*action) {
                Some(shortcut) => (
                    format!(
                        "{label:<label_width$}",
                        label_width = row_text_width - shortcut.len()
                    ),
                    format!(
                        "{shortcut:>shortcut_width$}",
                        shortcut_width = shortcut.len()
                    ),
                ),
                None => (format!("{label:<row_text_width$}"), String::new()),
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
            .border_style(
                Style::default()
                    .fg(classic::MENU_BORDER_FG)
                    .add_modifier(Modifier::BOLD),
            )
            .style(
                Style::default()
                    .bg(classic::MENU_BG)
                    .fg(classic::MENU_ITEM_FG),
            )
    } else {
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Gray).fg(Color::Black))
    };

    draw_shadow_for(frame, area);

    frame.render_widget(Clear, area);
    frame.render_widget(List::new(items).block(block), area);

    // Tee each separator into the dropdown's border, like the dividers in
    // the dialogs.
    let border_style = menu_border_style(app.classic_style);
    let buf = frame.buffer_mut();
    for (idx, action) in category.items.iter().enumerate() {
        if *action != Action::Separator {
            continue;
        }
        let y = area.y + 1 + idx as u16;
        if let Some(cell) = buf.cell_mut((area.x, y)) {
            cell.set_symbol("\u{251c}").set_style(border_style);
        }
        if let Some(cell) = buf.cell_mut((area.x + area.width.saturating_sub(1), y)) {
            cell.set_symbol("\u{2524}").set_style(border_style);
        }
    }
}

/// The pulldown's border (and separator) colors, matching `block` in
/// `draw_menu_dropdown`.
fn menu_border_style(classic_style: bool) -> Style {
    if classic_style {
        Style::default()
            .fg(classic::MENU_BORDER_FG)
            .bg(classic::MENU_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Black).bg(Color::Gray)
    }
}

/// Widths of the date ("20-09-26") and time ("15:57") halves of a
/// formatted modified time, which `format_modified` joins with one space —
/// shown as separate Date and Time columns when column headers are on.
const DATE_PART_WIDTH: usize = 8;
const TIME_PART_WIDTH: usize = 5;
/// Width of a formatted date/time like "20-09-26 15:57".
const DATE_COLUMN_WIDTH: usize = DATE_PART_WIDTH + 1 + TIME_PART_WIDTH;
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
    if let Some(query) = &app.quick_search {
        spans.push(Span::styled(
            "Quick search: ",
            Style::default().fg(cwd_fg).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            query.as_str(),
            Style::default().fg(typed_text_fg),
        ));
        spans.push(Span::styled(
            "_",
            Style::default()
                .fg(typed_text_fg)
                .add_modifier(Modifier::SLOW_BLINK),
        ));
        spans.push(Span::styled(
            "   (\u{2191}/\u{2193} next match, Esc/Enter done)",
            Style::default().fg(Color::DarkGray),
        ));
    } else {
        spans.push(Span::styled(
            format!("{}> ", cwd.display()),
            Style::default().fg(cwd_fg).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            &app.command_line,
            Style::default().fg(typed_text_fg),
        ));
    }
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

/// Insert-tagged entries render in this color in both themes, so marking
/// stays visible regardless of the active palette — including on the
/// cursor row itself, where it overrides the highlight's own fg.
const MARKED_FG: Color = Color::Rgb(0xFF, 0xFF, 0x50);

/// The pane's sort indicator ("[Size↓]") — the same fixed bright yellow,
/// so it stands out from the border in either palette.
const SORT_INDICATOR_FG: Color = MARKED_FG;

/// How file panes look: the user's display settings that both panes share,
/// read from `App` once per frame.
#[derive(Clone, Copy)]
struct PaneLook {
    classic_style: bool,
    /// The file/dir count (or marked-files summary) on the bottom border.
    show_totals: bool,
    /// The Name/Size/Date/Time column headers.
    show_headers: bool,
}

impl PaneLook {
    fn of(app: &App) -> Self {
        Self {
            classic_style: app.classic_style,
            show_totals: app.pane_totals,
            show_headers: app.column_headers,
        }
    }
}

fn draw_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &Pane,
    is_active: bool,
    list_state: &mut ListState,
    look: PaneLook,
) {
    let PaneLook {
        classic_style,
        show_totals,
        show_headers,
    } = look;
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

    // The title is just the pane's own directory — it used to fold in the
    // selected entry's name too, which meant it changed on every Up/Down
    // press. That per-selection detail now lives in the status footer
    // below instead, where it belongs with the rest of the file info.
    let title = pane.cwd.to_string_lossy().to_string();

    // -2 for the pane's own left/right border.
    let name_width = name_column_width(area.width.saturating_sub(2));

    let items: Vec<ListItem> = pane
        .entries
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let is_marked = pane.marked.contains(&entry.name);
            let fg = if is_marked {
                MARKED_FG
            } else if entry.is_dir {
                if classic_style {
                    classic::DIR_FG
                } else {
                    Color::Cyan
                }
            } else if entry.is_executable {
                // Classic mode treats executables exactly like directories
                // (same white, bold) rather than a distinct green — the
                // size/date columns already tell them apart.
                if classic_style {
                    classic::DIR_FG
                } else {
                    Color::Green
                }
            } else if classic_style {
                classic::FILE_FG
            } else {
                // Not White: this renders straight onto the terminal's own
                // background with no contrasting box behind it, so it must
                // follow the terminal's default foreground instead of
                // assuming a dark theme (a hardcoded white was invisible on
                // light-background terminals).
                Color::Reset
            };
            let bold = is_marked || entry.is_dir || entry.is_executable;

            // The cursor row's highlight is baked in per-item (instead of
            // going through `List::highlight_style`, which would apply one
            // fixed fg to every row) so a marked entry keeps its yellow text
            // instead of being swallowed by the highlight's own fg when
            // it's also under the cursor.
            let style = if is_active && idx == pane.selected {
                let bg = if classic_style {
                    classic::HIGHLIGHT_BG
                } else {
                    Color::Blue
                };
                let cursor_fg = if is_marked {
                    fg
                } else if classic_style {
                    classic::HIGHLIGHT_FG
                } else {
                    Color::White
                };
                Style::default()
                    .bg(bg)
                    .fg(cursor_fg)
                    .add_modifier(Modifier::BOLD)
            } else {
                let mut style = Style::default().fg(fg);
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                style
            };

            let date = format_modified(entry.modified);
            let size_label = if entry.name == ".." {
                String::new()
            } else if entry.is_dir {
                "<DIR>".to_string()
            } else {
                format_size(entry.size)
            };
            let name = fit_name(&entry.name, name_width);
            // Padded even when empty (".." has no date), so every row — and
            // the cursor bar on it — spans the full width.
            let label =
                format!("{name} {size_label:>SIZE_COLUMN_WIDTH$} {date:<DATE_COLUMN_WIDTH$} ");
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();

    let mut block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);
    // With column headers the sort arrow sits on its column's header;
    // without, on the border — right-aligned so a long path can't push it
    // out of sight.
    if !show_headers {
        block = block.title_top(
            Line::from(vec![
                Span::styled("[", border_style),
                Span::styled(
                    pane.sort_indicator(),
                    Style::default()
                        .fg(SORT_INDICATOR_FG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("]", border_style),
            ])
            .right_aligned(),
        );
    }
    if classic_style {
        block = block.style(Style::default().bg(classic::BG));
    }

    let status_text = show_totals.then(|| match pane.marked_summary() {
        Some((count, bytes)) => format!(
            "{} bytes in {count} selected file{}",
            with_thousands_separators(bytes),
            if count == 1 { "" } else { "s" }
        ),
        // Nothing marked: show the cursor's entry — name, size, date —
        // instead, now that the title above no longer does. Falls back to
        // plain totals for "..", which has none of those to show.
        // Lined up under the listing's own Name/Size/Date columns (same
        // widths as each row above), instead of just running the three
        // together against the left edge.
        None => match pane.selected_entry() {
            Some(entry) if entry.name != ".." => {
                let size = if entry.is_dir {
                    "<DIR>".to_string()
                } else {
                    format_size(entry.size)
                };
                let date = format_modified(entry.modified);
                let name = fit_name(&entry.name, name_width);
                format!("{name} {size:>SIZE_COLUMN_WIDTH$} {date}")
            }
            _ => {
                let (files, dirs) = pane.totals();
                format!("{files} files, {dirs} dirs")
            }
        },
    });

    // A real last row (plus a divider above it) inside the border, instead
    // of the old `title_bottom` approach that wrote the status straight
    // onto the border line itself — that crowded the last listed entry
    // right up against it, unlike the top, where the ".." entry already
    // gives the path title some breathing room below it.
    let mut inner = block.inner(area);
    let header_area = (show_headers && inner.height >= 1).then(|| {
        let header = Rect { height: 1, ..inner };
        inner.y += 1;
        inner.height -= 1;
        header
    });
    let footer_rows = if inner.height >= 2 { 2 } else { 0 };
    let (list_area, show_footer) = match &status_text {
        Some(_) if footer_rows > 0 => (
            Rect {
                height: inner.height - footer_rows,
                ..inner
            },
            true,
        ),
        _ => (inner, false),
    };

    frame.render_widget(block, area);
    if let Some(header_area) = header_area {
        frame.render_widget(
            Paragraph::new(column_header_line(pane, name_width)),
            header_area,
        );
    }

    // The cursor row's highlight is already baked into its `ListItem`
    // style above (so a marked entry can keep its own fg there), so `List`
    // itself doesn't need a `highlight_style` patched on top.
    let list = List::new(items);

    // Always keep the selection tracked (not just while active) so the
    // persisted `list_state`'s scroll offset stays correct for this pane
    // even while the other pane has focus; the `is_active` check above is
    // what actually hides the highlight when inactive.
    list_state.select(Some(pane.selected));
    // `List` only scrolls to keep the selection in view, so a listing that
    // got shorter (a deleted file) or a taller window would leave blank rows
    // under the last entry while others sit scrolled off the top.
    let max_offset = list.len().saturating_sub(list_area.height as usize);
    *list_state.offset_mut() = list_state.offset().min(max_offset);

    frame.render_stateful_widget(list, list_area, list_state);

    if show_footer {
        let divider_y = list_area.y + list_area.height;
        let status_y = divider_y + 1;
        frame.render_widget(
            Paragraph::new("─".repeat(inner.width as usize)).style(border_style),
            Rect {
                x: inner.x,
                y: divider_y,
                width: inner.width,
                height: 1,
            },
        );
        let is_marked_summary = pane.marked_summary().is_some();
        let status_style = if is_marked_summary {
            Style::default().fg(MARKED_FG).add_modifier(Modifier::BOLD)
        } else {
            border_style
        };
        // Centered for the marked-files summary (a standalone sentence);
        // the per-entry line stays left-aligned so it lines up with the
        // listing's own Name/Size/Date columns above it.
        let status_paragraph = Paragraph::new(status_text.unwrap_or_default())
            .style(status_style)
            .alignment(if is_marked_summary {
                Alignment::Center
            } else {
                Alignment::Left
            });
        frame.render_widget(
            status_paragraph,
            Rect {
                x: inner.x,
                y: status_y,
                width: inner.width,
                height: 1,
            },
        );

        // Tee the divider into the pane's own left/right border, matching
        // the dialogs' section separators.
        let buf = frame.buffer_mut();
        if let Some(cell) = buf.cell_mut((area.x, divider_y)) {
            cell.set_symbol("├").set_style(border_style);
        }
        if let Some(cell) = buf.cell_mut((area.x + area.width.saturating_sub(1), divider_y)) {
            cell.set_symbol("┤").set_style(border_style);
        }
    }

    if show_headers {
        draw_column_separators(
            frame,
            area,
            list_area,
            header_area,
            name_width,
            border_style,
        );
    }
}

/// The full-view NC look that goes with the column headers: vertical lines
/// in the spaces between Name, Size, Date and Time, from the header row down to
/// the bottom, teed into the top border (`┬`) and into the status divider
/// or bottom border (`┴`). Drawn over the already-rendered rows, keeping
/// each cell's colors (the cursor bar runs through them, as in NC).
fn draw_column_separators(
    frame: &mut Frame,
    area: Rect,
    list_area: Rect,
    header_area: Option<Rect>,
    name_width: usize,
    border_style: Style,
) {
    let top = header_area.map_or(list_area.y, |header| header.y);
    // The row just below the list: the status divider, or the bottom border
    // when there's no status line — `┴` either way.
    let bottom = list_area.y + list_area.height;
    let border_fg = border_style.fg.unwrap_or(Color::Reset);
    let right_edge = area.x + area.width.saturating_sub(1);
    let columns = [
        list_area.x + name_width as u16,
        list_area.x + (name_width + 1 + SIZE_COLUMN_WIDTH) as u16,
        list_area.x + (name_width + 1 + SIZE_COLUMN_WIDTH + 1 + DATE_PART_WIDTH) as u16,
    ];

    let buf = frame.buffer_mut();
    for x in columns.into_iter().filter(|&x| x < right_edge) {
        for y in top..bottom {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol("│").set_fg(border_fg);
            }
        }
        // Only where the border is plain line, so a long path title on the
        // top border is never cut into.
        if let Some(cell) = buf.cell_mut((x, area.y))
            && cell.symbol() == "─"
        {
            cell.set_symbol("┬");
        }
        if let Some(cell) = buf.cell_mut((x, bottom))
            && cell.symbol() == "─"
        {
            cell.set_symbol("┴");
        }
    }
}

/// The classic NC header row: "Name", "Size", "Date" and "Time" in yellow,
/// each centered over its column (same widths as the rows below), the
/// sorted column followed by the sort arrow — sorting by type marks the Name
/// column, since the extension is part of the name, and sorting by date
/// (date and time together) marks Date.
fn column_header_line(pane: &Pane, name_width: usize) -> Line<'static> {
    let sorted_column = match pane.sort_key {
        SortKey::Name | SortKey::Extension => 0,
        SortKey::Size => 1,
        SortKey::Modified => 2,
    };
    let columns = [
        ("Name", name_width),
        ("Size", SIZE_COLUMN_WIDTH),
        ("Date", DATE_PART_WIDTH),
        ("Time", TIME_PART_WIDTH),
    ];
    let mut spans = Vec::new();
    for (idx, (label, width)) in columns.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" "));
        }
        let text = if idx == sorted_column {
            let arrow = if pane.sort_descending() {
                '\u{2193}'
            } else {
                '\u{2191}'
            };
            format!("{} {arrow}", pane.sort_key.label())
        } else {
            label.to_string()
        };
        spans.push(Span::styled(
            format!("{text:^width$}"),
            Style::default().fg(SORT_INDICATOR_FG),
        ));
    }
    Line::from(spans)
}

/// How the quick-view pane is to be drawn. `focused` is whether Tab has moved
/// scroll focus onto it; `scroll` is how many lines of a text preview are
/// skipped from the top; `covered` is whether something is open over it.
#[derive(Clone, Copy)]
struct PreviewLook {
    focused: bool,
    scroll: usize,
    /// Sideways, in cells, for text that isn't wrapped.
    scroll_x: usize,
    /// Wrap Lines, shared with the editor.
    wrap: bool,
    classic_style: bool,
    covered: bool,
}

impl PreviewLook {
    fn of(app: &App, covered: bool) -> Self {
        PreviewLook {
            focused: app.preview_focus,
            scroll: app.preview_scroll,
            scroll_x: app.preview_scroll_x,
            wrap: app.wrap_lines,
            classic_style: app.classic_style,
            covered,
        }
    }
}

/// Quick-view: renders a live preview of `source`'s selected entry, in
/// place of the opposite pane's own listing (Ctrl+Q).
fn draw_preview_pane(
    frame: &mut Frame,
    area: Rect,
    source: &Pane,
    look: PreviewLook,
    images: &mut ImagePreview,
    find: Option<&crate::app::PreviewFind>,
) {
    let PreviewLook {
        focused,
        scroll,
        scroll_x,
        wrap,
        classic_style,
        covered,
    } = look;
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

    let mut block = titled_with_label(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style),
        &title,
        "Preview",
        area.width,
    );
    if classic_style {
        block = block.style(Style::default().bg(classic::BG).fg(classic::FILE_FG));
    }

    if images.is_detected()
        && let Some(entry) = source.selected_entry()
        && !entry.is_dir
        && preview::is_image(&entry.name)
        && let Some(path) = source.preview_target()
    {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if !covered {
            draw_preview_image(frame, inner, images, &path);
        }
        return;
    }

    // The scroll offset is in screen rows: whole entries for a listing,
    // and for text the rows it's laid out in — the editor's own, so a file
    // looks the same in F3 as in F4 (wrapped where it wraps, or cut at the
    // edge and scrolled sideways, tabs and odd characters drawn alike).
    let text_style = if classic_style {
        Style::default().fg(classic::FILE_FG)
    } else {
        Style::default().fg(Color::Reset)
    };
    let mut inner = block.inner(area);
    // The find bar, if open, takes the last row (searching this file).
    let find = find.filter(|find| source.preview_target().as_ref() == Some(&find.target));
    let bar_area = find
        .filter(|find| find.text.find.is_some() && inner.height > 1)
        .map(|_| {
            inner.height -= 1;
            Rect {
                y: inner.y + inner.height,
                height: 1,
                ..inner
            }
        });
    let editor_look = EditorLook::of(classic_style);
    let mut is_text = false;
    let lines: Vec<Line> = match preview::build_preview(source) {
        PreviewContent::Empty => Vec::new(),
        PreviewContent::Directory(entries) => entries
            .into_iter()
            .skip(scroll)
            .map(|entry| {
                let style = if entry.is_dir {
                    let fg = if classic_style {
                        classic::DIR_FG
                    } else {
                        Color::Cyan
                    };
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
            is_text = true;
            let cols = inner.width as usize;
            let height = inner.height as usize;
            let wrap_at = wrap.then(|| editor::wrap_width_for(cols));
            let scroll_x = if wrap { 0 } else { scroll_x };
            let mut rows = Vec::with_capacity(height);
            let mut skip = scroll;
            // As in the editor: the current match is the selection.
            let selection = find.and_then(|find| find.text.selection());
            let find_style = |line: usize, col: usize| {
                let pos = Pos::new(line, col);
                if selection.is_some_and(|(start, end)| start <= pos && pos < end) {
                    return editor_look.selected;
                }
                let found = find
                    .and_then(|find| find.text.find.as_ref())
                    .is_some_and(|bar| {
                        bar.matches_on(line)
                            .iter()
                            .any(|(start, end)| start.col <= col && col < end.col)
                    });
                if found { editor_look.found } else { text_style }
            };
            for (line_idx, line) in text_lines.iter().enumerate() {
                if rows.len() == height {
                    break;
                }
                // Unwrapped, a line is one row: no need to lay it out to
                // pass it by.
                if wrap_at.is_none() && skip > 0 {
                    skip -= 1;
                    continue;
                }
                let layout = editor::LineLayout::new(line, wrap_at);
                let count = layout.starts.len();
                if skip >= count {
                    skip -= count;
                    continue;
                }
                for idx in skip..count {
                    if rows.len() == height {
                        break;
                    }
                    let end = layout
                        .starts
                        .get(idx + 1)
                        .copied()
                        .unwrap_or(layout.chars.len());
                    rows.push(text_row_line(
                        &layout,
                        (layout.starts[idx], end),
                        scroll_x,
                        cols,
                        |col| find_style(line_idx, col),
                        None,
                    ));
                }
                skip = 0;
            }
            rows
        }
    };

    // The text inside, the frame round it: the find bar's row is the
    // frame's to keep clear.
    frame.render_widget(block, area);
    let mut paragraph = Paragraph::new(lines);
    // Listings and notes may wrap as they always did; text is already laid
    // out to fit.
    if !is_text {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(paragraph, inner);
    if let (Some(bar_area), Some(find)) = (bar_area, find)
        && let Some(bar) = &find.text.find
    {
        let note = if find.truncated { " (first 1 MB)" } else { "" };
        let cursor = draw_find_bar(frame, bar_area, bar, note, classic_style, editor_look);
        if focused
            && !covered
            && let Some(cursor) = cursor
        {
            frame.set_cursor_position(cursor);
        }
    }
}

/// The picture centered in `area`, or a line saying why it isn't there.
fn draw_preview_image(
    frame: &mut Frame,
    area: Rect,
    images: &mut ImagePreview,
    path: &std::path::Path,
) {
    let size = ratatui::layout::Size::new(area.width, area.height);
    let note = |text: String, color: Color| {
        Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color))))
            .wrap(Wrap { trim: false })
    };
    match images.state_for(path, size) {
        ImageState::Ready(picture) => {
            let drawn = picture.size();
            let centered = Rect {
                x: area.x + area.width.saturating_sub(drawn.width) / 2,
                y: area.y + area.height.saturating_sub(drawn.height) / 2,
                width: drawn.width.min(area.width),
                height: drawn.height.min(area.height),
            };
            frame.render_widget(Image::new(picture), centered);
        }
        ImageState::Loading => {
            frame.render_widget(note("Loading image...".to_string(), Color::DarkGray), area);
        }
        ImageState::Failed(err) => {
            frame.render_widget(note(format!("Cannot preview: {err}"), Color::Red), area);
        }
    }
}

/// The editor's selection — the find bar's current match included, which is
/// selected — in both palettes: the marked-files yellow, under dark text.
const SELECTION_BG: Color = MARKED_FG;

/// The find bar's other matches: turquoise, under dark text, so they can't
/// be taken for the selection.
const FIND_MATCH_BG: Color = Color::Rgb(0x55, 0xdd, 0xdd);

/// The colors the editor draws its text in — and the quick view its find.
#[derive(Clone, Copy)]
struct EditorLook {
    text: Style,
    selected: Style,
    found: Style,
}

impl EditorLook {
    fn of(classic_style: bool) -> Self {
        // Black, not the terminal's own dark color, which some themes make
        // too light to read on these.
        let selected = Style::default().fg(Color::Rgb(0, 0, 0)).bg(SELECTION_BG);
        let found = Style::default().fg(Color::Rgb(0, 0, 0)).bg(FIND_MATCH_BG);
        let text = if classic_style {
            Style::default().fg(classic::FILE_FG).bg(classic::BG)
        } else {
            // Like the file names: the terminal's own text color.
            Style::default().fg(Color::Reset)
        };
        EditorLook {
            text,
            selected,
            found,
        }
    }
}

/// The built-in editor in `area`: a framed text area titled with the file's
/// path, the find bar while it's open, and a status line. The cursor is the
/// terminal's own (unless `show_cursor` is off because something covers
/// the editor). Returns the text area, for the mouse.
fn draw_editor(
    frame: &mut Frame,
    area: Rect,
    editor: &mut Editor,
    classic_style: bool,
    focused: bool,
    show_cursor: bool,
) -> Rect {
    // Without the keyboard (Tab gave it to the file list), framed like an
    // inactive pane, the selection still there but dimmed.
    let border_style = match (classic_style, focused) {
        (true, true) => Style::default()
            .fg(classic::ACTIVE_BORDER_FG)
            .add_modifier(Modifier::BOLD),
        (true, false) => Style::default()
            .fg(classic::INACTIVE_BORDER_FG)
            .add_modifier(Modifier::BOLD),
        (false, true) => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        (false, false) => Style::default().fg(Color::DarkGray),
    };
    let mut look = EditorLook::of(classic_style);
    if !focused {
        look.selected = Style::default().fg(Color::White).bg(Color::DarkGray);
    }

    let mut block = titled_with_label(
        Block::default()
            .borders(Borders::ALL)
            .border_style(border_style),
        &if editor.untitled {
            editor.file_name()
        } else {
            editor.path.display().to_string()
        },
        // Marked as Notepad marks its title, on the border where the eye
        // goes first.
        if editor.is_modified() {
            "Editing*"
        } else {
            "Editing"
        },
        area.width,
    );
    if classic_style {
        block = block.style(look.text);
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // From the bottom up: the status line, the find bar, and a divider
    // between them and the text like the panes' own footer.
    let mut text_area = inner;
    let take_bottom_row = |text_area: &mut Rect| {
        (text_area.height > 0).then(|| {
            text_area.height -= 1;
            Rect {
                y: text_area.y + text_area.height,
                height: 1,
                ..*text_area
            }
        })
    };
    let status_area = take_bottom_row(&mut text_area);
    let find_area = if editor.find.is_some() {
        take_bottom_row(&mut text_area)
    } else {
        None
    };
    let divider_area = if text_area.height >= 3 {
        take_bottom_row(&mut text_area)
    } else {
        None
    };

    editor.scroll_into_view(text_area.height as usize, text_area.width as usize);
    let lines: Vec<Line> = editor
        .screen_rows(text_area.height as usize)
        .iter()
        .map(|row| editor_row_line(editor, row, text_area.width as usize, look))
        .collect();
    frame.render_widget(Paragraph::new(lines).style(look.text), text_area);

    if let Some(divider) = divider_area {
        frame.render_widget(
            Paragraph::new("\u{2500}".repeat(divider.width as usize)).style(border_style),
            divider,
        );
        let buf = frame.buffer_mut();
        if let Some(cell) = buf.cell_mut((area.x, divider.y)) {
            cell.set_symbol("\u{251c}").set_style(border_style);
        }
        if let Some(cell) = buf.cell_mut((area.x + area.width.saturating_sub(1), divider.y)) {
            cell.set_symbol("\u{2524}").set_style(border_style);
        }
    }

    let mut cursor = editor.cursor_in_view().map(|(row, x)| {
        ratatui::layout::Position::new(text_area.x + x as u16, text_area.y + row as u16)
    });
    if let (Some(find_area), Some(bar)) = (find_area, &editor.find) {
        cursor = draw_find_bar(frame, find_area, bar, "", classic_style, look);
    }
    if let Some(status_area) = status_area {
        frame.render_widget(
            Paragraph::new(editor_status_line(
                editor,
                status_area.width as usize,
                focused,
            ))
            .style(look.text),
            status_area,
        );
    }
    if show_cursor && let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
    text_area
}

/// Cells of path a pane keeps before its label gives way to it.
const MIN_TITLE_PATH: usize = 8;

/// The top border of a box `width` cells wide showing `path` and what it
/// is (`label`: "Preview", "Editing"): the path on the left, cut from the
/// left to fit, and the label on the right, a space apart so the two can
/// never run into each other. Where there isn't room for the label and a
/// useful bit of the path, the label goes.
fn titled_with_label<'a>(block: Block<'a>, path: &str, label: &'a str, width: u16) -> Block<'a> {
    let inside = width.saturating_sub(2) as usize;
    let label_width = label.chars().count();
    if inside >= label_width + 1 + MIN_TITLE_PATH {
        block
            .title(fit_path_title(path, inside - label_width - 1))
            .title_top(Line::from(label).right_aligned())
    } else {
        block.title(fit_path_title(path, inside))
    }
}

/// `path` in at most `width` cells: cut from the left (`…/src/main.rs`) when
/// it's too long, so the file's own name is what stays in sight.
fn fit_path_title(path: &str, width: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if Span::raw(path).width() <= width {
        return path.to_string();
    }
    let mut tail = String::new();
    let mut used = 1;
    for &c in chars.iter().rev() {
        let w = Span::raw(c.to_string()).width();
        if used + w > width {
            break;
        }
        used += w;
        tail.insert(0, c);
    }
    format!("\u{2026}{tail}")
}

/// One screen row of the editor's text, `cols` cells wide from the view's
/// horizontal scroll, with the selection and find matches highlighted and a
/// selected line break as one highlighted cell after the text.
fn editor_row_line(
    editor: &Editor,
    row: &ScreenRow,
    cols: usize,
    look: EditorLook,
) -> Line<'static> {
    // Shared by every screen row of the line, and kept between frames.
    let layout = editor.line_layout(row.line);
    let selection = editor.selection();
    let matches = editor
        .find
        .as_ref()
        .map_or(&[][..], |bar| bar.matches_on(row.line));
    let mut next_match = 0;
    let is_selected = |pos: Pos| selection.is_some_and(|(start, end)| start <= pos && pos < end);
    let style_at = |col: usize| {
        while next_match < matches.len() && matches[next_match].1.col <= col {
            next_match += 1;
        }
        if is_selected(Pos::new(row.line, col)) {
            look.selected
        } else if matches
            .get(next_match)
            .is_some_and(|(start, _)| start.col <= col)
        {
            look.found
        } else {
            look.text
        }
    };
    let line_break =
        (row.last && is_selected(Pos::new(row.line, layout.chars.len()))).then_some(look.selected);
    text_row_line(
        &layout,
        (row.start, row.end),
        editor.scroll_x(),
        cols,
        style_at,
        line_break,
    )
}

/// A screen row of text — characters `start..end` of the line laid out in
/// `layout` — as the editor draws it, and the quick view too, so a file
/// looks the same in both: `cols` cells from column `scroll_x`, tabs as
/// spaces to the next stop, control and direction marks as visible glyphs,
/// a wide letter cut by either edge as blank cells. `style_at` colors each
/// character (asked in order); `line_break`, if given, is one cell of that
/// style after the text.
fn text_row_line(
    layout: &editor::LineLayout,
    (start, end): (usize, usize),
    scroll_x: usize,
    cols: usize,
    mut style_at: impl FnMut(usize) -> Style,
    line_break: Option<Style>,
) -> Line<'static> {
    let chars = &layout.chars;
    let end_x = scroll_x + cols;
    let mut spans: Vec<Span<'static>> = Vec::new();
    let push = |spans: &mut Vec<Span<'static>>, text: String, style: Style| match spans.last_mut() {
        Some(last) if last.style == style => last.content.to_mut().push_str(&text),
        _ => spans.push(Span::styled(text, style)),
    };
    // Scrolled far along a long line, start near the first cell shown.
    let (from, mut x) = layout.seek(start, scroll_x);
    for (col, &c) in chars.iter().enumerate().take(end).skip(from) {
        let width = editor::char_width(c, x);
        if width == 0 {
            // A combining mark goes with the letter before it.
            if x > scroll_x && x <= end_x {
                let style = spans
                    .last()
                    .map_or_else(|| style_at(col), |span| span.style);
                push(&mut spans, c.to_string(), style);
            }
            continue;
        }
        if x + width <= scroll_x {
            x += width;
            continue;
        }
        if x >= end_x {
            break;
        }
        let style = style_at(col);
        let text = if c == '\t' || x < scroll_x || x + width > end_x {
            " ".repeat((x + width).min(end_x) - x.max(scroll_x))
        } else {
            editor::display_glyph(c).to_string()
        };
        push(&mut spans, text, style);
        x += width;
    }
    if let Some(style) = line_break
        && x >= scroll_x
        && x < end_x
    {
        push(&mut spans, " ".to_string(), style);
    }
    Line::from(spans)
}

/// The find bar: the search text in a field (preselected text shown as
/// such, so it's clear typing replaces it) and the match count. Returns
/// where the cursor goes, in the field.
fn draw_find_bar(
    frame: &mut Frame,
    area: Rect,
    bar: &editor::FindBar,
    note: &str,
    classic_style: bool,
    look: EditorLook,
) -> Option<ratatui::layout::Position> {
    let pal = dialog_palette(classic_style);
    let field_style = Style::default().bg(pal.field_bg).fg(pal.field_fg);
    const LABEL: &str = " Find: ";
    // `note` qualifies the count ("3/12 (first 1 MB)"), once there is one.
    let status = bar.status();
    let note = if status.is_empty() { "" } else { note };
    let status = format!(" {status}{note} ");
    let label_width = LABEL.chars().count();
    let status_width = Span::raw(status.as_str()).width();
    let field_width = (area.width as usize).saturating_sub(label_width + status_width);

    let chars: Vec<char> = bar.field.text().chars().collect();
    let cursor = bar.field.cursor();
    // One cell to spare at the end, for the cursor after the last letter.
    let (start, end) = visible_chars(&chars, cursor, field_width.saturating_sub(1).max(1));
    let shown: String = chars[start..end].iter().collect();
    let shown_width = Span::raw(shown.as_str()).width();
    let text_style = if bar.fresh {
        look.selected
    } else {
        field_style
    };
    let line = Line::from(vec![
        Span::styled(LABEL, look.text),
        Span::styled(shown, text_style),
        Span::styled(
            " ".repeat(field_width.saturating_sub(shown_width)),
            field_style,
        ),
        Span::styled(status, look.text),
    ]);
    frame.render_widget(Paragraph::new(line).style(look.text), area);

    let before: String = chars[start..cursor.max(start)].iter().collect();
    let x = label_width + Span::raw(before.as_str()).width();
    (x < area.width as usize).then(|| ratatui::layout::Position::new(area.x + x as u16, area.y))
}

/// "Ln 3, Col 14, U+0105  Modified" on the left — plus a note like "Saved" —
/// and the main keys on the right, as room allows.
#[cfg(test)]
fn editor_status_text(editor: &Editor, width: usize) -> String {
    editor_status_line(editor, width, true)
}

/// The editor's status line; `focused`: whether it has the keyboard (the
/// keys it offers are its own only then).
fn editor_status_line(editor: &Editor, width: usize, focused: bool) -> String {
    let cursor = editor.cursor();
    let mut left = format!(" Ln {}, Col {}", cursor.line + 1, cursor.col + 1);
    // The code of the character under the cursor, as Unicode writes it;
    // at the end of a line, the line ending's (nothing after the last line
    // of a file without one). Read from the cached layout, so a very long
    // line isn't walked again.
    let under: Vec<char> = match editor.line_layout(cursor.line).chars.get(cursor.col) {
        Some(&c) => vec![c],
        None => editor.line_ending(cursor.line).to_vec(),
    };
    if !under.is_empty() {
        left.push(',');
        for c in under {
            left.push_str(&format!(" U+{:04X}", u32::from(c)));
        }
    }
    if editor.is_modified() {
        left.push_str("  Modified");
    }
    if let Some(message) = &editor.message {
        left.push_str("  ");
        left.push_str(message);
    }
    // Longest first; the first that fits is shown. Full screen, with no
    // menu bar or F-key bar in sight, F1 and F9 lead, so it's clear where
    // help and the menu are, and F1 and the way out are the last to go. In
    // the pane the F-key bar below says all that, so only what it doesn't.
    // With something selected, Esc only lets go of it (see
    // `App::run_editor_command`), so that's what the hint says.
    let selected = editor.selection().is_some();
    let hints: &[&str] = match (editor.full_screen, editor.find.is_some(), selected) {
        // The file list has the keyboard: its keys are the F-key bar's.
        _ if !focused => &[],
        (false, true, _) => &[
            "F3 Next  Shift+F3 Previous  Enter Select ",
            "F3 Next  Enter Select ",
            "F3 Next ",
        ],
        (false, false, _) => &[
            "Ctrl+S Save  Tab List  F4 Full screen ",
            "Ctrl+S Save  Tab List ",
            "Tab List ",
        ],
        (true, true, _) => &[
            "F1 Help  F9 Menu  F3 Next  Shift+F3 Previous  Enter Select  Esc Done ",
            "F1 Help  F9 Menu  F3 Next  Enter Select  Esc Done ",
            "F1 Help  Esc Done ",
        ],
        (true, false, true) => &[
            "F1 Help  F9 Menu  Ctrl+S Save  Esc Done  F4 Pane ",
            "F1 Help  F9 Menu  Ctrl+S Save  Esc Done ",
            "F1 Help  F9 Menu  Esc Done ",
            "F1 Help  Esc Done ",
        ],
        (true, false, false) => &[
            "F1 Help  F9 Menu  Ctrl+S Save  Esc Close  F4 Pane ",
            "F1 Help  F9 Menu  Ctrl+S Save  Esc Close ",
            "F1 Help  F9 Menu  Esc Close ",
            "F1 Help  Esc Close ",
        ],
    };
    let left_width = Span::raw(left.as_str()).width();
    match hints
        .iter()
        .find(|hint| left_width + 2 + hint.chars().count() <= width)
    {
        Some(hint) => format!(
            "{left}{}{hint}",
            " ".repeat(width - left_width - hint.chars().count())
        ),
        None => left,
    }
}

/// Command > Show Logs: reads the tail of the log file fresh each draw
/// (simple, and the file is small enough that this is cheap) and shows as
/// many of the most recent lines as fit.
/// Command > Show Logs: the tail of the log file in a big dialog box.
fn draw_logs(frame: &mut Frame, classic_style: bool) {
    let pal = dialog_palette(classic_style);
    // Outer padding, border, the divider and the footer row — plus the
    // menu bar and F-key bar, which stay visible around it.
    let visible_rows = frame
        .area()
        .height
        .saturating_sub(2 * OUTER_PAD_Y + 2 + 2 + 2) as usize;
    let all_lines = std::fs::read_to_string(logging::log_path()).unwrap_or_default();
    let lines: Vec<&str> = all_lines.lines().collect();
    let shown = &lines[lines.len().saturating_sub(visible_rows.max(1))..];

    let body: Vec<DialogLine> = if shown.is_empty() {
        vec![DialogLine::Text(Line::from(Span::styled(
            "(no log entries yet)",
            Style::default().fg(pal.fg),
        )))]
    } else {
        shown
            .iter()
            .map(|line| {
                DialogLine::Text(Line::from(Span::styled(*line, Style::default().fg(pal.fg))))
            })
            .collect()
    };
    let footer = DialogLine::Centered(Line::from(Span::styled(
        format!(
            "Any key closes   (full log: {})",
            logging::log_path().display()
        ),
        Style::default().fg(pal.fg),
    )));
    let title = format!("Logs \u{2014} last {} lines", shown.len());
    let min_width = frame.area().width.saturating_sub(4);
    draw_dialog_box(frame, &pal, &title, vec![body, vec![footer]], min_width);
}

const PROGRAM_NAME: &str = "Peter Commander";

/// The first year in the copyright line — the same one LICENSE carries.
const COPYRIGHT_YEAR: u16 = 2026;

/// Author names from `CARGO_PKG_AUTHORS` (`Name <email>`, several joined
/// by `:`), without the emails.
fn author_names() -> String {
    env!("CARGO_PKG_AUTHORS")
        .split(':')
        .map(|author| author.split(" <").next().unwrap_or(author).trim())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Command > About: name, version, author, license and repository, all
/// taken from Cargo.toml so they can't drift from the packaged release.
fn draw_about(frame: &mut Frame, classic_style: bool) {
    let pal = dialog_palette(classic_style);
    let centered = |s: String, bold: bool| {
        let style = if bold {
            Style::default().fg(pal.fg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(pal.fg)
        };
        DialogLine::Centered(Line::from(Span::styled(s, style)))
    };
    let repository = env!("CARGO_PKG_REPOSITORY");
    let sections = vec![
        vec![
            centered(
                format!("{PROGRAM_NAME} {}", env!("CARGO_PKG_VERSION")),
                true,
            ),
            centered(env!("CARGO_PKG_DESCRIPTION").to_string(), false),
        ],
        vec![
            centered(format!("\u{a9} {COPYRIGHT_YEAR} {}", author_names()), false),
            centered(format!("{} License", env!("CARGO_PKG_LICENSE")), false),
            centered(
                repository
                    .strip_prefix("https://")
                    .unwrap_or(repository)
                    .to_string(),
                false,
            ),
        ],
        vec![button_row(&[("OK", true)], &pal)],
    ];
    draw_dialog_frame(frame, classic_style, "About", sections, false);
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

        // While editing, each key as the editor has it: relabeled where it
        // does something else there, dimmed (like a grayed menu item)
        // where it does nothing.
        let (label, active) = if app.editor_has_focus() {
            app.editor_fn_key(fn_key)
        } else if fn_key
            .action
            .is_some_and(|action| app.ruled_out_by_covered_pane(action))
        {
            // The file list beside the editor or the quick view: what that
            // rules out (Copy and Move, whose target is the other pane,
            // covered) is dimmed as in the menu.
            (fn_key.label, false)
        } else if fn_key.action == Some(Action::View) && app.preview_next_mode() {
            // F3 is Find Next in a quick view being searched.
            ("Next", true)
        } else {
            (fn_key.label, true)
        };
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
        let (num_fg, label_fg) = match (active, app.classic_style) {
            (true, _) => (num_fg, label_fg),
            (false, true) => (classic::MENU_DISABLED_FG, classic::MENU_DISABLED_FG),
            (false, false) => (Color::DarkGray, Color::DarkGray),
        };
        let num_style = Style::default().fg(num_fg).bg(num_bg);
        spans.push(Span::styled(
            fn_key.key,
            if active {
                num_style.add_modifier(Modifier::BOLD)
            } else {
                num_style
            },
        ));
        let label_width = tile_widths[idx].saturating_sub(fn_key.key.len() as u16) as usize;
        spans.push(Span::styled(
            format!("{label:<label_width$}"),
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
        let category = &crate::menu::MENU_BAR[0];
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

    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        (area.y..area.y + area.height)
            .map(|y| {
                (area.x..area.x + area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn char_vec(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn field_that_fits_shows_everything() {
        let chars = char_vec("/home/a.txt");
        assert_eq!(visible_chars(&chars, 3, 40), (0, chars.len()));
    }

    #[test]
    fn long_field_always_keeps_the_cursor_in_view() {
        let chars = char_vec(&"abcdefghij".repeat(10));
        for cursor in 0..=chars.len() {
            let (start, end) = visible_chars(&chars, cursor, 20);
            assert!(start <= cursor && cursor <= end, "{cursor}: {start}..{end}");
            assert!(end - start <= 18, "{cursor}: {start}..{end}");
        }
        assert_eq!(visible_chars(&chars, chars.len(), 20).1, chars.len());
        assert_eq!(visible_chars(&chars, 0, 20).0, 0);
    }

    #[test]
    fn wide_characters_count_as_two_cells() {
        let chars = char_vec(&"\u{65e5}".repeat(30));
        let (start, end) = visible_chars(&chars, 15, 20);
        assert!((end - start) * 2 <= 18, "{start}..{end}");
    }

    #[test]
    fn picture_shows_in_the_preview_pane_and_steps_aside_for_a_dialog() {
        use crate::app::App;
        use crate::pane::Pane;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui_image::picker::Picker;
        use std::fs;

        let dir = std::env::temp_dir().join("pc_test_preview_picture");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        image::RgbImage::from_pixel(300, 300, image::Rgb([200, 40, 40]))
            .save(dir.join("a.png"))
            .unwrap();

        let mut app = App::new().unwrap();
        app.left = Pane::new(dir.clone(), false).unwrap();
        app.active = Side::Left;
        app.left.selected = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "a.png")
            .unwrap();
        app.quick_view = true;
        app.image_preview.set_picker(Picker::halfblocks());

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(buffer_text(terminal.backend().buffer()).contains("Loading image"));

        let start = std::time::Instant::now();
        loop {
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            if !buffer_text(terminal.backend().buffer()).contains("Loading image") {
                break;
            }
            assert!(start.elapsed().as_secs() < 10, "picture never arrived");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let has_picture = |buf: &Buffer| {
            let area = buf.area;
            (area.top()..area.bottom()).any(|y| {
                (40..area.right()).any(|x| {
                    buf[(x, y)].bg == Color::Rgb(200, 40, 40)
                        || buf[(x, y)].fg == Color::Rgb(200, 40, 40)
                })
            })
        };
        assert!(has_picture(terminal.backend().buffer()));

        app.about_open = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(!has_picture(terminal.backend().buffer()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn shrinking_a_scrolled_listing_leaves_no_hole_at_the_bottom() {
        use crate::app::App;
        use crate::pane::Pane;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use std::fs;

        let dir = std::env::temp_dir().join("pc_test_list_hole");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for index in 0..50 {
            fs::write(dir.join(format!("f{index:02}")), "x").unwrap();
        }
        let mut app = App::new().unwrap();
        app.left = Pane::new(dir.clone(), false).unwrap();
        app.left.selected = app.left.entries.len() - 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let scrolled = app.left_list_state.offset();
        assert!(scrolled > 0);

        // Deleting the last file: one row less to scroll past.
        fs::remove_file(dir.join("f49")).unwrap();
        app.left.reload().unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.left_list_state.offset(), scrolled - 1);
        assert_eq!(app.left.selected, app.left.entries.len() - 1);

        // A listing that fits again shows from the top.
        for index in 5..49 {
            fs::remove_file(dir.join(format!("f{index:02}"))).unwrap();
        }
        app.left.reload().unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.left_list_state.offset(), 0);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_button_is_highlighted_while_editing_the_target() {
        use crate::app::App;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new().unwrap();
        app.classic_style = false;
        let items = vec![("a.txt".to_string(), PathBuf::from("/src/a.txt"))];
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        for editing in [false, true] {
            let mut transfer =
                Transfer::new(DialogKind::Copy, items.clone(), PathBuf::from("/dest"));
            transfer.editing = editing;
            app.dialog = Dialog::Transfer(transfer);
            app.dialog_cancel_focused = false;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();

            let buf = terminal.backend().buffer();
            let area = buf.area;
            // Only the button's own cells: the panes behind the dialog have
            // bold entries of their own on the same row.
            let has_bold_button = (area.top()..area.bottom()).any(|y| {
                let cells: Vec<_> = (area.left()..area.right()).map(|x| &buf[(x, y)]).collect();
                cells.windows(8).any(|button| {
                    let label: String = button.iter().map(|cell| cell.symbol()).collect();
                    label == "[ Copy ]" && button[0].modifier.contains(Modifier::BOLD)
                })
            });
            assert_eq!(has_bold_button, !editing);
        }
    }

    #[test]
    fn transfer_dialog_draws_target_and_overwrite_prompt_in_both_styles() {
        use crate::app::App;
        use crate::transfer::Overwrite;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use std::path::Path;

        // In the platform's own spelling, as the dialog shows it.
        let target = Path::new("/dest").join("a.txt").display().to_string();
        for classic_style in [false, true] {
            let mut app = App::new().unwrap();
            app.classic_style = classic_style;
            let items = vec![("a.txt".to_string(), PathBuf::from("/src/a.txt"))];
            let mut transfer =
                Transfer::new(DialogKind::Copy, items.clone(), PathBuf::from("/dest"));
            transfer.editing = true;
            app.dialog = Dialog::Transfer(transfer);

            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let text = buffer_text(terminal.backend().buffer());
            for expected in [
                "Copy \"a.txt\" to",
                target.as_str(),
                "[ Copy ]",
                "[ Cancel ]",
            ] {
                assert!(text.contains(expected), "missing {expected:?}:\n{text}");
            }

            if let Dialog::Transfer(transfer) = &mut app.dialog {
                transfer.overwrite = Some(Overwrite {
                    items,
                    dest_dir: Path::new("/dest").to_path_buf(),
                    conflicts: vec!["a.txt".to_string()],
                });
            }
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let text = buffer_text(terminal.backend().buffer());
            for expected in ["already exists", target.as_str(), "[ Overwrite ]"] {
                assert!(text.contains(expected), "missing {expected:?}:\n{text}");
            }
        }
    }

    #[test]
    fn author_names_drop_the_email() {
        let names = author_names();
        assert!(names.contains("Piotr \u{15a}r\u{f3}dka"), "{names}");
        assert!(!names.contains('<'), "{names}");
    }

    #[test]
    fn about_dialog_shows_version_and_author_in_both_styles() {
        use crate::app::App;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        for classic_style in [false, true] {
            let mut app = App::new().unwrap();
            app.classic_style = classic_style;
            app.about_open = true;

            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let text = buffer_text(terminal.backend().buffer());

            for expected in [
                format!("{PROGRAM_NAME} {}", env!("CARGO_PKG_VERSION")),
                author_names(),
                "MIT License".to_string(),
                "[ OK ]".to_string(),
            ] {
                assert!(text.contains(&expected), "missing {expected:?}:\n{text}");
            }
        }
    }

    #[test]
    fn help_box_keeps_its_place_on_every_page() {
        use crate::app::App;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut app = App::new().unwrap();
        app.help_open = true;
        let corners: Vec<usize> = (0..HELP_PAGE_COUNT)
            .map(|page| {
                app.help_page = page;
                let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let text = buffer_text(terminal.backend().buffer());
                let row = text.lines().find(|l| l.contains("Help \u{2014}")).unwrap();
                row.chars().position(|c| c == '\u{2554}').unwrap()
            })
            .collect();
        assert!(corners.windows(2).all(|w| w[0] == w[1]), "{corners:?}");
    }

    /// An app editing a file holding `text`, drawn on an 80x24 screen.
    fn editing_app(name: &str, text: &str) -> (App, PathBuf) {
        use crate::pane::Pane;
        let dir = std::env::temp_dir().join(format!("pc_test_ui_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), text).unwrap();
        let mut app = App::new().unwrap();
        app.classic_style = false;
        app.left = Pane::new(dir.clone(), false).unwrap();
        app.active = Side::Left;
        app.left.selected = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == "notes.txt")
            .unwrap();
        app.editor = Some(Editor::open(&dir.join("notes.txt")).unwrap());
        (app, dir)
    }

    fn draw_app(app: &mut App) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
    }

    fn cursor_of(terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>) -> (u16, u16) {
        use ratatui::backend::Backend;
        let position = terminal.backend_mut().get_cursor_position().unwrap();
        (position.x, position.y)
    }

    #[test]
    fn editor_shows_in_the_other_pane_with_its_cursor_and_status() {
        let (mut app, dir) = editing_app("editor_pane", "zażółć\n\tgęślą\n");
        if let Some(editor) = &mut app.editor {
            editor.apply(editor::Command::Move {
                motion: editor::Motion::Down,
                select: false,
            });
            editor.apply(editor::Command::Move {
                motion: editor::Motion::Right,
                select: false,
            });
        }
        let mut terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        let rows: Vec<&str> = text.lines().collect();
        // The listing stays on the left; the editor is the right pane.
        // A long path is cut from the left, so the file name stays in sight.
        let right_title = &rows[1][rows[1].find('\u{2510}').unwrap()..];
        assert!(right_title.contains("\u{2026}"), "{text}");
        // What the pane is, on the right of the same border.
        assert!(
            right_title.contains("/notes.txt\u{2500}Editing\u{2510}"),
            "{text}"
        );
        assert!(rows[2].contains("\u{2502}zażółć"), "{text}");
        assert!(rows[3].contains("\u{2502}    gęślą"), "{text}");
        assert!(text.contains("Ln 2, Col 2"), "{text}");
        // In the pane the F-key bar below has help, the menu and the rest;
        // the status line only what it doesn't.
        assert!(text.contains("Tab List"), "{text}");
        assert!(
            !text.contains("F1 Help") && !text.contains("Esc Close"),
            "{text}"
        );
        assert!(text.contains("F1Help"), "the F-key bar is still there");
        assert!(text.contains("F3Next") && text.contains("F4Full"), "{text}");
        // After the tab: four cells in, on the right pane's text.
        let editor_x = app.editor_text_area.x;
        assert_eq!(cursor_of(&mut terminal), (editor_x + 4, 3));
        assert!(editor_x >= 40);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn full_screen_editor_takes_the_whole_screen_in_both_styles() {
        for classic_style in [false, true] {
            let (mut app, dir) = editing_app("editor_full", "first line\n");
            app.classic_style = classic_style;
            app.editor_focus_mode = false;
            app.editor.as_mut().unwrap().full_screen = true;
            let mut terminal = draw_app(&mut app);
            let buf = terminal.backend().buffer().clone();
            let text = buffer_text(&buf);
            let rows: Vec<&str> = text.lines().collect();
            // The menu bar stays on top, as everywhere else in the app.
            assert!(rows[0].contains("File  Edit  Options  Command"), "{text}");
            assert!(rows[1].contains("notes.txt"), "{text}");
            assert!(rows[2].starts_with("\u{2502}first line"), "{text}");
            assert!(text.contains("F1 Help  F9 Menu"), "{text}");
            assert!(text.contains("F4 Pane"), "{text}");
            assert!(!text.contains("F1Help"), "{text}");
            assert_eq!(cursor_of(&mut terminal), (1, 2));
            assert!(!app.menu_bar_tiles.is_empty(), "the bar takes clicks");
            if classic_style {
                assert_eq!(buf[(20, 10)].bg, classic::BG);
            }

            app.menu_open = true;
            app.menu_category = 1;
            let terminal = draw_app(&mut app);
            let text = buffer_text(terminal.backend().buffer());
            assert!(text.contains("Wrap Lines      [ ]"), "{text}");
            assert!(text.contains("Find..."), "{text}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn focus_mode_hides_the_menu_bar_until_it_is_called_up() {
        let (mut app, dir) = editing_app("editor_focus", "first line\n");
        app.editor_focus_mode = true;
        app.editor.as_mut().unwrap().full_screen = true;
        let mut terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        let rows: Vec<&str> = text.lines().collect();
        assert!(!rows[0].contains("Options"), "{text}");
        assert!(rows[0].contains("notes.txt"), "{text}");
        assert!(rows[1].starts_with("\u{2502}first line"), "{text}");
        // The way to the menu (and help) is still written down.
        assert!(text.contains("F1 Help  F9 Menu"), "{text}");
        assert_eq!(cursor_of(&mut terminal), (1, 1));

        // F9 brings the menu bar up over the title, with the Edit menu.
        app.menu_open = true;
        app.menu_category = 1;
        let terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.lines().next().unwrap().contains("Edit"), "{text}");
        assert!(text.contains("Wrap Lines      [ ]"), "{text}");

        // In the pane the setting changes nothing: the bar is always there.
        app.menu_open = false;
        app.editor.as_mut().unwrap().full_screen = false;
        let terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.lines().next().unwrap().contains("Options"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The open dropdown's text: the rows under the menu bar, from its
    /// left edge on.
    fn dropdown_text(app: &mut App) -> String {
        let terminal = draw_app(app);
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn menus_follow_the_editor_opening_and_closing() {
        let (mut app, dir) = editing_app("editor_menu_switch", "x\n");
        app.editor = None;
        // Outside the editor, last on Command (the third menu).
        app.open_menu_at(2);
        assert!(dropdown_text(&mut app).contains("Sort by Name"));
        app.close_menu();
        app.run_action(Action::Edit).unwrap();
        assert!(app.editor.is_some());
        app.editor_key(
            crossterm::event::KeyCode::F(9),
            crossterm::event::KeyModifiers::NONE,
        )
        .unwrap();
        assert_eq!(app.menu_category, 0);
        let text = dropdown_text(&mut app);
        assert!(text.contains("Save") && text.contains("Close"), "{text}");
        assert!(!text.contains("Quit "), "Quit is under Command: {text}");
        assert!(
            !text.contains("Sort by Name") && !text.contains("Invert Selection"),
            "{text}"
        );

        // And back: last on the editor's Command (the fourth), then closed.
        app.close_menu();
        app.open_menu_at(3);
        let text = dropdown_text(&mut app);
        assert!(
            text.contains("Show Logs") && text.contains("Quit "),
            "{text}"
        );
        app.close_menu();
        app.run_action(Action::Close).unwrap();
        assert!(app.editor.is_none());
        app.open_menu();
        let text = dropdown_text(&mut app);
        assert!(text.contains("Invert Selection"), "{text}");
        assert!(!text.contains("Close"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The colors of the text area's cells, row by row.
    fn text_area_backgrounds(app: &mut App) -> Vec<Color> {
        let terminal = draw_app(app);
        let buf = terminal.backend().buffer().clone();
        let area = app.editor_text_area;
        (area.y..area.bottom())
            .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
            .map(|(x, y)| buf[(x, y)].bg)
            .collect()
    }

    /// The text in `area` of a freshly drawn screen, row by row, without
    /// trailing spaces.
    fn rows_in(app: &mut App, area: Rect) -> Vec<String> {
        let terminal = draw_app(app);
        let text = buffer_text(terminal.backend().buffer());
        text.lines()
            .skip(area.y as usize)
            .take(area.height as usize)
            .map(|row| {
                row.chars()
                    .skip(area.x as usize)
                    .take(area.width as usize)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The right pane's inside on the 80x24 screen: where the quick view
    /// shows (with the left pane active), and the editor's text.
    const RIGHT_INSIDE: Rect = Rect {
        x: 41,
        y: 2,
        width: 38,
        height: 8,
    };

    /// An app with the quick view of a file holding `text` open on the
    /// right.
    fn previewing_app(name: &str, text: &str) -> (App, PathBuf) {
        let (mut app, dir) = editing_app(name, text);
        app.editor = None;
        app.run_action(Action::View).unwrap();
        assert!(app.quick_view);
        draw_app(&mut app);
        (app, dir)
    }

    const LONG_TEXT: &str =
        "slowo slowo slowo slowo slowo slowo slowo slowo slowo slowo\n\tx\u{1b}\n";

    #[test]
    fn find_in_the_preview_highlights_its_matches_with_a_bar_below() {
        // Past the 1 MB the preview reads, which the counter mentions.
        let mut text = String::from("kot i kot\n");
        text.push_str(&"x".repeat(crate::preview::PREVIEW_READ_LIMIT));
        let (mut app, dir) = previewing_app("preview_find_render", &text);
        app.preview_focus = true;
        assert!(app.open_preview_find());
        for c in "kot".chars() {
            app.preview_find_key(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            );
        }
        let terminal = draw_app(&mut app);
        let buf = terminal.backend().buffer().clone();
        let text = buffer_text(&buf);
        let (x, y) = (RIGHT_INSIDE.x, RIGHT_INSIDE.y);
        assert_eq!(buf[(x, y)].bg, SELECTION_BG, "the current match");
        assert_eq!(buf[(x + 4, y)].bg, Color::Reset);
        assert_eq!(buf[(x + 6, y)].bg, FIND_MATCH_BG, "another match");
        assert!(text.contains("Find: kot"), "{text}");
        assert!(text.contains("1/2 (first 1 MB)"), "{text}");
        assert!(text.contains("F3Next"), "F3 is Next on the bar: {text}");
        // Esc: the bar and every highlight go; F3 is View again only once
        // there's nothing to look for — here there still is.
        app.preview_find_key(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        );
        let terminal = draw_app(&mut app);
        let buf = terminal.backend().buffer().clone();
        let text = buffer_text(&buf);
        assert!(!text.contains("Find:"), "{text}");
        assert_eq!(buf[(x, y)].bg, Color::Reset);
        assert_eq!(buf[(x + 6, y)].bg, Color::Reset);
        assert!(text.contains("F3Next"), "{text}");
        app.preview_focus = false;
        let text = buffer_text(draw_app(&mut app).backend().buffer());
        assert!(text.contains("F3View"), "the listing has the keys: {text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unwrapped_preview_cuts_long_lines_and_scrolls_sideways() {
        let (mut app, dir) = previewing_app("preview_nowrap", LONG_TEXT);
        app.wrap_lines = false;
        let rows = rows_in(&mut app, RIGHT_INSIDE);
        assert_eq!(rows[0], "slowo slowo slowo slowo slowo slowo sl");
        // A tab to the next stop and a control character as its picture,
        // as in the editor.
        assert_eq!(rows[1], "    x\u{241b}");
        assert_eq!(rows[2], "");
        // Tab on the preview, then Right: a tab stop at a time.
        app.preview_focus = true;
        app.scroll_preview_sideways(true);
        app.scroll_preview_sideways(true);
        let rows = rows_in(&mut app, RIGHT_INSIDE);
        assert_eq!(rows[0], "owo slowo slowo slowo slowo slowo slow");
        assert_eq!(rows[1], "");
        // No further than the longest line needs.
        for _ in 0..20 {
            app.scroll_preview_sideways(true);
        }
        assert_eq!(app.preview_scroll_x, 59 - 38);
        app.scroll_preview_sideways(false);
        assert_eq!(app.preview_scroll_x, 59 - 38 - 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrapped_preview_breaks_lines_exactly_as_the_editor_does() {
        let (mut app, dir) = previewing_app("preview_wrap", LONG_TEXT);
        app.wrap_lines = true;
        let preview = rows_in(&mut app, RIGHT_INSIDE);
        assert!(preview[0].starts_with("slowo slowo"), "{preview:?}");
        assert!(preview[1].starts_with("slowo"), "{preview:?}");
        // Sideways scrolling is for unwrapped text only.
        app.scroll_preview_sideways(true);
        assert_eq!(app.preview_scroll_x, 0);

        app.run_action(Action::Edit).unwrap();
        let editor = rows_in(&mut app, RIGHT_INSIDE);
        assert_eq!(preview[..4], editor[..4]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrap_lines_in_options_switches_the_open_preview_keeping_its_place() {
        let text: String = (0..30)
            .map(|n| format!("line {n} {}\n", "word ".repeat(12)))
            .collect();
        let (mut app, dir) = previewing_app("preview_toggle", &text);
        app.wrap_lines = false;
        app.preview_focus = true;
        for _ in 0..5 {
            app.scroll_preview_down();
        }
        assert!(rows_in(&mut app, RIGHT_INSIDE)[0].starts_with("line 5 "));
        app.open_menu_at(1);
        assert_eq!(app.menu_bar()[1].items[0], Action::WrapLines);
        assert!(app.action_enabled(Action::WrapLines));
        app.confirm_menu_selection().unwrap();
        assert!(app.wrap_lines);
        let rows = rows_in(&mut app, RIGHT_INSIDE);
        assert!(rows[0].starts_with("line 5 "), "{rows:?}");
        assert!(!rows[1].starts_with("line 6 "), "now wrapped: {rows:?}");
        assert_eq!(app.menu_shortcut(Action::WrapLines), Some("[x]"));

        // Switched back in the editor, F3 shows the same again on Esc.
        app.run_action(Action::Edit).unwrap();
        app.run_action(Action::WrapLines).unwrap();
        app.run_action(Action::Close).unwrap();
        assert!(app.quick_view && !app.wrap_lines);
        let rows = rows_in(&mut app, RIGHT_INSIDE);
        assert!(rows[0].starts_with("line 5 "), "{rows:?}");
        assert!(rows[1].starts_with("line 6 "), "unwrapped again: {rows:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pane_titles_put_the_path_left_and_what_it_is_right() {
        let top = |width: u16, path: &str, label: &'static str| {
            let block =
                titled_with_label(Block::default().borders(Borders::ALL), path, label, width);
            let mut buf = Buffer::empty(Rect::new(0, 0, width, 3));
            ratatui::widgets::Widget::render(block, buf.area, &mut buf);
            buffer_text(&buf).lines().next().unwrap().to_string()
        };
        assert_eq!(
            top(30, "/home/pit/a.txt", "Preview"),
            "┌/home/pit/a.txt──────Preview┐"
        );
        // A long path gives way first, from the left, a cell short of the
        // label.
        assert_eq!(
            top(30, "/home/pit/projects/peter/notes.txt", "Editing"),
            "┌…cts/peter/notes.txt─Editing┐"
        );
        // Too narrow for both: the label goes, the file name stays.
        assert_eq!(
            top(16, "/home/pit/notes.txt", "Preview"),
            "┌…pit/notes.txt┐"
        );
        assert_eq!(top(4, "/a/b", "Preview"), "┌…b┐");
    }

    #[test]
    fn quick_view_title_is_the_path_with_preview_on_the_right() {
        let (mut app, dir) = previewing_app("preview_title", "x\n");
        let terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        let right = text.lines().nth(1).unwrap();
        let right = &right[right.find('\u{2510}').unwrap()..];
        assert!(right.contains("/notes.txt\u{2500}"), "{text}");
        assert!(right.ends_with("Preview\u{2510}"), "{text}");
        assert!(!text.contains("Preview:"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_f_key_bar_dims_what_does_nothing_in_the_editor() {
        for classic_style in [false, true] {
            let (mut app, dir) = editing_app("editor_fn_bar", "kot\n");
            app.classic_style = classic_style;
            let dim = if classic_style {
                classic::MENU_DISABLED_FG
            } else {
                Color::DarkGray
            };
            let label_fg = |app: &mut App, label: &str| {
                let terminal = draw_app(app);
                let buf = terminal.backend().buffer().clone();
                let row = buf.area.height - 1;
                let text: String = (0..buf.area.width)
                    .map(|x| buf[(x, row)].symbol())
                    .collect();
                let x = text
                    .find(label)
                    .unwrap_or_else(|| panic!("{label} in {text}")) as u16;
                buf[(x, row)].fg
            };
            for label in ["Copy", "Move", "MkDir", "Delete", "Next"] {
                assert_eq!(
                    label_fg(&mut app, label),
                    dim,
                    "{label}, classic {classic_style}"
                );
            }
            for label in ["Help", "Rename", "Full", "Menu"] {
                assert_ne!(
                    label_fg(&mut app, label),
                    dim,
                    "{label}, classic {classic_style}"
                );
            }
            assert!(!buffer_text(draw_app(&mut app).backend().buffer()).contains("View"));
            app.editor.as_mut().unwrap().apply(editor::Command::Find);
            assert_ne!(label_fg(&mut app, "Next"), dim, "with something to find");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn the_title_follows_a_rename_of_the_edited_file() {
        let (mut app, dir) = editing_app("editor_title_rename", "x\n");
        app.editor_key(
            crossterm::event::KeyCode::F(2),
            crossterm::event::KeyModifiers::NONE,
        )
        .unwrap();
        for _ in 0.."notes.txt".len() {
            app.dialog_field_backspace();
        }
        for c in "nowe.md".chars() {
            app.dialog_field_insert(c);
        }
        app.confirm_dialog().unwrap();
        let text = buffer_text(draw_app(&mut app).backend().buffer());
        let title = text.lines().nth(1).unwrap();
        assert!(title.contains("/nowe.md\u{2500}"), "{text}");
        assert!(dir.join("nowe.md").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_status_line_shows_the_code_of_the_character_under_the_cursor() {
        let path = PathBuf::from("notes.txt");
        let editor = Editor::from_bytes(path.clone(), "ąb".as_bytes()).unwrap();
        assert!(
            editor_status_text(&editor, 80).starts_with(" Ln 1, Col 1, U+0105"),
            "{}",
            editor_status_text(&editor, 80)
        );
        // At the end of a line, the line ending's codes; after the last
        // line of a file without one, none.
        let lf = Editor::from_bytes(path.clone(), b"\n").unwrap();
        let text = editor_status_text(&lf, 80);
        assert!(text.starts_with(" Ln 1, Col 1, U+000A "), "{text}");
        let crlf = Editor::from_bytes(path.clone(), b"\r\n").unwrap();
        let text = editor_status_text(&crlf, 80);
        assert!(text.starts_with(" Ln 1, Col 1, U+000D U+000A "), "{text}");
        let empty = Editor::from_bytes(path, b"").unwrap();
        let text = editor_status_text(&empty, 80);
        assert!(
            text.starts_with(" Ln 1, Col 1 ") && !text.contains("U+"),
            "{text}"
        );
    }

    #[test]
    fn the_part_with_the_keyboard_looks_it() {
        let (mut app, dir) = editing_app("editor_focus_look", "kot\n");
        app.editor
            .as_mut()
            .unwrap()
            .apply(editor::Command::SelectAll);
        let corner = |app: &mut App| {
            let terminal = draw_app(app);
            let buf = terminal.backend().buffer().clone();
            let area = app.editor_area;
            (buf[(area.x, area.y)].fg, buf[(area.x + 1, area.y + 1)].bg)
        };
        let mut terminal = draw_app(&mut app);
        assert!(terminal.backend_mut().get_cursor_position().is_ok());
        let text_area = app.editor_text_area;
        assert_eq!(corner(&mut app), (Color::Yellow, SELECTION_BG));
        let fn_bar = |app: &mut App| {
            let text = buffer_text(draw_app(app).backend().buffer());
            text.lines().last().unwrap().to_string()
        };
        assert!(fn_bar(&mut app).contains("F3Next"));
        assert!(buffer_text(draw_app(&mut app).backend().buffer()).contains("Tab List"));

        // Tab: the list's. The editor's frame and selection dim, no cursor
        // of its own, and the F-key bar is the usual one.
        app.toggle_editor_focus();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.hide_cursor().unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        use ratatui::backend::Backend;
        let cursor = terminal.backend_mut().get_cursor_position().unwrap();
        assert!(
            !text_area.contains(cursor),
            "no cursor in the text: {cursor:?}"
        );
        assert_eq!(corner(&mut app), (Color::DarkGray, Color::DarkGray));
        let bar = fn_bar(&mut app);
        assert!(bar.contains("F3View") && bar.contains("F4Edit"), "{bar}");
        // Copy and Move dimmed: their target pane is under the editor.
        let terminal = draw_app(&mut app);
        let buf = terminal.backend().buffer().clone();
        let row = buf.area.height - 1;
        let x = bar.find("Copy").unwrap() as u16;
        assert_eq!(buf[(x, row)].fg, Color::DarkGray);
        let x = bar.find("View").unwrap() as u16;
        assert_ne!(buf[(x, row)].fg, Color::DarkGray);
        let text = buffer_text(draw_app(&mut app).backend().buffer());
        assert!(!text.contains("Tab List"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_untitled_editor_is_titled_so() {
        let (mut app, dir) = editing_app("editor_untitled_title", "x\n");
        app.editor = Some(Editor::untitled());
        let text = buffer_text(draw_app(&mut app).backend().buffer());
        let title = text.lines().nth(1).unwrap();
        assert!(title.contains("\u{250c}Untitled\u{2500}"), "{text}");
        assert!(title.ends_with("Editing\u{2510}"), "{text}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_editing_label_marks_unsaved_changes() {
        let (mut app, dir) = editing_app("editor_label_modified", "x\n");
        let title = |app: &mut App| {
            let terminal = draw_app(app);
            buffer_text(terminal.backend().buffer())
                .lines()
                .nth(1)
                .unwrap()
                .to_string()
        };
        assert!(title(&mut app).ends_with("Editing\u{2510}"));
        app.editor
            .as_mut()
            .unwrap()
            .apply(editor::Command::Insert('y'));
        assert!(title(&mut app).ends_with("Editing*\u{2510}"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_selection_is_yellow_in_both_palettes() {
        for classic_style in [false, true] {
            let (mut app, dir) = editing_app("editor_selection_color", "kot i kot\n");
            app.classic_style = classic_style;
            app.editor.as_mut().unwrap().apply(editor::Command::Move {
                motion: editor::Motion::End,
                select: true,
            });
            let terminal = draw_app(&mut app);
            let buf = terminal.backend().buffer();
            let area = app.editor_text_area;
            let cell = &buf[(area.x, area.y)];
            assert_eq!(cell.bg, SELECTION_BG, "classic {classic_style}");
            assert_eq!(cell.fg, Color::Rgb(0, 0, 0), "classic {classic_style}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn wrap_lines_is_in_options_only_with_the_quick_view_open() {
        let (mut app, dir) = editing_app("options_wrap", "x\n");
        app.editor = None;
        let options = |app: &App| {
            let bar = app.menu_bar();
            let options = bar.iter().find(|c| c.title == "Options").unwrap();
            options.items.to_vec()
        };
        assert_eq!(options(&app), [Action::Settings]);
        app.run_action(Action::View).unwrap();
        assert_eq!(options(&app), [Action::WrapLines, Action::Settings]);
        // Drawn open, its items' click areas follow the bar shown.
        app.open_menu_at(1);
        draw_app(&mut app);
        assert_eq!(app.menu_item_tiles.len(), 2);
        app.close_menu();
        app.run_action(Action::Edit).unwrap();
        assert_eq!(options(&app), [Action::Settings], "the editor's is in Edit");
        app.run_action(Action::Close).unwrap();
        app.run_action(Action::View).unwrap();
        assert!(!app.quick_view);
        assert_eq!(options(&app), [Action::Settings]);
        app.open_menu_at(1);
        draw_app(&mut app);
        assert_eq!(app.menu_item_tiles.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_hint_says_what_esc_will_do() {
        let (mut app, dir) = editing_app("editor_esc_hint", "kot\n");
        app.editor.as_mut().unwrap().full_screen = true;
        let text = buffer_text(draw_app(&mut app).backend().buffer());
        assert!(text.contains("Esc Close"), "{text}");
        app.editor
            .as_mut()
            .unwrap()
            .apply(editor::Command::SelectAll);
        let text = buffer_text(draw_app(&mut app).backend().buffer());
        assert!(
            text.contains("Esc Done") && !text.contains("Esc Close"),
            "{text}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_find_leaves_no_highlights_behind() {
        let (mut app, dir) = editing_app("editor_find_closed", "kot i kot\n");
        let find = |app: &mut App, key| app.editor.as_mut().unwrap().find_key(key);
        app.editor.as_mut().unwrap().apply(editor::Command::Find);
        for c in "kot".chars() {
            find(&mut app, editor::FindKey::Insert(c));
        }
        let backgrounds = text_area_backgrounds(&mut app);
        assert!(backgrounds.contains(&FIND_MATCH_BG));

        // Esc: neither the matches nor the selection show any more.
        find(&mut app, editor::FindKey::Close);
        let backgrounds = text_area_backgrounds(&mut app);
        assert!(
            backgrounds
                .iter()
                .all(|&bg| bg != FIND_MATCH_BG && bg != SELECTION_BG),
            "{backgrounds:?}"
        );
        assert!(!buffer_text(draw_app(&mut app).backend().buffer()).contains("Find:"));

        // Enter: the match stays selected, the other one isn't marked.
        app.editor.as_mut().unwrap().apply(editor::Command::Find);
        find(&mut app, editor::FindKey::Accept);
        let backgrounds = text_area_backgrounds(&mut app);
        assert!(backgrounds.contains(&SELECTION_BG));
        assert!(!backgrounds.contains(&FIND_MATCH_BG));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn selection_and_find_matches_are_highlighted() {
        let (mut app, dir) = editing_app("editor_highlight", "kot i kot\n");
        app.editor.as_mut().unwrap().apply(editor::Command::Find);
        for c in "kot".chars() {
            app.editor
                .as_mut()
                .unwrap()
                .find_key(editor::FindKey::Insert(c));
        }
        let mut terminal = draw_app(&mut app);
        let buf = terminal.backend().buffer().clone();
        let text = buffer_text(&buf);
        assert!(text.contains("Find: kot"), "{text}");
        assert!(text.contains("1/2"), "{text}");
        let area = app.editor_text_area;
        // The current match is the selection; the other one is yellow.
        assert_eq!(buf[(area.x, area.y)].bg, SELECTION_BG);
        assert_eq!(buf[(area.x + 4, area.y)].bg, Color::Reset);
        assert_eq!(buf[(area.x + 6, area.y)].bg, FIND_MATCH_BG);
        // The cursor is in the find bar, after "kot".
        let (x, y) = cursor_of(&mut terminal);
        assert!(y > area.y + 1, "{y}");
        assert_eq!(x, area.x + " Find: kot".len() as u16);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wrapped_lines_continue_on_the_next_row() {
        let (mut app, dir) = editing_app("editor_wrap", &"slowo ".repeat(20));
        app.editor.as_mut().unwrap().set_wrap(true);
        let terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        let area = app.editor_text_area;
        let rows: Vec<String> = text
            .lines()
            .skip(area.y as usize)
            .take(4)
            .map(|row| {
                row.chars()
                    .skip(area.x as usize)
                    .take(area.width as usize)
                    .collect()
            })
            .collect();
        assert!(rows[0].starts_with("slowo slowo"), "{rows:?}");
        assert!(rows[1].starts_with("slowo"), "{rows:?}");
        assert!(rows.iter().all(|row| !row.ends_with("slo")), "{rows:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unsaved_changes_dialog_names_the_file() {
        let (mut app, dir) = editing_app("editor_unsaved", "x");
        app.dialog = Dialog::UnsavedChanges {
            quit_after: false,
            focused: 0,
        };
        let terminal = draw_app(&mut app);
        let text = buffer_text(terminal.backend().buffer());
        for expected in [
            "Save changes to \"notes.txt\"?",
            "[ Save ]",
            "[ Don't Save ]",
            "[ Cancel ]",
        ] {
            assert!(text.contains(expected), "missing {expected:?}:\n{text}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f1_opens_the_editor_help_page() {
        assert_eq!(
            HELP_PAGES[crate::app::EDITOR_HELP_PAGE].0,
            "Built-in editor"
        );
    }

    #[test]
    fn exact_width_name_is_unchanged() {
        let result = fit_name("1234567890", 10);
        assert_eq!(result, "1234567890");
    }

    #[test]
    fn opaque_item_counts_as_half_done_but_never_full() {
        let progress = crate::job::Progress {
            bytes_total: 1000,
            files_total: 1,
            opaque_bytes: 1000,
            ..Default::default()
        };
        assert_eq!(job_fraction(&progress), 0.5);
        let two_items = crate::job::Progress {
            bytes_done: 500,
            bytes_total: 1000,
            files_done: 1,
            files_total: 2,
            opaque_bytes: 500,
            ..Default::default()
        };
        assert_eq!(job_fraction(&two_items), 0.75);
    }

    /// Not a check but a measurement (`cargo test --release -- --ignored
    /// --nocapture huge_line`): how long a frame takes with a 10 MB file on
    /// one line, wrapped and not, idle and right after typing a letter.
    #[test]
    #[ignore]
    fn huge_line_frame_timings() {
        let text = "abc defgh ".repeat(1024 * 1024);
        let (mut app, dir) = editing_app("editor_huge_line", &text);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        for wrap in [false, true] {
            app.editor.as_mut().unwrap().set_wrap(wrap);
            for (place, motion) in [
                ("start", editor::Motion::DocStart),
                ("end", editor::Motion::DocEnd),
            ] {
                app.editor.as_mut().unwrap().apply(editor::Command::Move {
                    motion,
                    select: false,
                });
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let started = std::time::Instant::now();
                for _ in 0..5 {
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                }
                let idle = started.elapsed() / 5;
                let started = std::time::Instant::now();
                for _ in 0..5 {
                    app.editor
                        .as_mut()
                        .unwrap()
                        .apply(editor::Command::Insert('x'));
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                }
                let typing = started.elapsed() / 5;

                let started = std::time::Instant::now();
                for _ in 0..5 {
                    app.editor.as_mut().unwrap().apply(editor::Command::Move {
                        motion: editor::Motion::Left,
                        select: false,
                    });
                    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                }
                let moving = started.elapsed() / 5;
                println!(
                    "wrap {wrap}, cursor at {place}: idle frame {idle:?}, key + frame {typing:?}, arrow + frame {moving:?}"
                );
            }
        }
        let editor = app.editor.as_mut().unwrap();
        editor.apply(editor::Command::Find);
        let started = std::time::Instant::now();
        for c in "defg".chars() {
            editor.find_key(editor::FindKey::Insert(c));
        }
        println!("find: {:?} per letter typed", started.elapsed() / 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
