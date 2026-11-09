mod app;
mod bulk_rename;
mod clipboard;
mod editor;
#[cfg(test)]
mod editor_spec_tests;
mod fs_ops;
mod image_preview;
mod job;
mod linux_console;
mod logging;
mod menu;
mod pane;
mod preview;
mod state;
mod transfer;
#[cfg(target_os = "linux")]
mod tty_image;
mod ui;

use std::env;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;
use signal_hook::consts::TERM_SIGNALS;
use signal_hook::flag;

use app::{App, Dialog, ExternalRequest, StartDirs};
use editor::Editor;
use menu::{Action, FN_KEYS};

fn restore_terminal() {
    let _ = io::stdout().execute(DisableBracketedPaste);
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(DisableMouseCapture);
    let _ = io::stdout().execute(LeaveAlternateScreen);
    // Hidden while drawing; on a panic nothing else would show it again.
    let _ = io::stdout().execute(crossterm::cursor::Show);
}

/// What the command line asks for, before any terminal setup.
#[derive(Debug, PartialEq, Eq)]
enum CliRequest {
    /// Start the UI, optionally in the given left/right pane directories
    /// (as typed; checked by `resolve_dir` once parsing is done).
    Run {
        left: Option<OsString>,
        right: Option<OsString>,
    },
    /// `--edit` (or run as `pce`): the built-in editor alone, on `file` (or
    /// an untitled text), at `line` (`+N`, counted from 1).
    Edit {
        line: Option<usize>,
        file: Option<OsString>,
    },
    Version,
    Help,
    /// A usage mistake, reported with the usage text and exit code 2.
    Invalid(String),
}

/// The command line as given — the program's name first, since run as
/// `pce` (a link to this binary, or the launcher) it's the editor alone,
/// as with `--edit`. Taken as the system gives it, not as UTF-8: a name
/// in another encoding is still a name.
fn parse_cli<I: IntoIterator<Item = OsString>>(argv: I) -> CliRequest {
    let mut argv = argv.into_iter();
    let program = argv.next().unwrap_or_default();
    let as_pce = Path::new(&program)
        .file_stem()
        .is_some_and(|stem| stem == "pce");
    if as_pce {
        parse_args(std::iter::once(OsString::from("--edit")).chain(argv))
    } else {
        parse_args(argv)
    }
}

/// `-V`/`-v`/`--version`/`-version` and `-h`/`--help` win wherever they
/// appear; otherwise up to two directories, for the left and right pane.
/// A bare `version` is a directory name like any other, so `pc version`
/// opens it. After `--` nothing is an option any more — sudoedit and
/// visudo call `$EDITOR -- FILE` (or `+N -- FILE`), and a name may start
/// with `-`.
fn parse_args<I: IntoIterator<Item = OsString>>(args: I) -> CliRequest {
    // Each name, and whether it came after `--`.
    let mut names: Vec<(OsString, bool)> = Vec::new();
    let mut invalid = None;
    let mut edit = false;
    let mut options_done = false;
    for arg in args {
        if options_done {
            names.push((arg, true));
            continue;
        }
        match arg.to_str() {
            Some("--") => options_done = true,
            Some("-V" | "-v" | "--version" | "-version") => return CliRequest::Version,
            Some("-h" | "--help") => return CliRequest::Help,
            Some("--edit") => edit = true,
            Some(option) if option.len() > 1 && option.starts_with('-') => {
                invalid.get_or_insert(format!("unknown option '{option}'"));
            }
            _ => names.push((arg, false)),
        }
    }
    if let Some(message) = invalid {
        return CliRequest::Invalid(message);
    }
    if edit {
        return parse_edit_args(names);
    }
    if names.len() > 2 {
        return CliRequest::Invalid(format!(
            "too many directories: expected at most 2 (left and right pane), got {}",
            names.len()
        ));
    }
    let mut dirs = names.into_iter().map(|(name, _)| name);
    CliRequest::Run {
        left: dirs.next(),
        right: dirs.next(),
    }
}

/// `--edit [+N] [FILE]`: a line to start on, as vim and nano take it, and
/// one file at most. A `+` not followed by digits is a file name, and so
/// is anything after `--`.
fn parse_edit_args(names: Vec<(OsString, bool)>) -> CliRequest {
    let mut names = names.into_iter().peekable();
    let line = names
        .next_if(|(name, after_dashes)| {
            !after_dashes
                && name.to_str().is_some_and(|name| {
                    name.len() > 1
                        && name.starts_with('+')
                        && name[1..].bytes().all(|b| b.is_ascii_digit())
                })
        })
        .and_then(|(name, _)| name.to_str()?[1..].parse().ok());
    let file = names.next().map(|(name, _)| name);
    if let Some((extra, _)) = names.next() {
        return CliRequest::Invalid(format!(
            "--edit takes one file, not also '{}'",
            extra.to_string_lossy()
        ));
    }
    CliRequest::Edit { line, file }
}

/// What `pce` (or `--edit`) starts: on a directory — `$EDITOR .`, as
/// tmux layouts and the like run it, expecting nvim's file browser — the
/// file manager, its left pane there, just as `peter-commander DIR` (`+N`
/// then means nothing); on anything else, the editor alone.
fn start_for_edit(file: Option<&Path>, line: Option<usize>) -> std::result::Result<Start, String> {
    if let Some(arg) = file
        && arg.is_dir()
    {
        return Ok(Start::Panes(StartDirs {
            left: Some(resolve_dir(arg)?),
            right: None,
        }));
    }
    Ok(Start::Editor(Box::new(editor_for(file, line)?)))
}

/// The editor `pce` starts with, or why it can't — said before the
/// terminal switches to the UI, so it lands in the shell (or in git's
/// output, as `core.editor`). A name that isn't there yet is a new file,
/// made on the first save.
fn editor_for(file: Option<&Path>, line: Option<usize>) -> std::result::Result<Editor, String> {
    let mut editor = match file {
        None => Editor::untitled(),
        Some(given) => {
            let arg = given.display();
            let path =
                std::path::absolute(given).map_err(|err| format!("cannot edit '{arg}': {err}"))?;
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_dir() => {
                    return Err(format!("cannot edit '{arg}': it's a directory"));
                }
                Ok(_) => {
                    Editor::read(&path).map_err(|why| format!("cannot edit '{arg}': {why}"))?
                }
                // New, in a directory that's there: made on the first save.
                // A link whose target isn't there yet is the same, the
                // target being the new file (the link stays a link).
                Err(err)
                    if err.kind() == io::ErrorKind::NotFound
                        && !given
                            .to_string_lossy()
                            .ends_with(['/', std::path::MAIN_SEPARATOR]) =>
                {
                    let target = fs_ops::link_target(&path);
                    if !target.parent().is_some_and(Path::is_dir) {
                        return Err(format!("cannot edit '{arg}': {err}"));
                    }
                    Editor::new_file(target)
                }
                Err(err) => return Err(format!("cannot edit '{arg}': {err}")),
            }
        }
    };
    if let Some(line) = line {
        editor.go_to_line(line);
    }
    Ok(editor)
}

/// Turns a directory argument into an absolute path, or says why it can't
/// be opened — checked before the terminal switches to the UI, so the
/// message lands in the shell the user typed the command in.
fn resolve_dir(arg: &Path) -> std::result::Result<PathBuf, String> {
    let path = arg;
    let arg = arg.display();
    let resolved = path
        .canonicalize()
        .map_err(|err| format!("cannot open '{arg}': {err}"))?;
    if !resolved.is_dir() {
        return Err(format!("'{arg}' is not a directory"));
    }
    Ok(resolved)
}

fn version_text() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

fn usage_text() -> String {
    let name = env!("CARGO_PKG_NAME");
    format!(
        "{}\n{}\n\n\
         Usage:\n  \
         {name}                      Start with the panes as usual (below)\n  \
         {name} LEFT_DIR             Open the left pane in LEFT_DIR\n  \
         {name} LEFT_DIR RIGHT_DIR   Open the left and right pane\n  \
         {name} --edit [+N] [FILE]   Just the editor, on FILE (at line N);\n  \
         pce [+N] [FILE]                      the same, also as $EDITOR\n  \
         pce DIR                              The panes, the left one in DIR\n  \
         {name} -h, --help           Print this help\n  \
         {name} -V, -v, --version    Print the version (-version works too)\n\n\
         As usual, the left pane starts in the current directory, or\n\
         where you left it with \"Restore last session\" (Options > Settings);\n\
         the right pane starts where you left it. First run: both in the\n\
         current directory.\n\n\
         The editor alone (pce) closes with Esc, F10 or Ctrl+Q, asking first\n\
         about unsaved changes. A FILE that isn't there yet is made on the\n\
         first save; without one, Save asks for a name. For git:\n  \
         git config --global core.editor pce\n",
        version_text(),
        env!("CARGO_PKG_DESCRIPTION"),
    )
}

