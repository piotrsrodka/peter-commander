mod app;
mod fs_ops;
mod logging;
mod menu;
mod pane;
mod preview;
mod state;
mod ui;

use std::env;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;
use signal_hook::consts::TERM_SIGNALS;
use signal_hook::flag;

use app::{App, Dialog, ExternalRequest, SettingItem};
use menu::{Action, FN_KEYS, MENU_BAR};

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(DisableMouseCapture);
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

fn main() -> Result<()> {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        logging::log_error(&format!("panic: {info}"));
        default_panic_hook(info);
    }));

    let should_exit = Arc::new(AtomicBool::new(false));
    // TERM_SIGNALS is itself platform-appropriate: no SIGHUP/SIGQUIT on
    // Windows, since those don't exist there.
    for &signal in TERM_SIGNALS {
        flag::register(signal, Arc::clone(&should_exit))?;
    }

    // Read directly rather than via App::new() (which happens later, inside
    // run()) so the very first frame already has mouse capture in the
    // state the user last left it in, instead of always starting enabled.
    let mouse_capture_enabled = state::load_settings()
        .get(SettingItem::MouseCapture.key())
        .copied()
        .unwrap_or(true);

    enable_raw_mode().context("enable_raw_mode")?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .context("EnterAlternateScreen")?;
    if mouse_capture_enabled {
        stdout
            .execute(EnableMouseCapture)
            .context("EnableMouseCapture")?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Terminal::new")?;

    let result = run(&mut terminal, &should_exit, mouse_capture_enabled);

    restore_terminal();

    if let Err(err) = &result {
        logging::log_error(&format!("fatal: {err:?}"));
    }

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    should_exit: &AtomicBool,
    mouse_capture_enabled: bool,
) -> Result<()> {
    let mut app = App::new()?;
    // Tracks what's actually been sent to the terminal, so a change to the
    // live "Capture mouse" setting (toggled in the Settings dialog) can be
    // applied the moment it happens, not just at the next TUI suspend.
    let mut mouse_capture_applied = mouse_capture_enabled;

    while !app.should_quit && !should_exit.load(Ordering::Relaxed) {
        if app.mouse_capture != mouse_capture_applied {
            set_mouse_capture(app.mouse_capture)?;
            mouse_capture_applied = app.mouse_capture;
        }

        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, key.code, key.modifiers)?;
                }
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse)?,
                _ => {}
            }
        }
        app.sync_preview_scroll();
        app.reap_finished_children();

        if let Some(request) = app.external_request.take() {
            run_external(terminal, request, &mut app)?;
            // The suspend/resume path below re-applies mouse capture to
            // match `app.mouse_capture` on its own; keep this in sync so
            // the check above doesn't redundantly re-toggle it.
            mouse_capture_applied = app.mouse_capture;
        }
    }

    app.save_state();
    Ok(())
}

fn set_mouse_capture(enabled: bool) -> Result<()> {
    if enabled {
        io::stdout()
            .execute(EnableMouseCapture)
            .context("EnableMouseCapture")?;
    } else {
        // Best-effort, like restore_terminal()'s disable: on Windows,
        // crossterm errors ("Initial console modes not set") disabling
        // mouse capture that was never enabled in this session (e.g.
        // "Capture mouse" was already off at startup) — there's nothing to
        // actually undo in that case, and it shouldn't be fatal.
        let _ = io::stdout().execute(DisableMouseCapture);
    }
    Ok(())
}

/// Suspends the TUI, runs an external program (pager/editor/shell command),
/// and restores the TUI once it exits.
fn run_external(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    request: ExternalRequest,
    app: &mut App,
) -> Result<()> {
    match request {
        ExternalRequest::View(path) => {
            let default_pager = if cfg!(windows) { "notepad" } else { "less" };
            run_pager_or_editor(terminal, "PAGER", default_pager, &path, app)
        }
        ExternalRequest::Edit(path) => {
            let default_editor = if cfg!(windows) { "notepad" } else { "vi" };
            run_pager_or_editor(terminal, "EDITOR", default_editor, &path, app)
        }
        ExternalRequest::Shell { command, cwd } => run_shell_command(terminal, &command, &cwd, app),
        ExternalRequest::RevealTerminal => reveal_terminal(terminal, app),
    }
}

