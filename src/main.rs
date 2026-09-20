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
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use signal_hook::consts::TERM_SIGNALS;
use signal_hook::flag;

use app::{App, Dialog, ExternalRequest};
use menu::{Action, FN_KEYS};

fn restore_terminal() {
    let _ = disable_raw_mode();
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

    enable_raw_mode().context("enable_raw_mode")?;
    let mut stdout = io::stdout();
    stdout
        .execute(EnterAlternateScreen)
        .context("EnterAlternateScreen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Terminal::new")?;

    let result = run(&mut terminal, &should_exit);

    restore_terminal();

    if let Err(err) = &result {
        logging::log_error(&format!("fatal: {err:?}"));
    }

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    should_exit: &AtomicBool,
) -> Result<()> {
    let mut app = App::new()?;

    while !app.should_quit && !should_exit.load(Ordering::Relaxed) {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key.code, key.modifiers)?;
        }
        app.sync_preview_scroll();
        app.reap_finished_children();

        if let Some(request) = app.external_request.take() {
            run_external(terminal, request, &mut app)?;
        }
    }

    app.save_state();
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
        ExternalRequest::RevealTerminal => reveal_terminal(terminal),
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
    let status = Command::new(&shell)
        .arg(shell_flag)
        .arg(command)
        .current_dir(cwd)
        .status();

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
fn reveal_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
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
    terminal.clear()?;
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    if app.help_open {
        app.help_open = false;
        return Ok(());
    }

    if app.dialog_is_text_input() {
        match code {
            KeyCode::Enter => app.confirm_dialog()?,
            KeyCode::Esc => app.cancel_dialog(),
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
            KeyCode::Enter => app.settings_save(),
            KeyCode::Esc | KeyCode::F(9) => app.settings_cancel(),
            _ => {}
        }
        return Ok(());
    }

    if !matches!(app.dialog, Dialog::None) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => app.confirm_dialog()?,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => app.cancel_dialog(),
            KeyCode::Enter => {
                if app.dialog_default_yes() {
                    app.confirm_dialog()?;
                } else {
                    app.cancel_dialog();
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
