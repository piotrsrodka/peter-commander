use std::env;
use std::path::PathBuf;

use anyhow::Result;

use crate::fs_ops;
use crate::logging;
use crate::menu::{Action, MENU_BAR};
use crate::pane::Pane;
use crate::state;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Delete,
    Copy,
    Move,
}

impl DialogKind {
    pub fn verb(&self) -> &'static str {
        match self {
            DialogKind::Delete => "Delete",
            DialogKind::Copy => "Copy",
            DialogKind::Move => "Move",
        }
    }

    /// Delete is destructive and defaults to "no"; Copy/Move default to "yes".
    pub fn default_yes(&self) -> bool {
        !matches!(self, DialogKind::Delete)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextInputKind {
    MkDir,
    NewFile,
}

impl TextInputKind {
    pub fn prompt(&self) -> &'static str {
        match self {
            TextInputKind::MkDir => "New directory name:",
            TextInputKind::NewFile => "New file name:",
        }
    }
}

pub enum Dialog {
    None,
    Confirm {
        kind: DialogKind,
        name: String,
        src: PathBuf,
    },
    TextInput {
        kind: TextInputKind,
        input: String,
    },
    ConfirmQuit,
    Settings {
        selected: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingItem {
    WaitAfterShellCommand,
}

impl SettingItem {
    pub const ALL: &'static [SettingItem] = &[SettingItem::WaitAfterShellCommand];

    pub fn label(&self) -> &'static str {
        match self {
            SettingItem::WaitAfterShellCommand => "Wait for Enter after running commands",
        }
    }
}

/// A request to suspend the TUI and hand the terminal to an external
/// process. `main` owns the `Terminal` so it performs the actual
/// suspend/spawn/resume; `App` just records what it wants run.
pub enum ExternalRequest {
    View(PathBuf),
    Edit(PathBuf),
    Shell { command: String, cwd: PathBuf },
    RevealTerminal,
}

pub struct App {
    pub left: Pane,
    pub right: Pane,
    pub active: Side,
    pub should_quit: bool,
    pub menu_open: bool,
    pub menu_category: usize,
    pub menu_item: usize,
    pub status_message: String,
    pub dialog: Dialog,
    pub external_request: Option<ExternalRequest>,
    pub help_open: bool,
    pub command_line: String,
    /// Whether running a command from the command line pauses with
    /// "Press Enter to continue" afterward, or returns straight to the
    /// panels (output can still be seen later via Ctrl+O).
    pub wait_after_shell_command: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let cwd = env::current_dir()?;
        let (left_dir, right_dir) = match state::load() {
            Some(last) => (last.left, last.right),
            None => (cwd.clone(), cwd),
        };
        Ok(App {
            left: Pane::new(left_dir)?,
            right: Pane::new(right_dir)?,
            active: Side::Left,
            should_quit: false,
            menu_open: false,
            menu_category: 0,
            menu_item: 0,
            status_message: String::new(),
            dialog: Dialog::None,
            external_request: None,
            help_open: false,
            command_line: String::new(),
            wait_after_shell_command: true,
        })
    }

    pub fn save_state(&self) {
        state::save(&self.left.cwd, &self.right.cwd);
    }

    /// Sets the status message and appends it to the error log. Use this for
    /// unexpected failures (I/O errors), not routine user-facing guidance.
    pub fn set_error(&mut self, message: String) {
        logging::log_error(&message);
        self.status_message = message;
    }

    /// Ctrl+O: reveal the real terminal underneath the panels (classic NC
    /// behavior) so previous command output/scrollback is visible again.
    pub fn request_reveal_terminal(&mut self) {
        self.external_request = Some(ExternalRequest::RevealTerminal);
    }

    /// Alt+F1: point the left pane at the right pane's current directory.
    pub fn sync_left_to_right_dir(&mut self) -> Result<()> {
        self.left.cwd = self.right.cwd.clone();
        self.left.selected = 0;
        if let Err(err) = self.left.reload() {
            self.set_error(format!("Cannot switch left pane: {err}"));
        }
        Ok(())
    }

    /// Alt+F2: point the right pane at the left pane's current directory.
    pub fn sync_right_to_left_dir(&mut self) -> Result<()> {
        self.right.cwd = self.left.cwd.clone();
        self.right.selected = 0;
        if let Err(err) = self.right.reload() {
            self.set_error(format!("Cannot switch right pane: {err}"));
        }
        Ok(())
    }