fn run_pager_or_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    env_var: &str,
    default_program: &str,
    path: &Path,
    app: &mut App,
) -> Result<()> {
    // $EDITOR/$PAGER may be a full command line (e.g. "omarchy-launch-editor
    // --inline"), not just a bare program name, so parse it like a shell would.
    let command_line = env::var(env_var).unwrap_or_else(|_| default_program.to_string());
    let mut parts =
        shell_words::split(&command_line).unwrap_or_else(|_| vec![command_line.clone()]);
    if parts.is_empty() {
        parts.push(default_program.to_string());
    }
    let program = parts.remove(0);
    let args = parts;

    restore_terminal();
    let status = Command::new(&program).args(&args).arg(path).status();
    enable_raw_mode().context("enable_raw_mode")?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .context("EnterAlternateScreen")?;
    set_mouse_capture(app.mouse_capture)?;
    terminal.clear()?;

    match status {
        Ok(status) if !status.success() => {
            app.set_error(format!("{program} exited with {status}"));
        }
        Err(err) => {
            app.set_error(format!("Failed to launch {program}: {err}"));
        }
        Ok(_) => {}
    }

    app.left.reload()?;
    app.right.reload()?;
    Ok(())
}

/// Runs a shell command line (Norton Commander's built-in command prompt),
/// pausing for a keypress afterward so the user can read its output before
/// the TUI redraws over it.
fn run_shell_command(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    command: &str,
    cwd: &Path,
    app: &mut App,
) -> Result<()> {
    let (shell, shell_flag) = if cfg!(windows) {
        (
            env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string()),
            "/C",
        )
    } else {
        (env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()), "-c")
    };
    let wait = app.wait_after_shell_command;

    restore_terminal();
    println!("$ {command}");
    let mut cmd = Command::new(&shell);
    cmd.arg(shell_flag);
    #[cfg(windows)]
    {
        // cmd.exe parses its own quotes out of the raw command line, so the
        // quotes app.rs puts around an executable name must reach it
        // untouched. `.arg()` would otherwise backslash-escape them for
        // CreateProcess, leaving cmd.exe looking at `\"name\"` and failing
        // to find the program.
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(command);
    }
    #[cfg(not(windows))]
    {
        cmd.arg(command);
    }
    let status = cmd.current_dir(cwd).status();

    if wait {
        match &status {
            Ok(status) if !status.success() => println!("\n[exited with {status}]"),
            Err(err) => println!("\n[failed to launch {shell}: {err}]"),
            Ok(_) => {}
        }
        println!("\nPress Enter to continue...");
        let mut discard = String::new();
        let _ = io::stdin().read_line(&mut discard);
    }

    enable_raw_mode().context("enable_raw_mode")?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .context("EnterAlternateScreen")?;
    set_mouse_capture(app.mouse_capture)?;
    terminal.clear()?;

    match status {
        Ok(status) if !status.success() => {
            app.set_error(format!("Command exited with {status}"));
        }
        Err(err) => {
            app.set_error(format!("Failed to launch {shell}: {err}"));
        }
        Ok(_) => {}
    }

    app.left.reload()?;
    app.right.reload()?;
    Ok(())
}