fn main() -> Result<()> {
    let start = match parse_cli(env::args_os()) {
        CliRequest::Edit { line, file } => {
            match start_for_edit(file.as_deref().map(Path::new), line) {
                Ok(start) => start,
                Err(message) => {
                    eprintln!("pce: {message}");
                    std::process::exit(1);
                }
            }
        }
        CliRequest::Run { left, right } => {
            let resolve = |arg: Option<OsString>| {
                arg.as_deref()
                    .map(|arg| resolve_dir(Path::new(arg)))
                    .transpose()
            };
            match resolve(left).and_then(|left| {
                Ok(StartDirs {
                    left,
                    right: resolve(right)?,
                })
            }) {
                Ok(dirs) => Start::Panes(dirs),
                Err(message) => {
                    eprintln!("error: {message}");
                    std::process::exit(2);
                }
            }
        }
        CliRequest::Version => {
            println!("{}", version_text());
            return Ok(());
        }
        CliRequest::Help => {
            print!("{}", usage_text());
            return Ok(());
        }
        CliRequest::Invalid(message) => {
            eprint!("error: {message}\n\n{}", usage_text());
            std::process::exit(2);
        }
    };

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

    // Before the terminal is the UI's, so what goes wrong (a directory
    // that can't be listed) is said plainly in the shell.
    let app = match start {
        Start::Panes(start_dirs) => App::with_start_dirs(start_dirs),
        Start::Editor(editor) => App::editor_only(*editor),
    };
    let app = match app {
        Ok(app) => app,
        Err(err) => {
            eprintln!("error: {err:#}");
            std::process::exit(1);
        }
    };
    let mouse_capture_enabled = app.mouse_capture;

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
    enable_bracketed_paste();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Terminal::new")?;

    let result = run(&mut terminal, &should_exit, mouse_capture_enabled, app);

    restore_terminal();

    match result {
        Err(err) => {
            logging::log_error(&format!("fatal: {err:?}"));
            Err(err)
        }
        // `pce` ending after an in-place save failed part-way: whoever is
        // waiting for the editor (git, crontab, sudoedit) is told it
        // failed, so it doesn't take a damaged file.
        Ok(Some(damaged)) => {
            eprintln!(
                "pce: saving {} failed part-way; the file may be damaged",
                damaged.display()
            );
            std::process::exit(1);
        }
        Ok(None) => Ok(()),
    }
}

/// What `main` starts: the file manager, or (`pce`) the editor alone.
enum Start {
    Panes(StartDirs),
    Editor(Box<Editor>),
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    should_exit: &AtomicBool,
    mouse_capture_enabled: bool,
    mut app: App,
) -> Result<Option<PathBuf>> {
    // Tracks what's actually been sent to the terminal, so a change to the
    // live "Capture mouse" setting (toggled in the Settings dialog) can be
    // applied the moment it happens, not just at the next TUI suspend.
    let mut mouse_capture_applied = mouse_capture_enabled;
    let console = linux_console::Console::detect();

    while !app.should_quit && !should_exit.load(Ordering::Relaxed) {
        if app.mouse_capture != mouse_capture_applied {
            set_mouse_capture(app.mouse_capture)?;
            mouse_capture_applied = app.mouse_capture;
        }

        if app.image_preview_needs_detection() {
            app.image_preview.detect();
            // In case the terminal printed any of a query it doesn't know;
            // a terminal this quiet isn't worth quitting over.
            let _ = terminal.clear();
        }

        terminal.draw(|frame| {
            ui::draw(frame, &mut app);
            if let Some(console) = &console {
                console.adapt(frame.buffer_mut());
            }
        })?;

        // Redraw more often while a Copy/Move is running, so its progress
        // bar moves smoothly,
        // and a picture being prepared shows up as soon as it's ready.
        let poll_timeout = if app.image_preview.is_loading() {
            30
        } else if app.job.is_some() {
            100
        } else {
            200
        };
        if event::poll(Duration::from_millis(poll_timeout))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    handle_key(&mut app, key.code, key.modifiers)?;
                }
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse)?,
                Event::Paste(text) => handle_paste(&mut app, &text),
                _ => {}
            }
        }
        app.sync_preview_scroll();
        app.reap_finished_children();
        app.poll_job()?;
        app.auto_refresh(std::time::Instant::now());

        if let Some(request) = app.external_request.take() {
            run_external(terminal, request, &mut app, should_exit)?;
            // The suspend/resume path below re-applies mouse capture to
            // match `app.mouse_capture` on its own; keep this in sync so
            // the check above doesn't redundantly re-toggle it.
            mouse_capture_applied = app.mouse_capture;
        }
    }

    app.cancel_job_and_wait();
    app.save_state();
    Ok(app.damaged_file.take())
}

/// A paste then arrives as one `Event::Paste` instead of a stream of key
/// presses: the editor gets it as a single undoable insert, and a newline in
/// it can't run a half-typed command line. Best-effort: a terminal (or the
/// legacy Windows console) that doesn't do it just goes on typing the text.
fn enable_bracketed_paste() {
    let _ = io::stdout().execute(EnableBracketedPaste);
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
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] should_exit: &AtomicBool,
) -> Result<()> {
    match request {
        ExternalRequest::View(path) => {
            let default_pager = if cfg!(windows) { "notepad" } else { "less" };
            run_pager_or_editor(terminal, "PAGER", default_pager, &path, app).map(|_| ())
        }
        ExternalRequest::Edit(path) => {
            run_pager_or_editor(terminal, "EDITOR", default_editor(), &path, app).map(|_| ())
        }
        ExternalRequest::RenameSelected {
            list_file,
            dir,
            names,
        } => {
            let editor_ok =
                run_pager_or_editor(terminal, "EDITOR", default_editor(), &list_file, app)?;
            app.finish_bulk_rename(&list_file, dir, &names, editor_ok);
            Ok(())
        }
        ExternalRequest::Shell { command, cwd } => run_shell_command(terminal, &command, &cwd, app),
        ExternalRequest::RevealTerminal => reveal_terminal(terminal, app),
        #[cfg(target_os = "linux")]
        ExternalRequest::ShowImage(path) => show_tty_image(terminal, &path, app, should_exit),
    }
}

/// Shows an image full-screen on the tty (see `tty_image`) until Esc, `q`,
/// Ctrl+C or a termination signal, also across switches to other consoles
/// and back. Left/Right step through the other images in the active pane,
/// in its listing order, moving its cursor along so Esc lands on the last
/// one shown. The TUI stays in raw mode, so keys still arrive as crossterm
/// events; it only needs a full redraw once the console is back.
#[cfg(target_os = "linux")]
fn show_tty_image(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
    app: &mut App,
    should_exit: &AtomicBool,
) -> Result<()> {
    let shown = tty_image::decode(path).and_then(|picture| {
        let mut screen = tty_image::Screen::open()?;
        screen.draw(&picture)?;
        Ok(screen)
    });
    let mut screen = match shown {
        Ok(screen) => screen,
        Err(err) => {
            app.set_error(format!("Cannot show image: {err:#}"));
            return Ok(());
        }
    };
    // The next picture, decoded on another thread so console switches and
    // Esc are still answered meanwhile: its index, the direction (to go on
    // past a file that fails to decode) and where the result arrives.
    let mut decoding: Option<Decoding> = None;
    let closed: Result<()> = loop {
        if should_exit.load(Ordering::Relaxed) {
            break Ok(());
        }
        // Console switches wait for us, so answer them before anything else.
        if let Err(err) = screen.follow_console_switches() {
            break Err(err);
        }
        if let Some((idx, forward, rx)) = &decoding {
            let (idx, forward) = (*idx, *forward);
            match rx.try_recv() {
                Ok(Ok(picture)) => {
                    decoding = None;
                    if let Err(err) = screen.draw(&picture) {
                        break Err(err);
                    }
                    app.active_pane().selected = idx;
                }
                Ok(Err(_)) => {
                    let pane = app.active_pane();
                    decoding = next_image(&pane.entries, idx, forward)
                        .map(|next| decode_in_background(pane, next, forward));
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => decoding = None,
            }
        }
        let timeout = if decoding.is_some() { 30 } else { 100 };
        let key = match event::poll(Duration::from_millis(timeout))
            .and_then(|ready| ready.then(event::read).transpose())
        {
            Ok(Some(Event::Key(key))) if key.kind == KeyEventKind::Press => key,
            Ok(_) => continue,
            Err(err) => break Err(err.into()),
        };
        let forward = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => break Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
            KeyCode::Right => true,
            KeyCode::Left => false,
            _ => continue,
        };
        // One step at a time: presses during a decode are dropped, not
        // queued up to fire one after another once it's done.
        if decoding.is_none() {
            let pane = app.active_pane();
            decoding = next_image(&pane.entries, pane.selected, forward)
                .map(|next| decode_in_background(pane, next, forward));
        }
    };
    // A decode still running finishes on its own; its result is dropped.
    drop(screen);
    if let Err(err) = closed {
        app.set_error(format!("Image viewer closed: {err:#}"));
    }
    terminal.clear()?;
    Ok(())
}

#[cfg(target_os = "linux")]
type Decoding = (usize, bool, mpsc::Receiver<Result<image::RgbImage>>);

#[cfg(target_os = "linux")]
fn decode_in_background(pane: &pane::Pane, idx: usize, forward: bool) -> Decoding {
    let path = pane.cwd.join(&pane.entries[idx].name);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(tty_image::decode(&path));
    });
    (idx, forward, rx)
}

/// The nearest image file after (or before) `from` in `entries`, without
/// wrapping around.
#[cfg(target_os = "linux")]
fn next_image(entries: &[pane::Entry], from: usize, forward: bool) -> Option<usize> {
    let is_image = |&idx: &usize| {
        let entry: &pane::Entry = &entries[idx];
        !entry.is_dir && preview::is_image(&entry.name)
    };
    if forward {
        (from + 1..entries.len()).find(is_image)
    } else {
        (0..from).rev().find(is_image)
    }
}

fn default_editor() -> &'static str {
    if cfg!(windows) { "notepad" } else { "vi" }
}

/// Returns whether the program ran and exited successfully.
fn run_pager_or_editor(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    env_var: &str,
    default_program: &str,
    path: &Path,
    app: &mut App,
) -> Result<bool> {
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
    enable_bracketed_paste();
    terminal.clear()?;

    let succeeded = matches!(&status, Ok(status) if status.success());
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
    Ok(succeeded)
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
        (
            env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()),
            "-c",
        )
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
    enable_bracketed_paste();
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
        || app.job_visible
        || app.help_open
        || app.logs_open
        || app.about_open
        || app.dialog_is_settings()
        || !matches!(app.dialog, Dialog::None)
    {
        return Ok(());
    }

    if app.editor.is_some() && !app.menu_open && handle_editor_mouse(app, mouse)? {
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
        MouseEventKind::Down(MouseButton::Left) => {
            handle_mouse_click(app, mouse.column, mouse.row)?
        }
        _ => {}
    }
    Ok(())
}

