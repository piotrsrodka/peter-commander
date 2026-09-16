mod app;
mod fs_ops;
mod menu;
mod pane;
mod state;
mod ui;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;

use app::{App, Dialog};
use menu::FN_KEYS;

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

fn main() -> Result<()> {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_panic_hook(info);
    }));

    let should_exit = Arc::new(AtomicBool::new(false));
    for signal in [SIGINT, SIGTERM, SIGHUP] {
        flag::register(signal, Arc::clone(&should_exit))?;
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &should_exit);

    restore_terminal();

    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    should_exit: &AtomicBool,
) -> Result<()> {
    let mut app = App::new()?;

    while !app.should_quit && !should_exit.load(Ordering::Relaxed) {
        terminal.draw(|frame| ui::draw(frame, &app))?;

        if event::poll(Duration::from_millis(200))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            handle_key(&mut app, key.code)?;
        }
    }

    app.save_state();
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode) -> Result<()> {
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
            KeyCode::F(10) => app.close_menu(),
            _ => {}
        }
        return Ok(());
    }

    match code {
        KeyCode::Char('q') | KeyCode::Char('Q') => app.quit(),
        KeyCode::Tab => app.toggle_active(),
        KeyCode::Up => app.active_pane().move_up(),
        KeyCode::Down => app.active_pane().move_down(),
        KeyCode::Enter => app.active_pane().enter_selected()?,
        KeyCode::F(n) => {
            if let Some(fn_key) = FN_KEYS.iter().find(|k| k.key == format!("F{n}")) {
                match fn_key.action {
                    Some(action) => app.run_action(action)?,
                    None => app.open_menu(),
                }
            }
        }
        _ => {}
    }
    Ok(())
}