/// Ctrl+O: reveals the real terminal underneath the panels — the same
/// screen that one-off shell commands print to — so its scrollback is
/// visible again, without spawning anything. Pressing Ctrl+O (or Esc)
/// again toggles back. Raw mode stays on throughout, so this is just an
/// alternate-screen flip, not a full terminal suspend/resume.
///
/// Deliberately draws nothing here: anything printed to this (primary)
/// screen becomes permanent scrollback once later output scrolls past it,
/// so even a "toast" hint ends up baked in as a stray line per use. Classic
/// Norton Commander doesn't overlay anything on the revealed screen either.
fn reveal_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &App) -> Result<()> {
    // Mouse capture would otherwise intercept the wheel instead of letting
    // the terminal scroll its own native scrollback, defeating the point.
    // Best-effort, like set_mouse_capture()'s disable branch: on Windows,
    // crossterm errors disabling mouse capture that was never enabled in
    // this session, which isn't fatal — it's already off either way.
    let _ = io::stdout().execute(DisableMouseCapture);
    io::stdout()
        .execute(LeaveAlternateScreen)
        .context("LeaveAlternateScreen")?;

    loop {
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let is_ctrl_o =
                key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL);
            if is_ctrl_o || key.code == KeyCode::Esc {
                break;
            }
        }
    }

    io::stdout()
        .execute(EnterAlternateScreen)
        .context("EnterAlternateScreen")?;
    set_mouse_capture(app.mouse_capture)?;
    terminal.clear()?;
    Ok(())
}

/// Which pane (if any) a screen position falls in, based on the areas
/// recorded during the last drawn frame.
fn side_at(app: &App, column: u16, row: u16) -> Option<app::Side> {
    let pos = Position::new(column, row);
    if app.left_pane_area.contains(pos) {
        Some(app::Side::Left)
    } else if app.right_pane_area.contains(pos) {
        Some(app::Side::Right)
    } else {
        None
    }
}

/// Mouse support: wheel scrolls a pane (or the quick-view preview, if that's
/// what's showing under the pointer) by exactly one row per notch, and
/// clicking an F-key bar tile does whatever pressing that key would do.
/// Ignored while a dialog, the pulldown menu, or help is open, matching how
/// `handle_key` gates those same keyboard shortcuts.
fn handle_mouse(app: &mut App, mouse: MouseEvent) -> Result<()> {
    if app.error_dialog.is_some()
        || app.help_open
        || app.logs_open
        || app.dialog_is_text_input()
        || app.dialog_is_settings()
        || !matches!(app.dialog, Dialog::None)
    {
        return Ok(());
    }

    match mouse.kind {
        // Only the active pane has a visible highlight, so scrolling the
        // inactive one would move its selection with no visible feedback.
        // Blocked while the menu is open too, same as the keyboard's
        // arrows are repurposed for menu navigation in that state.
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if !app.menu_open => {
            if app.debounced_scroll() {
                return Ok(());
            }
            if let Some(side) = side_at(app, mouse.column, mouse.row) {
                // The active side always shows the file list (whether or
                // not quick_view is on); when quick_view is on, the other
                // side shows the preview instead of a second list.
                let is_preview_side = app.quick_view && side != app.active;
                let is_list_side = side == app.active;
                let scroll_up = mouse.kind == MouseEventKind::ScrollUp;
                // Text scrolls faster per notch than the file list — 1 row
                // per click feels sluggish for reading, matching the usual
                // "a few lines per wheel click" convention.
                const WHEEL_PREVIEW_LINES: usize = 3;
                match (is_preview_side, is_list_side, scroll_up) {
                    (true, _, true) => {
                        for _ in 0..WHEEL_PREVIEW_LINES {
                            app.scroll_preview_up();
                        }
                    }
                    (true, _, false) => {
                        for _ in 0..WHEEL_PREVIEW_LINES {
                            app.scroll_preview_down();
                        }
                    }
                    (false, true, true) => app.pane_mut(side).move_up(),
                    (false, true, false) => app.pane_mut(side).move_down(),
                    (false, false, _) => {}
                }
            }
        }
        MouseEventKind::Down(MouseButton::Left) => handle_mouse_click(app, mouse.column, mouse.row)?,
        _ => {}
    }
    Ok(())
}