/// The mouse while editing: the wheel over the editor scrolls it, a click in
/// its text gives it the keyboard and places its cursor (Shift+click or a
/// drag selects), a click in the file list beside it gives the keyboard to
/// the list, the menu bar opens the menu, and with the editor's keyboard an
/// F-key tile does what that key does there (nothing, for the dimmed ones).
/// Returns `false` for what's handled as if no editor were open: the wheel
/// over the list, and with the list's keyboard the F-key bar.
fn handle_editor_mouse(app: &mut App, mouse: MouseEvent) -> Result<bool> {
    let pos = Position::new(mouse.column, mouse.row);
    let area = app.editor_text_area;
    // Where in the text a press or drag lands, held to the text area so a
    // drag past its edge selects up to it.
    let column = mouse
        .column
        .clamp(area.x, area.right().saturating_sub(1).max(area.x));
    let row = mouse
        .row
        .clamp(area.y, area.bottom().saturating_sub(1).max(area.y));
    let (row, x) = ((row - area.y) as usize, (column - area.x) as usize);
    let list_area = match app.active {
        app::Side::Left => app.left_pane_area,
        app::Side::Right => app.right_pane_area,
    };
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if !app.editor_area.contains(pos) {
                return Ok(false);
            }
            if app.debounced_scroll() {
                return Ok(true);
            }
            const WHEEL_EDITOR_ROWS: isize = 3;
            let rows = if mouse.kind == MouseEventKind::ScrollUp {
                -WHEEL_EDITOR_ROWS
            } else {
                WHEEL_EDITOR_ROWS
            };
            if let Some(editor) = &mut app.editor {
                editor.scroll_view(rows);
            }
        }
        MouseEventKind::Down(MouseButton::Left) if area.contains(pos) => {
            app.set_list_focus(false);
            let extend = mouse.modifiers.contains(KeyModifiers::SHIFT);
            if let Some(editor) = &mut app.editor
                && editor.find.is_none()
            {
                editor.click(row, x, extend);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.list_focus {
                return Ok(false);
            }
            if let Some(editor) = &mut app.editor
                && editor.find.is_none()
            {
                editor.click(row, x, true);
            }
        }
        MouseEventKind::Down(MouseButton::Left) if list_area.contains(pos) => {
            app.set_list_focus(true);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if click_menu_bar(app, pos) {
                return Ok(true);
            }
            let tile = app.fn_key_tiles.iter().find(|(rect, _)| rect.contains(pos));
            match tile {
                Some(_) if app.list_focus => return Ok(false),
                Some(&(_, action)) => app.click_editor_fn_key(action)?,
                None => {}
            }
        }
        _ => {}
    }
    Ok(true)
}

/// A click on a menu bar title opens that menu, whether or not one is
/// already open (it then switches to it). Returns whether it was on one.
fn click_menu_bar(app: &mut App, pos: Position) -> bool {
    let category = app
        .menu_bar_tiles
        .iter()
        .find(|(rect, _)| rect.contains(pos))
        .map(|&(_, category)| category);
    if let Some(category) = category {
        app.open_menu_at(category);
    }
    category.is_some()
}

fn handle_mouse_click(app: &mut App, column: u16, row: u16) -> Result<()> {
    let pos = Position::new(column, row);

    if click_menu_bar(app, pos) {
        return Ok(());
    }

    if app.menu_open {
        let item = app
            .menu_item_tiles
            .iter()
            .find(|(rect, _)| rect.contains(pos))
            .map(|&(_, idx)| idx);
        if let Some(idx) = item {
            let action = app.menu_bar()[app.menu_category].items[idx];
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
            Some(Action::View) if app.preview_next_mode() => app.preview_find_next(true),
            Some(action) => app.run_action(action)?,
            None => app.open_menu(),
        }
    }
    Ok(())
}

/// Text the terminal pasted in one go (see `enable_bracketed_paste`): into
/// the editor (lines and all), a dialog's field, quick search or the command
/// line — whichever would get it typed. Nothing is ever run by a paste: a
/// one-line field takes just the first line (see `first_pasted_line`), and
/// says so when that left the rest out.
fn handle_paste(app: &mut App, text: &str) {
    if app.error_dialog.is_some()
        || app.job_visible
        || app.help_open
        || app.logs_open
        || app.about_open
        || app.menu_open
        || app.dialog_is_settings()
    {
        return;
    }
    if !app.dialog_has_field() && !matches!(app.dialog, Dialog::None) {
        return;
    }
    if app.editor_has_focus() && !app.dialog_has_field() {
        app.editor_paste(text);
        return;
    }
    let (line, count) = first_pasted_line(text);
    for c in line.chars() {
        if app.dialog_has_field() {
            app.dialog_field_insert(c);
        } else if app.quick_search.is_some() {
            app.quick_search_push(c);
        } else {
            app.command_line_push(c);
        }
    }
    if count > 1 {
        app.set_notice(format!(
            "Pasted 1 of {count} lines: only the first goes into a one-line field"
        ));
    }
}