    pub fn active_pane(&mut self) -> &mut Pane {
        match self.active {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    pub fn inactive_pane(&mut self) -> &mut Pane {
        match self.active {
            Side::Left => &mut self.right,
            Side::Right => &mut self.left,
        }
    }

    pub fn toggle_active(&mut self) {
        self.active = match self.active {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        };
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn open_menu(&mut self) {
        self.menu_open = true;
        self.menu_category = 0;
        self.menu_item = 0;
    }

    pub fn close_menu(&mut self) {
        self.menu_open = false;
    }

    pub fn menu_left(&mut self) {
        if self.menu_category == 0 {
            self.menu_category = MENU_BAR.len() - 1;
        } else {
            self.menu_category -= 1;
        }
        self.menu_item = 0;
    }

    pub fn menu_right(&mut self) {
        self.menu_category = (self.menu_category + 1) % MENU_BAR.len();
        self.menu_item = 0;
    }

    pub fn menu_up(&mut self) {
        let len = MENU_BAR[self.menu_category].items.len();
        if self.menu_item == 0 {
            self.menu_item = len - 1;
        } else {
            self.menu_item -= 1;
        }
    }

    pub fn menu_down(&mut self) {
        let len = MENU_BAR[self.menu_category].items.len();
        self.menu_item = (self.menu_item + 1) % len;
    }

    pub fn confirm_menu_selection(&mut self) -> Result<()> {
        let action = MENU_BAR[self.menu_category].items[self.menu_item];
        self.menu_open = false;
        self.run_action(action)
    }

    pub fn run_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Open => self.open_selected()?,
            Action::Copy => self.request_copy(),
            Action::Move => self.request_move(),
            Action::Delete => self.request_delete(),
            Action::MkDir => self.request_mkdir(),
            Action::NewFile => self.request_new_file(),
            Action::View => self.request_external(ExternalRequest::View, "view"),
            Action::Edit => self.request_external(ExternalRequest::Edit, "edit"),
            Action::Help => self.help_open = true,
            Action::Quit => self.dialog = Dialog::ConfirmQuit,
            Action::Settings => self.dialog = Dialog::Settings { selected: 0 },
            other => {
                self.status_message = format!("{} is not implemented yet", other.label());
            }
        }
        Ok(())
    }

    pub fn dialog_is_settings(&self) -> bool {
        matches!(self.dialog, Dialog::Settings { .. })
    }

    pub fn setting_value(&self, item: SettingItem) -> bool {
        match item {
            SettingItem::WaitAfterShellCommand => self.wait_after_shell_command,
        }
    }

    fn toggle_setting(&mut self, item: SettingItem) {
        match item {
            SettingItem::WaitAfterShellCommand => {
                self.wait_after_shell_command = !self.wait_after_shell_command;
            }
        }
    }

    pub fn settings_move_up(&mut self) {
        if let Dialog::Settings { selected } = &mut self.dialog {
            *selected = if *selected == 0 {
                SettingItem::ALL.len() - 1
            } else {
                *selected - 1
            };
        }
    }

    pub fn settings_move_down(&mut self) {
        if let Dialog::Settings { selected } = &mut self.dialog {
            *selected = (*selected + 1) % SettingItem::ALL.len();
        }
    }

    pub fn settings_toggle_selected(&mut self) {
        if let Dialog::Settings { selected } = &self.dialog {
            let item = SettingItem::ALL[*selected];
            self.toggle_setting(item);
        }
    }

    pub fn open_selected(&mut self) -> Result<()> {
        if let Some(message) = self.active_pane().enter_selected()? {
            self.set_error(message);
        }
        Ok(())
    }

    fn request_copy(&mut self) {
        self.request_transfer(DialogKind::Copy);
    }

    fn request_move(&mut self) {
        self.request_transfer(DialogKind::Move);
    }

    fn request_transfer(&mut self, kind: DialogKind) {
        let pane = self.active_pane();
        let Some(src) = pane.selected_path() else {
            return;
        };
        let Some(entry) = pane.selected_entry() else {
            return;
        };
        self.dialog = Dialog::Confirm {
            kind,
            name: entry.name.clone(),
            src,
        };
    }

    fn request_external(&mut self, make: fn(PathBuf) -> ExternalRequest, verb: &str) {
        let pane = self.active_pane();
        let Some(entry) = pane.selected_entry() else {
            return;
        };
        if entry.is_dir {
            self.status_message = format!("Cannot {verb} a directory");
            return;
        }
        if let Some(path) = pane.selected_path() {
            self.external_request = Some(make(path));
        }
    }

    fn request_delete(&mut self) {
        let pane = self.active_pane();
        if let Some(path) = pane.selected_path()
            && let Some(entry) = pane.selected_entry()
        {
            self.dialog = Dialog::Confirm {
                kind: DialogKind::Delete,
                name: entry.name.clone(),
                src: path,
            };
        }
    }

    pub fn confirm_dialog(&mut self) -> Result<()> {
        match &self.dialog {
            Dialog::Confirm { kind, name, src } => {
                let kind = *kind;
                let name = name.clone();
                let src = src.clone();

                match kind {
                    DialogKind::Delete => match fs_ops::delete_recursive(&src) {
                        Ok(()) => self.status_message = format!("Deleted {name}"),
                        Err(err) => self.set_error(format!("Delete failed: {err}")),
                    },
                    DialogKind::Copy => {
                        let dest = self.inactive_pane().cwd.join(&name);
                        match fs_ops::copy_recursive(&src, &dest) {
                            Ok(()) => {
                                self.status_message =
                                    format!("Copied {} to {}", src.display(), dest.display());
                            }
                            Err(err) => self.set_error(format!("Copy failed: {err}")),
                        }
                    }
                    DialogKind::Move => {
                        let dest = self.inactive_pane().cwd.join(&name);
                        match fs_ops::move_path(&src, &dest) {
                            Ok(()) => {
                                self.status_message =
                                    format!("Moved {} to {}", src.display(), dest.display());
                            }
                            Err(err) => self.set_error(format!("Move failed: {err}")),
                        }
                    }
                }

                self.dialog = Dialog::None;
                self.left.reload()?;
                self.right.reload()?;
            }
            Dialog::TextInput { kind, input } => {
                let kind = *kind;
                let name = input.clone();
                if name.trim().is_empty() {
                    self.status_message = "Name cannot be empty".to_string();
                    self.dialog = Dialog::None;
                    return Ok(());
                }
                let target = self.active_pane().cwd.join(&name);

                match kind {
                    TextInputKind::MkDir => match std::fs::create_dir(&target) {
                        Ok(()) => self.status_message = format!("Created directory {name}"),
                        Err(err) => self.set_error(format!("MkDir failed: {err}")),
                    },
                    TextInputKind::NewFile => match std::fs::File::create(&target) {
                        Ok(_) => self.status_message = format!("Created file {name}"),
                        Err(err) => self.set_error(format!("New file failed: {err}")),
                    },
                }

                self.dialog = Dialog::None;
                self.left.reload()?;
                self.right.reload()?;
            }
            Dialog::ConfirmQuit => {
                self.dialog = Dialog::None;
                self.quit();
            }
            Dialog::Settings { .. } | Dialog::None => {}
        }
        Ok(())
    }

    pub fn cancel_dialog(&mut self) {
        self.dialog = Dialog::None;
    }

    pub fn dialog_is_text_input(&self) -> bool {
        matches!(self.dialog, Dialog::TextInput { .. })
    }

    pub fn request_mkdir(&mut self) {
        self.dialog = Dialog::TextInput {
            kind: TextInputKind::MkDir,
            input: String::new(),
        };
    }

    pub fn request_new_file(&mut self) {
        self.dialog = Dialog::TextInput {
            kind: TextInputKind::NewFile,
            input: String::new(),
        };
    }

    pub fn dialog_default_yes(&self) -> bool {
        match &self.dialog {
            Dialog::Confirm { kind, .. } => kind.default_yes(),
            Dialog::ConfirmQuit => true,
            Dialog::TextInput { .. } | Dialog::Settings { .. } | Dialog::None => false,
        }
    }

    pub fn text_input_push(&mut self, c: char) {
        if let Dialog::TextInput { input, .. } = &mut self.dialog {
            input.push(c);
        }
    }

    pub fn text_input_backspace(&mut self) {
        if let Dialog::TextInput { input, .. } = &mut self.dialog {
            input.pop();
        }
    }

    pub fn command_line_push(&mut self, c: char) {
        self.command_line.push(c);
    }

    pub fn command_line_backspace(&mut self) {
        self.command_line.pop();
    }

    pub fn command_line_clear(&mut self) {
        self.command_line.clear();
    }

    /// Submits the command line: if it holds a non-empty command, queues it
    /// as a shell request in the active pane's directory and clears the
    /// buffer, returning true. If it's empty, does nothing and returns
    /// false, so the caller can fall back to opening the selected entry
    /// instead (matching classic Norton Commander: Enter runs the typed
    /// command, or opens the selection when nothing was typed).
    pub fn submit_command_line(&mut self) -> bool {
        let command = self.command_line.trim().to_string();
        if command.is_empty() {
            return false;
        }
        self.command_line.clear();
        let cwd = self.active_pane().cwd.clone();
        self.external_request = Some(ExternalRequest::Shell { command, cwd });
        true
    }
}