fn handle_mouse_click(app: &mut App, column: u16, row: u16) -> Result<()> {
    let pos = Position::new(column, row);

    // A menu bar label is clickable whether or not the menu is already
    // open — either opens it fresh on that category, or switches to it.
    let bar_category = app
        .menu_bar_tiles
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|&(_, category)| category);
    if let Some(category) = bar_category {
        app.open_menu_at(category);
        return Ok(());
    }

    if app.menu_open {
        let item = app
            .menu_item_tiles
            .iter()
            .find(|(rect, _)| rect.contains(pos))
            .map(|&(_, idx)| idx);
        if let Some(idx) = item {
            let action = MENU_BAR[app.menu_category].items[idx];
            if app.action_enabled(action) {
                app.menu_item = idx;
                app.confirm_menu_selection()?;
            }
        } else {
            // Clicked outside the bar and the dropdown: dismiss it, same
            // as clicking away from a menu does in any GUI.
            app.close_menu();
        }
        return Ok(());
    }

    let clicked = app
        .fn_key_tiles
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|(_, action)| *action);
    if let Some(clicked_action) = clicked {
        match clicked_action {
            Some(action) => app.run_action(action)?,
            None => app.open_menu(),
        }
    }
    Ok(())
}

/// The `MENU_BAR` category whose title starts with `letter` (case
/// insensitive), if any — used for the Alt+F/O/C mnemonic shortcuts.
fn menu_category_for(letter: char) -> Option<usize> {
    let lower = letter.to_ascii_lowercase();
    MENU_BAR
        .iter()
        .position(|category| category.title.to_ascii_lowercase().starts_with(lower))
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    if app.error_dialog.is_some() {
        app.error_dialog = None;
        return Ok(());
    }

    if app.help_open {
        match code {
            KeyCode::Left => app.help_page = 0,
            KeyCode::Right => app.help_page = 1,
            _ => app.help_open = false,
        }
        return Ok(());
    }

    if app.logs_open {
        app.logs_open = false;
        return Ok(());
    }

    if app.dialog_is_text_input() {
        match code {
            KeyCode::Enter => {
                if app.dialog_cancel_focused {
                    app.cancel_dialog();
                } else {
                    app.confirm_dialog()?;
                }
            }
            KeyCode::Esc => app.cancel_dialog(),
            KeyCode::Left | KeyCode::Right => app.toggle_dialog_focus(),
            KeyCode::Backspace => app.text_input_backspace(),
            KeyCode::Char(c) => app.text_input_push(c),
            _ => {}
        }
        return Ok(());
    }

    if app.dialog_is_settings() {
        match code {
            KeyCode::Up => app.settings_move_up(),
            KeyCode::Down => app.settings_move_down(),
            KeyCode::Char(' ') => app.settings_toggle_selected(),
            KeyCode::Left => app.settings_select_left(),
            KeyCode::Right => app.settings_select_right(),
            KeyCode::Enter => app.settings_save(),
            KeyCode::Esc | KeyCode::F(9) => app.settings_cancel(),
            _ => {}
        }
        return Ok(());
    }

    if !matches!(app.dialog, Dialog::None) {
        match code {
            KeyCode::Esc => app.cancel_dialog(),
            KeyCode::Left | KeyCode::Right => app.toggle_dialog_focus(),
            KeyCode::Enter => {
                if app.dialog_cancel_focused {
                    app.cancel_dialog();
                } else {
                    app.confirm_dialog()?;
                }
            }
            _ => {}
        }
        return Ok(());
    }

    if app.menu_open {
        match code {
            KeyCode::Esc => app.close_menu(),
            KeyCode::Left => app.menu_left(),
            KeyCode::Right => app.menu_right(),
            KeyCode::Up => app.menu_up(),
            KeyCode::Down => app.menu_down(),
            KeyCode::Enter => app.confirm_menu_selection()?,
            KeyCode::F(9) => app.close_menu(),
            // The mnemonic letter switches category even while the menu
            // is already open (with or without Alt still held), so Alt+F
            // then Alt+O jumps straight from File to Options.
            KeyCode::Char(c) => {
                if let Some(idx) = menu_category_for(c) {
                    app.open_menu_at(idx);
                }
            }
            _ => {}
        }
        return Ok(());
    }

    match code {
        KeyCode::Tab => {
            if app.quick_view {
                app.toggle_preview_focus();
            } else {
                app.toggle_active();
            }
        }
        KeyCode::Up if app.quick_view && app.preview_focus => app.scroll_preview_up(),
        KeyCode::Down if app.quick_view && app.preview_focus => app.scroll_preview_down(),
        KeyCode::Up => app.active_pane().move_up(),
        KeyCode::Down => app.active_pane().move_down(),
        KeyCode::Home if app.quick_view && app.preview_focus => app.scroll_preview_to_top(),
        KeyCode::End if app.quick_view && app.preview_focus => app.scroll_preview_to_bottom(),
        KeyCode::PageUp if app.quick_view && app.preview_focus => app.scroll_preview_page_up(),
        KeyCode::PageDown if app.quick_view && app.preview_focus => app.scroll_preview_page_down(),
        KeyCode::Home => app.active_pane().move_to_top(),
        KeyCode::End => app.active_pane().move_to_bottom(),
        KeyCode::PageUp => {
            let page = app.pane_visible_lines;
            app.active_pane().move_page_up(page);
        }
        KeyCode::PageDown => {
            let page = app.pane_visible_lines;
            app.active_pane().move_page_down(page);
        }
        KeyCode::Backspace => app.command_line_backspace(),
        KeyCode::Esc => app.command_line_clear(),
        // Classic Norton Commander: Enter runs whatever is typed on the
        // command line, or opens the selected entry if nothing was typed —
        // unless keyboard focus is on the quick-view preview, which has
        // nothing of its own to open.
        KeyCode::Enter => {
            let preview_has_focus = app.quick_view && app.preview_focus;
            if !app.submit_command_line() && !preview_has_focus {
                app.open_selected()?;
            }
        }
        KeyCode::F(4) if modifiers.contains(KeyModifiers::SHIFT) => {
            app.run_action(Action::NewFile)?;
        }
        KeyCode::F(1) if modifiers.contains(KeyModifiers::ALT) => {
            app.sync_left_to_right_dir()?;
        }
        KeyCode::F(2) if modifiers.contains(KeyModifiers::ALT) => {
            app.sync_right_to_left_dir()?;
        }
        // Alt+<first letter> jumps straight to that menu category — Alt+F
        // for File, Alt+O for Options, Alt+C for Command — matching each
        // category's title rather than hardcoding the letters, so this
        // stays correct if MENU_BAR is ever relabeled.
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::ALT) => {
            if let Some(idx) = menu_category_for(c) {
                app.open_menu_at(idx);
            }
        }
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_reveal_terminal();
        }
        // Some terminals (e.g. GNOME/ptyxis) intercept F10 for their own
        // menu and never forward it to us, leaving F10-only Quit
        // unreachable there. Ctrl+Q is a widely recognized "quit" shortcut
        // and gives those users a way out.
        KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.run_action(Action::Quit)?;
        }
        KeyCode::F(n) => {
            if let Some(fn_key) = FN_KEYS.iter().find(|k| k.key == format!("F{n}")) {
                match fn_key.action {
                    Some(action) => app.run_action(action)?,
                    None => app.open_menu(),
                }
            }
        }
        // Any other printable character is typed straight into the
        // always-visible command line, same as classic Norton Commander.
        KeyCode::Char(c) => app.command_line_push(c),
        _ => {}
    }
    Ok(())
}