/// What a paste leaves for a one-line field: its first line, without the
/// line break a copied line usually ends with, tabs as spaces and other
/// control characters dropped — and how many lines it had.
fn first_pasted_line(text: &str) -> (String, usize) {
    let text = text.trim_end_matches(['\n', '\r']);
    let count = text.replace("\r\n", "\n").split(['\n', '\r']).count();
    let line = text
        .split(['\n', '\r'])
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    (line, count)
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    if app.error_dialog.is_some() {
        app.error_dialog = None;
        return Ok(());
    }

    // The Copy/Move progress window sits on top of everything until it's
    // sent to the background (Esc, or its Background button).
    if app.job_visible {
        match code {
            KeyCode::Esc => app.hide_job(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => app.toggle_job_focus(),
            KeyCode::Enter => app.activate_job_button(),
            _ => {}
        }
        return Ok(());
    }

    if app.help_open {
        match code {
            KeyCode::Left => app.help_page = app.help_page.saturating_sub(1),
            KeyCode::Right => app.help_page = (app.help_page + 1).min(ui::HELP_PAGE_COUNT - 1),
            _ => app.help_open = false,
        }
        return Ok(());
    }

    if app.logs_open {
        app.logs_open = false;
        return Ok(());
    }

    if app.about_open {
        app.about_open = false;
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

    if app.dialog_has_field() {
        match code {
            KeyCode::Esc => app.cancel_dialog(),
            KeyCode::Enter => app.activate_dialog_button()?,
            KeyCode::Up => app.focus_dialog_field(),
            KeyCode::Down => app.focus_dialog_buttons(),
            KeyCode::Left => app.dialog_field_left(),
            KeyCode::Right => app.dialog_field_right(),
            KeyCode::Home => app.dialog_field_home(),
            KeyCode::End => app.dialog_field_end(),
            KeyCode::Backspace => app.dialog_field_backspace(),
            KeyCode::Delete => app.dialog_field_delete(),
            // AltGr letters arrive in several disguises (see there).
            KeyCode::Char(c) if transfer::is_text_input(c, modifiers) => app.dialog_field_insert(c),
            _ => {}
        }
        return Ok(());
    }

    if app.dialog_is_unsaved_changes() {
        match code {
            KeyCode::Esc => app.cancel_dialog(),
            KeyCode::Left | KeyCode::BackTab => app.step_unsaved_focus(false),
            KeyCode::Right | KeyCode::Tab => app.step_unsaved_focus(true),
            KeyCode::Enter => app.activate_unsaved_button(),
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
                if let Some(idx) = app.menu_category_for(c) {
                    app.open_menu_at(idx);
                }
            }
            _ => {}
        }
        return Ok(());
    }

    // The editor has the keyboard while it's open — after any dialog or
    // the menu on top of it, so those still work as usual — unless Tab gave
    // it to the file list beside it: then everything below is as with no
    // editor open, but Tab, which gives it back.
    if app.editor_has_focus() {
        return app.editor_key(code, modifiers);
    }
    if app.editor.is_some() && code == KeyCode::Tab {
        app.toggle_editor_focus();
        return Ok(());
    }

    if app.quick_search.is_some() {
        match code {
            KeyCode::Char(c)
                if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                app.quick_search_push(c);
                return Ok(());
            }
            KeyCode::Backspace => {
                app.quick_search_backspace();
                return Ok(());
            }
            KeyCode::Up => {
                app.quick_search_step(false);
                return Ok(());
            }
            KeyCode::Down => {
                app.quick_search_step(true);
                return Ok(());
            }
            KeyCode::Esc | KeyCode::Enter => {
                app.end_quick_search();
                return Ok(());
            }
            // Anything else ends the search and then does what it normally
            // does (e.g. F5 copies the entry just found).
            _ => app.end_quick_search(),
        }
    }

    // Find in the quick view, with the keyboard on it (Tab).
    if app.quick_view && app.preview_focus {
        if app.preview_find_bar_open() {
            if app.preview_find_key(code, modifiers) {
                return Ok(());
            }
            // With the bar open, the rest only scrolls the preview.
            if !matches!(
                code,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            ) {
                return Ok(());
            }
        } else {
            match code {
                // Not a text preview: Ctrl+F is the listing's quick search,
                // as with the focus there.
                KeyCode::Char('f')
                    if modifiers.contains(KeyModifiers::CONTROL) && app.open_preview_find() =>
                {
                    return Ok(());
                }
                KeyCode::F(3) if app.preview_next_mode() => {
                    app.preview_find_next(!modifiers.contains(KeyModifiers::SHIFT));
                    return Ok(());
                }
                _ => {}
            }
        }
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
        // Sideways only without Wrap Lines; Home/End stay top and bottom.
        KeyCode::Left if app.quick_view && app.preview_focus => app.scroll_preview_sideways(false),
        KeyCode::Right if app.quick_view && app.preview_focus => app.scroll_preview_sideways(true),
        KeyCode::Up => app.active_pane().move_up(),
        KeyCode::Down => app.active_pane().move_down(),
        // Classic Norton Commander: Insert tags the entry under the cursor
        // for F5/F6/F8 to act on as a batch, and steps down so repeated
        // presses sweep through a run of files.
        KeyCode::Insert => app.active_pane().toggle_mark_selected(),
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
        KeyCode::Esc => app.escape(),
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
        KeyCode::F(8) if modifiers.contains(KeyModifiers::SHIFT) => {
            app.run_action(Action::DeletePermanently)?;
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
        // stays correct if the menu bar is ever relabeled.
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::ALT) => {
            if let Some(idx) = app.menu_category_for(c) {
                app.open_menu_at(idx);
            }
        }
        KeyCode::Char('o') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.request_reveal_terminal();
        }
        KeyCode::Char('l') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.run_action(Action::ShowLogs)?;
        }
        KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.run_action(Action::QuickSearch)?;
        }
        KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.run_action(Action::ShowProgress)?;
        }
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            app.run_action(Action::Settings)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn next_image_skips_dirs_and_other_files() {
        let entry = |name: &str, is_dir: bool| pane::Entry {
            name: name.to_string(),
            is_dir,
            size: 0,
            modified: None,
            is_executable: false,
        };
        let entries = [
            entry("..", true),
            entry("a.jpg", false),
            entry("dir.png", true),
            entry("b.txt", false),
            entry("c.PNG", false),
            entry("d.webp", false),
        ];
        assert_eq!(next_image(&entries, 1, true), Some(4));
        assert_eq!(next_image(&entries, 4, true), Some(5));
        assert_eq!(next_image(&entries, 4, false), Some(1));
        assert_eq!(next_image(&entries, 5, true), None);
        assert_eq!(next_image(&entries, 1, false), None);
    }

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn run(left: Option<&str>, right: Option<&str>) -> CliRequest {
        CliRequest::Run {
            left: left.map(OsString::from),
            right: right.map(OsString::from),
        }
    }

    #[test]
    fn no_arguments_starts_the_ui() {
        assert_eq!(parse_args(args(&[])), run(None, None));
    }

    #[test]
    fn version_and_help_flags_are_recognised() {
        for flag in ["-V", "-v", "--version", "-version"] {
            assert_eq!(parse_args(args(&[flag])), CliRequest::Version, "{flag}");
        }
        assert_eq!(parse_args(args(&["-h"])), CliRequest::Help);
        assert_eq!(parse_args(args(&["--help"])), CliRequest::Help);
    }

    #[test]
    fn flags_win_over_directories() {
        assert_eq!(parse_args(args(&["~/a", "-v"])), CliRequest::Version);
        assert_eq!(parse_args(args(&["--nope", "-h"])), CliRequest::Help);
    }

    #[test]
    fn bare_version_is_a_directory() {
        assert_eq!(parse_args(args(&["version"])), run(Some("version"), None));
    }

    #[test]
    fn up_to_two_directories_fill_left_then_right() {
        assert_eq!(parse_args(args(&["."])), run(Some("."), None));
        assert_eq!(parse_args(args(&["a", "b"])), run(Some("a"), Some("b")));
        assert!(matches!(
            parse_args(args(&["a", "b", "c"])),
            CliRequest::Invalid(_)
        ));
    }

    #[test]
    fn unknown_options_are_rejected() {
        assert_eq!(
            parse_args(args(&["-x"])),
            CliRequest::Invalid("unknown option '-x'".into())
        );
        // A lone "-" isn't an option; it's treated as a (nonexistent) dir.
        assert_eq!(parse_args(args(&["-"])), run(Some("-"), None));
    }

    #[test]
    fn resolve_dir_accepts_directories_only() {
        let tmp = std::env::temp_dir().join(format!("pc-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("file.txt");
        std::fs::write(&file, "x").unwrap();

        assert_eq!(resolve_dir(&tmp), Ok(tmp.canonicalize().unwrap()));
        assert!(
            resolve_dir(&file)
                .unwrap_err()
                .contains("is not a directory")
        );
        assert!(
            resolve_dir(&tmp.join("missing"))
                .unwrap_err()
                .starts_with("cannot open")
        );

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn version_text_matches_the_package() {
        assert_eq!(
            version_text(),
            format!("peter-commander {}", env!("CARGO_PKG_VERSION"))
        );
    }

    /// An app with the left pane on a fresh directory holding `a.txt`
    /// (`text`), selected — and, with `edit`, open in the editor.
    fn app_on_a_file(name: &str, text: &str, edit: bool) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pc_test_main_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), text).unwrap();
        let mut app = App::new().unwrap();
        app.left = pane::Pane::new(dir.clone(), false).unwrap();
        app.left.select_name("a.txt");
        app.active = app::Side::Left;
        if edit {
            app.run_action(Action::Edit).unwrap();
            assert!(app.editor.is_some());
        }
        (app, dir)
    }

    fn press(app: &mut App, code: KeyCode) {
        handle_key(app, code, KeyModifiers::NONE).unwrap();
    }

    fn mkdir_field(app: &App) -> String {
        match &app.dialog {
            Dialog::TextInput { field, .. } => field.text().to_string(),
            _ => panic!("no MkDir dialog"),
        }
    }

    #[test]
    fn a_paste_onto_the_command_line_never_runs_it() {
        let (mut app, dir) = app_on_a_file("paste_command", "", false);
        handle_paste(&mut app, "ls\n");
        assert_eq!(app.command_line, "ls");
        assert!(app.error_dialog.is_none(), "one line: nothing to say");
        assert!(app.external_request.is_none());

        app.command_line.clear();
        handle_paste(&mut app, "ls\r\nrm x\n");
        assert_eq!(app.command_line, "ls");
        assert!(app.external_request.is_none());
        assert!(app.error_dialog_is_notice);
        assert!(app.error_dialog.take().unwrap().contains("1 of 2 lines"));

        app.command_line.clear();
        handle_paste(&mut app, "echo\ta\u{1b}b");
        assert_eq!(app.command_line, "echo ab");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_paste_into_a_dialog_field_takes_its_first_line() {
        let (mut app, dir) = app_on_a_file("paste_dialog", "", false);
        app.request_mkdir();
        handle_paste(&mut app, "nowy\nkatalog\n");
        assert_eq!(mkdir_field(&app), "nowy");
        assert!(app.error_dialog.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_paste_into_the_editor_keeps_its_lines_and_a_dialog_without_a_field_ignores_it() {
        let (mut app, dir) = app_on_a_file("paste_editor", "", true);
        handle_paste(&mut app, "one\ntwo\n");
        assert_eq!(app.editor.as_ref().unwrap().lines(), ["one", "two", ""]);
        assert!(app.error_dialog.is_none());

        press(&mut app, KeyCode::Esc);
        assert!(app.dialog_is_unsaved_changes());
        handle_paste(&mut app, "three");
        assert_eq!(app.editor.as_ref().unwrap().lines(), ["one", "two", ""]);
        assert!(app.dialog_is_unsaved_changes());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_unsaved_prompt_takes_the_keys_before_the_editor() {
        let (mut app, dir) = app_on_a_file("unsaved_keys", "x", true);
        press(&mut app, KeyCode::Char('y'));
        press(&mut app, KeyCode::Esc);
        assert!(app.dialog_is_unsaved_changes());
        // Typed with the prompt up, a letter reaches neither it nor the text.
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.dialog, Dialog::None));
        let editor = app.editor.as_ref().expect("Esc cancels: still editing");
        assert_eq!(editor.lines(), ["yx"]);
        assert!(editor.is_modified());

        // Esc, Right to Don't Save, Enter: closed, the file untouched.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        assert!(app.editor.is_none());
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn altgr_letters_type_into_dialog_fields_in_every_disguise() {
        let (mut app, dir) = app_on_a_file("altgr_dialog", "", false);
        app.request_mkdir();
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ] {
            handle_key(&mut app, KeyCode::Char('ó'), modifiers).unwrap();
        }
        // Alt+ASCII and Ctrl+letter stay shortcuts, not text.
        handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::ALT).unwrap();
        handle_key(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL).unwrap();
        assert_eq!(mkdir_field(&app), "óóó");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
        let event = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        handle_mouse(app, event).unwrap();
    }

    #[test]
    fn the_mouse_places_the_cursor_in_the_editor_and_drags_select() {
        let (mut app, dir) = app_on_a_file("editor_mouse", "hello\nworld\n", true);
        // As drawn: the text from (41, 2), 30 by 10 cells.
        app.editor_area = ratatui::layout::Rect::new(40, 1, 32, 14);
        app.editor_text_area = ratatui::layout::Rect::new(41, 2, 30, 10);
        app.menu_bar_tiles = vec![(ratatui::layout::Rect::new(1, 0, 6, 1), 1)];
        let down = MouseEventKind::Down(MouseButton::Left);
        let drag = MouseEventKind::Drag(MouseButton::Left);
        let cursor = |app: &App| app.editor.as_ref().unwrap().cursor();

        mouse(&mut app, down, 43, 3);
        assert_eq!(cursor(&app), editor::Pos::new(1, 2));
        // Dragged past the text area's left and top edges: held to them.
        mouse(&mut app, drag, 5, 0);
        assert_eq!(cursor(&app), editor::Pos::new(0, 0));
        assert_eq!(
            app.editor.as_ref().unwrap().selected_text().as_deref(),
            Some("hello\nwo")
        );
        // Far right and below: the end of the text.
        mouse(&mut app, drag, 79, 23);
        assert_eq!(cursor(&app), editor::Pos::new(1, 5));
        // Outside the text, a press only counts on the menu bar.
        mouse(&mut app, down, 79, 23);
        assert_eq!(cursor(&app), editor::Pos::new(1, 5));
        mouse(&mut app, down, 2, 0);
        assert!(app.menu_open);
        assert_eq!(app.menu_category, 1);
        app.close_menu();

        // With the find bar open, clicks leave the found text selected.
        mouse(&mut app, down, 41, 2);
        handle_key(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL).unwrap();
        for c in "wor".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let found = app.editor.as_ref().unwrap().selection();
        mouse(&mut app, down, 45, 2);
        mouse(&mut app, drag, 46, 3);
        assert_eq!(app.editor.as_ref().unwrap().selection(), found);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn left_and_right_scroll_an_unwrapped_preview_with_the_focus_on_it() {
        let (mut app, dir) = app_on_a_file("preview_keys", &"x".repeat(100), false);
        app.run_action(Action::View).unwrap();
        app.preview_visible_width = 38;
        app.wrap_lines = false;
        press(&mut app, KeyCode::Right);
        assert_eq!(app.preview_scroll_x, 0, "the listing has the focus");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.preview_scroll_x, 8);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.preview_scroll_x, 4);
        // Home stays "to the top", as before.
        press(&mut app, KeyCode::Home);
        assert_eq!((app.preview_scroll, app.preview_scroll_x), (0, 4));
        app.wrap_lines = true;
        press(&mut app, KeyCode::Right);
        assert_eq!(
            app.preview_scroll_x, 4,
            "wrapped text doesn't scroll sideways"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clicking_the_f_key_bar_while_editing_does_what_the_editor_does() {
        let (mut app, dir) = app_on_a_file("editor_fn_clicks", "x\n", true);
        app.editor_text_area = ratatui::layout::Rect::new(41, 2, 30, 10);
        let tile = |x| ratatui::layout::Rect::new(x, 23, 7, 1);
        app.fn_key_tiles = vec![
            (tile(32), Some(Action::Copy)),
            (tile(24), Some(Action::Edit)),
        ];
        let down = MouseEventKind::Down(MouseButton::Left);
        mouse(&mut app, down, 33, 23);
        assert!(app.job.is_none() && matches!(app.dialog, Dialog::None));
        assert!(!app.editor.as_ref().unwrap().full_screen);
        mouse(&mut app, down, 25, 23);
        assert!(app.editor.as_ref().unwrap().full_screen);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The quick view of `a.txt` (holding `text`) open, the keyboard on it.
    fn previewing(name: &str, text: &str) -> (App, PathBuf) {
        let (mut app, dir) = app_on_a_file(name, text, false);
        std::fs::write(dir.join("b.txt"), "kot b\n").unwrap();
        app.left.reload().unwrap();
        app.left.select_name("a.txt");
        press(&mut app, KeyCode::F(3));
        assert!(app.quick_view);
        app.sync_preview_scroll();
        app.preview_visible_lines = 10;
        app.preview_visible_width = 38;
        press(&mut app, KeyCode::Tab);
        assert!(app.preview_focus);
        (app, dir)
    }

    fn ctrl(app: &mut App, c: char) {
        handle_key(app, KeyCode::Char(c), KeyModifiers::CONTROL).unwrap();
    }

    fn preview_selection(app: &App) -> Option<(editor::Pos, editor::Pos)> {
        app.preview_find
            .as_ref()
            .and_then(|find| find.text.selection())
    }

    fn typed(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn ctrl_f_on_the_preview_finds_in_it_and_on_the_listing_searches_names() {
        let (mut app, dir) = previewing("preview_find_keys", "kot pies\nKOT\nkot\n");
        ctrl(&mut app, 'f');
        assert!(app.preview_find_bar_open());
        assert!(app.quick_search.is_none());
        typed(&mut app, "kot");
        assert_eq!(app.command_line, "", "typing goes to the bar");
        let status = |app: &App| {
            app.preview_find
                .as_ref()
                .unwrap()
                .text
                .find
                .as_ref()
                .unwrap()
                .status()
        };
        assert_eq!(status(&app), "1/3");
        press(&mut app, KeyCode::F(3));
        assert_eq!(status(&app), "2/3");
        handle_key(&mut app, KeyCode::F(3), KeyModifiers::SHIFT).unwrap();
        handle_key(&mut app, KeyCode::F(3), KeyModifiers::SHIFT).unwrap();
        assert_eq!(status(&app), "3/3", "wraps backwards");
        // Enter keeps the match; F3 goes on with the bar closed.
        press(&mut app, KeyCode::Enter);
        assert!(!app.preview_find_bar_open());
        assert_eq!(preview_selection(&app).unwrap().0, editor::Pos::new(2, 0));
        press(&mut app, KeyCode::F(3));
        assert_eq!(preview_selection(&app).unwrap().0, editor::Pos::new(0, 0));
        assert!(app.quick_view, "F3 is Next here, not closing the view");
        // Esc in steps: the match first, then as before (the command line).
        app.command_line = "ls".to_string();
        press(&mut app, KeyCode::Esc);
        assert_eq!(preview_selection(&app), None);
        assert_eq!(app.command_line, "ls");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.command_line, "");
        // On the listing Ctrl+F is the quick search, and F3 closes the view.
        press(&mut app, KeyCode::Tab);
        ctrl(&mut app, 'f');
        assert!(app.quick_search.is_some());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::F(3));
        assert!(!app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn esc_in_the_preview_find_bar_clears_everything() {
        let (mut app, dir) = previewing("preview_find_esc", "kot\n");
        ctrl(&mut app, 'f');
        typed(&mut app, "kot");
        assert!(preview_selection(&app).is_some());
        press(&mut app, KeyCode::Esc);
        assert!(!app.preview_find_bar_open());
        assert_eq!(preview_selection(&app), None);
        // Up/Down don't belong to the bar: they scroll the preview.
        ctrl(&mut app, 'f');
        press(&mut app, KeyCode::Down);
        assert!(app.preview_find_bar_open());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f3_on_the_preview_with_nothing_searched_closes_it_as_before() {
        let (mut app, dir) = previewing("preview_find_f3", "kot\n");
        assert!(!app.preview_next_mode());
        press(&mut app, KeyCode::F(3));
        assert!(!app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn another_previewed_file_drops_the_find_but_f3_still_knows_the_query() {
        let (mut app, dir) = previewing("preview_find_other", "kot\n");
        ctrl(&mut app, 'f');
        typed(&mut app, "kot");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        app.sync_preview_scroll();
        assert!(app.preview_find.is_none());
        press(&mut app, KeyCode::Tab);
        assert!(app.preview_next_mode());
        press(&mut app, KeyCode::F(3));
        let find = app.preview_find.as_ref().unwrap();
        assert_eq!(find.target, dir.join("b.txt"));
        assert_eq!(
            find.text.selection(),
            Some((editor::Pos::new(0, 0), editor::Pos::new(0, 3)))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_preview_scrolls_to_the_match_down_and_sideways() {
        let mut text = "x\n".repeat(80);
        text.push_str(&format!("{}kot\n", " ".repeat(200)));
        for wrap in [false, true] {
            let (mut app, dir) = previewing("preview_find_scroll", &text);
            app.wrap_lines = wrap;
            ctrl(&mut app, 'f');
            typed(&mut app, "kot");
            if wrap {
                // Line 80 starts at row 80; its match is some rows on.
                assert!(app.preview_scroll > 80 - 9, "{}", app.preview_scroll);
                assert_eq!(app.preview_scroll_x, 0);
            } else {
                // Nine rows show above the bar; the match's on the last.
                assert_eq!(app.preview_scroll, 80 - 8);
                assert!(app.preview_scroll_x > 200 - 38 && app.preview_scroll_x <= 200);
            }
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn ctrl_f_on_a_preview_that_is_not_text_is_the_quick_search() {
        let (mut app, dir) = app_on_a_file("preview_find_dir", "", false);
        std::fs::create_dir(dir.join("sub")).unwrap();
        app.left.reload().unwrap();
        app.left.select_name("sub");
        press(&mut app, KeyCode::F(3));
        press(&mut app, KeyCode::Tab);
        ctrl(&mut app, 'f');
        assert!(app.preview_find.is_none());
        assert!(app.quick_search.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An editor open on `a.txt` (changed, not saved) beside a list of
    /// `a.txt`, `b.txt` and `sub/`, the list's cursor on `a.txt`.
    fn editing_beside_a_list(name: &str) -> (App, PathBuf) {
        let (mut app, dir) = app_on_a_file(name, "jeden\ndwa\n", true);
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        app.left.reload().unwrap();
        app.left.select_name("a.txt");
        press(&mut app, KeyCode::Char('x'));
        assert!(app.editor.as_ref().unwrap().is_modified());
        (app, dir)
    }

    /// Every kind of key, with the modifiers that make a difference.
    fn all_keys() -> Vec<(KeyCode, KeyModifiers)> {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        let alt = KeyModifiers::ALT;
        let ctrl = KeyModifiers::CONTROL;
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let mut keys = Vec::new();
        for n in 1..=10 {
            for m in [none, shift, alt] {
                keys.push((KeyCode::F(n), m));
            }
        }
        for c in ['a', 'Z', '1', ' ', '/', 'ą', 'ó', 'ł', 'Ż'] {
            for m in [none, shift, alt, altgr] {
                keys.push((KeyCode::Char(c), m));
            }
        }
        for c in 'a'..='z' {
            keys.push((KeyCode::Char(c), ctrl));
        }
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Insert,
            KeyCode::Delete,
            KeyCode::Backspace,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
        ] {
            for m in [none, shift, ctrl] {
                keys.push((code, m));
            }
        }
        for code in [KeyCode::Tab, KeyCode::BackTab, KeyCode::Enter] {
            keys.push((code, alt));
        }
        keys
    }

    /// Whatever a key opened, closed again — without answering anything.
    fn settle(app: &mut App) {
        app.cancel_dialog();
        app.error_dialog = None;
        app.close_menu();
        app.help_open = false;
        app.logs_open = false;
        app.about_open = false;
        app.end_quick_search();
        app.external_request = None;
    }

    fn panes_snapshot(app: &App) -> String {
        format!(
            "{:?}",
            [&app.left, &app.right].map(|pane| (
                pane.cwd.clone(),
                pane.selected,
                {
                    let mut marked: Vec<_> = pane.marked.iter().cloned().collect();
                    marked.sort();
                    marked
                },
                pane.sort_key,
                pane.sort_reversed,
                pane.hide_hidden,
            ))
        ) + &format!("{:?}{}", app.active, app.command_line)
    }

    /// `editing_beside_a_list`, with all a leaked key could show itself on:
    /// something in the clipboard to paste, an indented line to unindent,
    /// and a selection to copy, cut or type over.
    fn matrix_fixture(name: &str) -> (App, PathBuf) {
        let (mut app, dir) = editing_beside_a_list(name);
        app.clipboard.copy("ZZZ".to_string());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Home);
        typed(&mut app, "    ");
        handle_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT).unwrap();
        assert!(app.editor.as_ref().unwrap().selection().is_some());
        (app, dir)
    }

    /// What a key left open, by kind, before `settle` closes it.
    fn opened(app: &App) -> String {
        let dialog = match &app.dialog {
            Dialog::None => "none".to_string(),
            Dialog::UnsavedChanges { .. } => "unsaved".to_string(),
            Dialog::Rename { src, .. } => format!("rename {}", src.display()),
            Dialog::TextInput { kind, .. } => format!("text {kind:?}"),
            Dialog::Settings { .. } => "settings".to_string(),
            Dialog::ConfirmQuit => "quit".to_string(),
            _ => "other dialog".to_string(),
        };
        format!(
            "{dialog}; external {}; menu {}; help {}",
            app.external_request.is_some(),
            if !app.menu_open {
                "closed"
            } else if app
                .menu_bar()
                .iter()
                .any(|category| category.title == "Edit")
            {
                "editor's"
            } else {
                "the panes'"
            },
            if app.help_open {
                app.help_page.to_string()
            } else {
                "closed".to_string()
            },
        )
    }

    #[test]
    fn with_the_list_focused_no_key_reaches_the_editor() {
        for (code, modifiers) in all_keys() {
            let (mut app, dir) = matrix_fixture("route_list");
            press(&mut app, KeyCode::Tab);
            assert!(app.list_focus);
            let before = app.editor.as_ref().unwrap().snapshot();
            let clipboard = app.clipboard.paste();
            handle_key(&mut app, code, modifiers).unwrap();
            settle(&mut app);
            let editor = app.editor.as_ref().expect("the editor stays open");
            assert_eq!(editor.snapshot(), before, "{code:?} {modifiers:?}");
            assert_eq!(app.clipboard.paste(), clipboard, "{code:?} {modifiers:?}");
            // Only Tab, and F4 on the edited file, give the keyboard back.
            // (Esc, F3, F10 and Ctrl+Q ask about the editor's changes —
            // answered with Cancel here — and the editor stays as it was.)
            let back = code == KeyCode::Tab
                || code == KeyCode::F(4) && !modifiers.contains(KeyModifiers::SHIFT);
            assert_eq!(app.list_focus, !back, "{code:?} {modifiers:?}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
        // A paste too.
        let (mut app, dir) = matrix_fixture("route_list_paste");
        press(&mut app, KeyCode::Tab);
        let before = app.editor.as_ref().unwrap().snapshot();
        handle_paste(&mut app, "ls -l");
        assert_eq!(app.editor.as_ref().unwrap().snapshot(), before);
        assert_eq!(app.command_line, "ls -l");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn with_the_editor_focused_no_key_reaches_the_list() {
        let edited = |dir: &Path| format!("rename {}", dir.join("a.txt").display());
        for (code, modifiers) in all_keys() {
            let (mut app, dir) = matrix_fixture("route_editor");
            let before = panes_snapshot(&app);
            handle_key(&mut app, code, modifiers).unwrap();
            // What the editor may open: its own prompt, the Rename of its
            // file, Save As, its help page and its menu bar — never a pane
            // dialog, nothing run, the panes' menus or help.
            let what = opened(&app);
            let (dialog, rest) = what.split_once(';').unwrap();
            assert!(
                ["none", "unsaved", "text SaveAs"].contains(&dialog) || dialog == edited(&dir),
                "{code:?} {modifiers:?} opened {what}"
            );
            assert!(
                rest.contains("external false"),
                "{code:?} {modifiers:?}: {what}"
            );
            assert!(
                !rest.contains("the panes'"),
                "{code:?} {modifiers:?}: {what}"
            );
            assert!(
                rest.ends_with("help closed")
                    || rest.ends_with(&format!("help {}", app::EDITOR_HELP_PAGE)),
                "{code:?} {modifiers:?}: {what}"
            );
            settle(&mut app);
            assert_eq!(panes_snapshot(&app), before, "{code:?} {modifiers:?}");
            assert!(app.quick_search.is_none() && !app.quick_view);
            std::fs::remove_dir_all(&dir).unwrap();
        }
        let (mut app, dir) = editing_beside_a_list("route_editor_paste");
        let before = panes_snapshot(&app);
        handle_paste(&mut app, "ls -l");
        assert_eq!(panes_snapshot(&app), before);
        assert_eq!(app.editor.as_ref().unwrap().lines()[0], "xls -ljeden");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_list_beside_the_editor_works_as_with_no_editor() {
        let (mut app, dir) = editing_beside_a_list("list_keys");
        press(&mut app, KeyCode::Tab);
        // Moving and marking, the command line, Esc clearing it — and never
        // closing the editor.
        press(&mut app, KeyCode::Down);
        assert_eq!(app.left.entries[app.left.selected].name, "b.txt");
        press(&mut app, KeyCode::Insert);
        assert!(app.left.marked.contains("b.txt"));
        typed(&mut app, "ls");
        assert_eq!(app.command_line, "ls");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.command_line, "");
        assert!(app.editor.is_some(), "the command line goes first");
        // Then Esc closes the editor, as from the editor: asking first.
        press(&mut app, KeyCode::Esc);
        assert!(app.dialog_is_unsaved_changes());
        press(&mut app, KeyCode::Esc);
        assert!(app.editor.is_some());
        // The usual menu bar and its items, F2 renaming the list's entry.
        assert_eq!(app.menu_bar()[0].items, menu::MENU_BAR[0].items);
        assert!(app.action_enabled(Action::MkDir) && app.action_enabled(Action::Delete));
        // Copy and Move would go to the other pane, under the editor.
        for (key, verb) in [(5, "Copy"), (6, "Move")] {
            assert!(!app.action_enabled(if key == 5 { Action::Copy } else { Action::Move }));
            press(&mut app, KeyCode::F(key));
            assert!(matches!(app.dialog, Dialog::None));
            assert!(app.error_dialog.take().unwrap().starts_with(verb));
        }
        press(&mut app, KeyCode::F(2));
        match &app.dialog {
            Dialog::Rename { src, .. } => assert_eq!(src, &dir.join("b.txt")),
            _ => panic!("no Rename dialog"),
        }
        app.cancel_dialog();
        // Ctrl+S is Settings here.
        ctrl(&mut app, 's');
        assert!(app.dialog_is_settings());
        app.settings_cancel();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn esc_on_the_list_closes_an_unchanged_editor_back_to_the_view_before() {
        let (mut app, dir) = app_on_a_file("list_esc_closes", "x\n", false);
        press(&mut app, KeyCode::F(3));
        press(&mut app, KeyCode::F(4));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Esc);
        assert!(app.editor.is_none());
        assert!(app.quick_view, "back to the quick view F4 replaced");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f3_beside_the_editor_closes_it_for_the_quick_view_asking_first() {
        let (mut app, dir) = editing_beside_a_list("list_f3");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        // Unsaved changes: asked first; Cancel leaves everything as it was.
        press(&mut app, KeyCode::F(3));
        assert!(app.dialog_is_unsaved_changes());
        press(&mut app, KeyCode::Esc);
        assert!(app.editor.is_some() && !app.quick_view && app.list_focus);
        // Don't Save: the quick view of the file under the cursor.
        press(&mut app, KeyCode::F(3));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        assert!(app.editor.is_none());
        assert!(app.quick_view && !app.preview_focus);
        assert_eq!(app.left.preview_target(), Some(dir.join("b.txt")));
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"jeden\ndwa\n");
        // Unchanged, no question.
        press(&mut app, KeyCode::Esc);
        assert!(!app.quick_view, "Esc closes the quick view");
        press(&mut app, KeyCode::F(4));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::F(3));
        assert!(app.editor.is_none() && app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f4_beside_the_editor_goes_back_to_it_or_opens_another_file() {
        let (mut app, dir) = editing_beside_a_list("list_f4");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::F(4));
        assert!(!app.list_focus, "on the edited file: back to it");
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("a.txt"));

        // Another file, with unsaved changes: asked first; Cancel keeps all.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::F(4));
        assert!(app.dialog_is_unsaved_changes());
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("a.txt"));
        assert!(app.editor.as_ref().unwrap().is_modified());
        // Don't Save: the other file, with the keyboard.
        press(&mut app, KeyCode::F(4));
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.path, dir.join("b.txt"));
        assert!(!editor.is_modified() && !app.list_focus);
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"jeden\ndwa\n");
        // Unchanged, the next one opens straight away; a directory doesn't.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::F(4));
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("a.txt"));
        press(&mut app, KeyCode::Tab);
        app.left.select_name("sub");
        press(&mut app, KeyCode::F(4));
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("a.txt"));
        assert!(app.list_focus);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quitting_from_the_list_still_asks_about_the_editors_changes() {
        for (code, modifiers) in [
            (KeyCode::F(10), KeyModifiers::NONE),
            (KeyCode::Char('q'), KeyModifiers::CONTROL),
        ] {
            let (mut app, dir) = editing_beside_a_list("list_quit");
            press(&mut app, KeyCode::Tab);
            handle_key(&mut app, code, modifiers).unwrap();
            assert!(matches!(
                app.dialog,
                Dialog::UnsavedChanges {
                    quit_after: true,
                    ..
                }
            ));
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    fn save_as_field(app: &App) -> String {
        match &app.dialog {
            Dialog::TextInput {
                kind: app::TextInputKind::SaveAs,
                field,
                ..
            } => field.text().to_string(),
            _ => panic!("no Save As dialog"),
        }
    }

    #[test]
    fn renamed_from_the_list_the_edited_file_is_saved_where_the_user_says() {
        let (mut app, dir) = editing_beside_a_list("list_rename_edited");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::F(2));
        for _ in 0.."a.txt".len() {
            press(&mut app, KeyCode::Backspace);
        }
        typed(&mut app, "c.txt");
        press(&mut app, KeyCode::Enter);
        // The editor stays on its file's old name, and says so.
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("a.txt"));
        assert!(app.error_dialog.take().unwrap().contains("renamed to"));
        // Ctrl+S asks where, suggesting the new name.
        press(&mut app, KeyCode::Tab);
        ctrl(&mut app, 's');
        assert_eq!(save_as_field(&app), "c.txt");
        // Taken (it's the renamed file): asked again before replacing it.
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.dialog, Dialog::ConfirmSaveAs { .. }));
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.path, dir.join("c.txt"));
        assert!(!editor.is_modified());
        assert_eq!(std::fs::read(dir.join("c.txt")).unwrap(), b"xjeden\ndwa\n");
        assert!(!dir.join("a.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deleted_from_the_list_the_edited_file_keeps_its_text_unsaved() {
        let (mut app, dir) = editing_beside_a_list("list_delete_edited");
        app.editor.as_mut().unwrap().mark_saved();
        app.use_trash = false;
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::F(8));
        app.confirm_dialog().unwrap();
        assert!(!dir.join("a.txt").exists());
        assert!(
            app.error_dialog
                .take()
                .unwrap()
                .contains("renamed or deleted")
        );
        assert!(app.editor.as_ref().unwrap().is_modified());
        // The note isn't said again on every refresh.
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none());
        // Save: where? The old name; saving there brings the file back.
        press(&mut app, KeyCode::Tab);
        ctrl(&mut app, 's');
        assert_eq!(save_as_field(&app), "a.txt");
        press(&mut app, KeyCode::Enter);
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"xjeden\ndwa\n");
        assert!(!app.editor.as_ref().unwrap().is_modified());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_changed_by_another_program_is_noticed_once() {
        let (mut app, dir) = editing_beside_a_list("outside_rename_edited");
        std::fs::rename(dir.join("a.txt"), dir.join("z.txt")).unwrap();
        // Missing once might be a program saving it anew: looked at again.
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none());
        std::fs::rename(dir.join("z.txt"), dir.join("a.txt")).unwrap();
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none(), "back: nothing to say");
        std::fs::rename(dir.join("a.txt"), dir.join("z.txt")).unwrap();
        app.refresh_from_disk();
        app.refresh_from_disk();
        let note = app.error_dialog.take().unwrap();
        assert!(note.contains("renamed or deleted"), "{note}");
        assert!(app.editor.as_ref().unwrap().is_modified());
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_replaced_by_another_one_is_noticed_and_saved_through_save_as() {
        let (mut app, dir) = editing_beside_a_list("outside_replace_edited");
        // Another program writes a new file over it, the way many save.
        std::fs::write(dir.join("new.tmp"), "theirs, longer\n").unwrap();
        std::fs::rename(dir.join("new.tmp"), dir.join("a.txt")).unwrap();
        app.refresh_from_disk();
        let note = app.error_dialog.take().unwrap();
        assert!(note.contains("replaced on disk"), "{note}");
        // Then deleted too: the text has no copy anywhere — unsaved.
        app.editor.as_mut().unwrap().mark_saved();
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        app.refresh_from_disk();
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none(), "said once");
        assert!(app.editor.as_ref().unwrap().is_modified());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mouse_gives_the_keyboard_to_what_it_clicks() {
        let (mut app, dir) = editing_beside_a_list("mouse_focus");
        app.left_pane_area = ratatui::layout::Rect::new(0, 1, 40, 20);
        app.editor_area = ratatui::layout::Rect::new(40, 1, 40, 20);
        app.editor_text_area = ratatui::layout::Rect::new(41, 2, 38, 15);
        let down = MouseEventKind::Down(MouseButton::Left);
        mouse(&mut app, down, 5, 5);
        assert!(app.list_focus);
        mouse(&mut app, down, 43, 2);
        assert!(!app.list_focus);
        assert_eq!(
            app.editor.as_ref().unwrap().cursor(),
            editor::Pos::new(0, 2)
        );
        // The wheel scrolls what's under it, the keyboard staying put.
        mouse(&mut app, down, 5, 5);
        let before = app.left.selected;
        mouse(&mut app, MouseEventKind::ScrollDown, 50, 5);
        assert!(app.list_focus);
        assert_eq!(app.left.selected, before);
        // (Past the wheel's debounce.)
        std::thread::sleep(std::time::Duration::from_millis(30));
        mouse(&mut app, MouseEventKind::ScrollDown, 5, 5);
        assert!(app.list_focus);
        assert_eq!(app.left.selected, before + 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_mouse_under_the_lists_keyboard_leaves_the_editor_to_the_wheel() {
        let text: String = (0..60).map(|n| format!("line {n}\n")).collect();
        let (mut app, dir) = app_on_a_file("mouse_list_focus", &text, true);
        app.left_pane_area = ratatui::layout::Rect::new(0, 1, 40, 20);
        app.editor_area = ratatui::layout::Rect::new(40, 1, 40, 20);
        app.editor_text_area = ratatui::layout::Rect::new(41, 2, 38, 15);
        app.fn_key_tiles = vec![(
            ratatui::layout::Rect::new(48, 23, 7, 1),
            Some(Action::MkDir),
        )];
        app.editor.as_mut().unwrap().scroll_into_view(15, 38);
        press(&mut app, KeyCode::Tab);
        let cursor = app.editor.as_ref().unwrap().cursor();
        // A drag doesn't reach into the editor.
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 45, 5);
        assert_eq!(app.editor.as_ref().unwrap().cursor(), cursor);
        assert!(app.list_focus);
        // An F-key tile is the list's: F7 makes a directory there.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 50, 23);
        assert!(matches!(app.dialog, Dialog::TextInput { .. }));
        app.cancel_dialog();
        // The wheel over the editor scrolls it, the keyboard staying put.
        let before = app.editor.as_ref().unwrap().snapshot();
        mouse(&mut app, MouseEventKind::ScrollDown, 50, 5);
        assert_ne!(app.editor.as_ref().unwrap().snapshot(), before);
        assert_eq!(app.editor.as_ref().unwrap().cursor(), cursor);
        assert!(app.list_focus);
        // A click into the text ends a quick search on the list, like Tab.
        ctrl(&mut app, 'f');
        assert!(app.quick_search.is_some());
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 43, 3);
        assert!(!app.list_focus && app.quick_search.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn edit(line: Option<usize>, file: Option<&str>) -> CliRequest {
        CliRequest::Edit {
            line,
            file: file.map(OsString::from),
        }
    }

    #[test]
    fn edit_mode_is_asked_for_by_flag_or_by_the_name_pce() {
        assert_eq!(parse_args(args(&["--edit"])), edit(None, None));
        assert_eq!(
            parse_args(args(&["--edit", "a.txt"])),
            edit(None, Some("a.txt"))
        );
        assert_eq!(
            parse_args(args(&["a.txt", "--edit"])),
            edit(None, Some("a.txt"))
        );
        assert_eq!(
            parse_args(args(&["--edit", "+12", "a"])),
            edit(Some(12), Some("a"))
        );
        // A `+` without a number is a name; a line without a file is fine.
        assert_eq!(parse_args(args(&["--edit", "+x"])), edit(None, Some("+x")));
        assert_eq!(parse_args(args(&["--edit", "+3"])), edit(Some(3), None));
        assert!(matches!(
            parse_args(args(&["--edit", "a", "b"])),
            CliRequest::Invalid(_)
        ));
        assert_eq!(parse_args(args(&["--edit", "-h"])), CliRequest::Help);
        // Run as `pce` (a link, or the launcher's child named so).
        assert_eq!(
            parse_cli(args(&["/usr/bin/pce", "+2", "f"])),
            edit(Some(2), Some("f"))
        );
        assert_eq!(parse_cli(args(&["pce.exe"])), edit(None, None));
        assert_eq!(
            parse_cli(args(&["peter-commander", "d"])),
            run(Some("d"), None)
        );
    }

    #[test]
    fn double_dash_ends_the_options_as_sudoedit_and_visudo_expect() {
        assert_eq!(
            parse_args(args(&["--edit", "--", "f"])),
            edit(None, Some("f"))
        );
        assert_eq!(
            parse_args(args(&["--edit", "+3", "--", "f"])),
            edit(Some(3), Some("f"))
        );
        // After `--`, a leading `-` (or `+`) is part of a name.
        assert_eq!(
            parse_args(args(&["--edit", "--", "-file"])),
            edit(None, Some("-file"))
        );
        assert_eq!(
            parse_args(args(&["--edit", "--", "+3"])),
            edit(None, Some("+3"))
        );
        assert_eq!(
            parse_args(args(&["--edit", "--", "-h"])),
            edit(None, Some("-h"))
        );
        assert_eq!(
            parse_cli(args(&["pce", "--", "/var/tmp/sudoers.XXXX"])),
            edit(None, Some("/var/tmp/sudoers.XXXX"))
        );
        assert_eq!(parse_args(args(&["--", "-dir"])), run(Some("-dir"), None));
    }

    #[cfg(unix)]
    #[test]
    fn a_name_that_isnt_utf8_is_a_name_not_a_crash() {
        use std::os::unix::ffi::OsStringExt;
        let latin2 = OsString::from_vec(b"zaba-\xbf.txt".to_vec());
        let parsed = parse_cli([OsString::from("pce"), latin2.clone()]);
        assert_eq!(
            parsed,
            CliRequest::Edit {
                line: None,
                file: Some(latin2.clone())
            }
        );
        let dir = std::env::temp_dir().join(format!("pc_test_pce_latin2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let editor = editor_for(Some(&dir.join(&latin2)), None).unwrap();
        assert!(editor.new_file);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_refuses_what_it_cant_edit_before_any_ui() {
        let dir = std::env::temp_dir().join(format!("pc_test_pce_refuse_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bin.dat"), b"\x00\x01").unwrap();
        std::fs::write(dir.join("latin2.txt"), b"\xb1").unwrap();
        let refused = |name: &str| editor_for(Some(&dir.join(name)), None);
        let why = |name: &str| refused(name).err().unwrap();
        assert!(why("bin.dat").contains("binary"));
        assert!(why("latin2.txt").contains("UTF-8"));
        assert!(refused("missing/new.txt").is_err(), "no such directory");
        // A new name in a directory that's there: an editor, nothing made.
        let editor = refused("new.txt").unwrap();
        assert!(editor.new_file && !dir.join("new.txt").exists());
        // +N: that line, or the last.
        std::fs::write(dir.join("lines.txt"), "a\nb\nc\n").unwrap();
        let path = dir.join("lines.txt");
        assert_eq!(
            editor_for(Some(&path), Some(2)).unwrap().cursor(),
            editor::Pos::new(1, 0)
        );
        assert_eq!(
            editor_for(Some(&path), Some(99)).unwrap().cursor(),
            editor::Pos::new(2, 0)
        );
        assert!(editor_for(None, None).unwrap().untitled);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `pce` on `dir/name` (made with `text` unless `None`).
    fn pce_on(dir_name: &str, name: &str, text: Option<&str>) -> (App, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("pc_test_pce_{dir_name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        if let Some(text) = text {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let editor = editor_for(Some(&dir.join(name)), None).unwrap();
        let mut app = App::editor_only(editor).unwrap();
        app.clipboard = clipboard::Clipboard::internal();
        (app, dir)
    }

    #[test]
    fn pce_edits_a_commit_message_and_ends_when_closed() {
        let (mut app, dir) = pce_on("commit", "COMMIT_EDITMSG", Some("\n# Please enter...\n"));
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(dir.join("COMMIT_EDITMSG")).unwrap().ino()
        };
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.full_screen && app.editor_only);
        let titles: Vec<&str> = app.menu_bar().iter().map(|c| c.title).collect();
        assert_eq!(titles, ["File", "Edit", "Options", "Command"]);
        let items: Vec<Action> = app
            .menu_bar()
            .iter()
            .flat_map(|c| c.items.iter().copied())
            .collect();
        assert!(!items.contains(&Action::ShowLogs) && items.contains(&Action::SaveAs));
        typed(&mut app, "Fix the thing");
        ctrl(&mut app, 's');
        press(&mut app, KeyCode::Esc);
        assert!(app.should_quit, "closed: the program ends");
        assert_eq!(
            std::fs::read_to_string(dir.join("COMMIT_EDITMSG")).unwrap(),
            "Fix the thing\n# Please enter...\n"
        );
        // Written in place: the same file, as crontab -e requires.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(dir.join("COMMIT_EDITMSG")).unwrap().ino(),
                inode
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_has_no_panes_to_switch_to_and_quits_without_asking_twice() {
        let (mut app, dir) = pce_on("keys", "a.txt", Some("x\n"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.editor.as_ref().unwrap().lines()[0], "    x");
        assert!(!app.list_focus);
        press(&mut app, KeyCode::F(4));
        assert!(
            app.editor.as_ref().unwrap().full_screen,
            "F4: no pane to go to"
        );
        press(&mut app, KeyCode::F(1));
        assert_eq!(app.help_page, app::EDITOR_HELP_PAGE);
        press(&mut app, KeyCode::Esc);
        // Changed: F10 asks; Don't Save ends it, the file as it was.
        press(&mut app, KeyCode::F(10));
        assert!(app.dialog_is_unsaved_changes());
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        assert!(app.should_quit);
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"x\n");
        // Unchanged: Ctrl+Q just quits, no "Really quit?".
        let (mut app, dir2) = pce_on("keys_clean", "a.txt", Some("x\n"));
        ctrl(&mut app, 'q');
        assert!(app.should_quit && matches!(app.dialog, Dialog::None));
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&dir2).unwrap();
    }

    #[test]
    fn pce_on_a_new_name_makes_the_file_only_when_saved() {
        let (mut app, dir) = pce_on("new", "new.txt", None);
        press(&mut app, KeyCode::Esc);
        assert!(app.should_quit && matches!(app.dialog, Dialog::None));
        assert!(!dir.join("new.txt").exists());
        let (mut app, _) = pce_on("new", "new.txt", None);
        typed(&mut app, "hi");
        ctrl(&mut app, 's');
        assert_eq!(std::fs::read(dir.join("new.txt")).unwrap(), b"hi\n");
        assert!(!app.editor.as_ref().unwrap().new_file);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_leaves_the_panes_saved_state_alone() {
        let config = std::env::temp_dir().join(format!("pc_test_pce_cfg_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&config);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("last_dirs"), "/left\n/right\n").unwrap();
        state::test_dir::set(Some(config.clone()));
        let (app, dir) = pce_on("state", "a.txt", Some("x"));
        let cwd = env::current_dir().unwrap();
        assert_eq!((&app.left.cwd, &app.right.cwd), (&cwd, &cwd));
        app.save_state();
        assert_eq!(
            std::fs::read_to_string(config.join("last_dirs")).unwrap(),
            "/left\n/right\n"
        );
        assert!(!config.join("sort").exists());
        state::test_dir::set(None);
        std::fs::remove_dir_all(&config).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_on_a_directory_starts_the_file_manager_there() {
        let dir = std::env::temp_dir().join(format!("pc_test_pce_dir_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let start = |arg: &Path, line| start_for_edit(Some(arg), line);
        let left_pane = |start| match start {
            Ok(Start::Panes(dirs)) => dirs.left.expect("the left pane's directory"),
            Ok(Start::Editor(_)) => panic!("the editor, not the panes"),
            Err(message) => panic!("{message}"),
        };
        let real = dir.canonicalize().unwrap();
        assert_eq!(left_pane(start(&dir, None)), real);
        // `+N` means nothing for a directory.
        assert_eq!(
            left_pane(start(&dir.join("sub"), Some(3))),
            real.join("sub")
        );
        // A link to a directory is one.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("sub"), dir.join("link")).unwrap();
            assert_eq!(left_pane(start(&dir.join("link"), None)), real.join("sub"));
        }
        // `.`: where it's run.
        assert_eq!(
            left_pane(start_for_edit(Some(Path::new(".")), None)),
            env::current_dir().unwrap().canonicalize().unwrap()
        );
        // A file, or a name not there yet, is still the editor.
        assert!(matches!(
            start(&dir.join("a.txt"), None),
            Ok(Start::Editor(_))
        ));
        match start(&dir.join("new.txt"), None) {
            Ok(Start::Editor(editor)) => assert!(editor.new_file),
            _ => panic!("a new file in the editor"),
        }
        assert!(matches!(start_for_edit(None, None), Ok(Start::Editor(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_asks_before_saving_over_a_new_file_someone_else_made() {
        let (mut app, dir) = pce_on("new_taken", "new.txt", None);
        typed(&mut app, "mine");
        std::fs::write(dir.join("new.txt"), "theirs").unwrap();
        ctrl(&mut app, 's');
        assert!(matches!(app.dialog, Dialog::ConfirmSaveAs { .. }));
        press(&mut app, KeyCode::Esc);
        assert_eq!(std::fs::read(dir.join("new.txt")).unwrap(), b"theirs");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_on_a_new_name_isnt_taken_for_a_vanished_file() {
        let (mut app, dir) = pce_on("new_quiet", "new.txt", None);
        app.refresh_from_disk();
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none());
        assert!(!app.editor.as_ref().unwrap().is_modified());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pce_on_a_dangling_link_makes_its_target_and_keeps_the_link() {
        let dir = std::env::temp_dir().join(format!("pc_test_pce_link_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink("target.txt", dir.join("link.txt")).unwrap();
        let editor = editor_for(Some(&dir.join("link.txt")), None).unwrap();
        assert!(editor.new_file);
        let mut app = App::editor_only(editor).unwrap();
        typed(&mut app, "via link");
        ctrl(&mut app, 's');
        assert!(
            matches!(app.dialog, Dialog::None),
            "no Replace for its own file"
        );
        assert_eq!(
            std::fs::read(dir.join("target.txt")).unwrap(),
            b"via link\n"
        );
        assert!(
            std::fs::symlink_metadata(dir.join("link.txt"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_refuses_big_unreadable_and_slashed_names() {
        let dir = std::env::temp_dir().join(format!("pc_test_pce_refuse2_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = std::fs::File::create(dir.join("big.txt")).unwrap();
        big.set_len(editor::MAX_FILE_BYTES + 1).unwrap();
        let why = |path: &Path| editor_for(Some(path), None).err().unwrap();
        assert!(why(&dir.join("big.txt")).contains("10 MB"));
        assert!(why(Path::new(&format!("{}/new.txt/", dir.display()))).contains("cannot edit"));
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(dir.join("secret.txt"), "x").unwrap();
            std::fs::set_permissions(
                dir.join("secret.txt"),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
            assert!(why(&dir.join("secret.txt")).contains("ermission"));
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pce_ends_in_failure_after_a_save_that_failed_part_way() {
        // /dev/full takes the open but no byte: a save that got going.
        let editor = Editor::from_bytes(PathBuf::from("/dev/full"), b"x")
            .ok()
            .unwrap();
        let mut app = App::editor_only(editor).unwrap();
        typed(&mut app, "y");
        ctrl(&mut app, 's');
        let error = app.error_dialog.take().unwrap();
        assert!(error.contains("may be damaged"), "{error}");
        assert_eq!(app.damaged_file.as_deref(), Some(Path::new("/dev/full")));
        // Quitting without a save that works: still failed, for main to say.
        ctrl(&mut app, 'q');
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Enter);
        assert!(app.should_quit);
        assert!(app.damaged_file.is_some());
    }
}
