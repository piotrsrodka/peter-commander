use std::env;
use std::path::PathBuf;

use anyhow::Result;

use crate::fs_ops;
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
}

/// A request to suspend the TUI and hand the terminal to an external
/// process. `main` owns the `Terminal` so it performs the actual
/// suspend/spawn/resume; `App` just records what it wants run.
pub enum ExternalRequest {
    View(PathBuf),
    Edit(PathBuf),
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
        })
    }

    pub fn save_state(&self) {
        state::save(&self.left.cwd, &self.right.cwd);
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
            Action::Quit => self.quit(),
            other => {
                self.status_message = format!("{} is not implemented yet", other.label());
            }
        }
        Ok(())
    }

    pub fn open_selected(&mut self) -> Result<()> {
        if let Some(message) = self.active_pane().enter_selected()? {
            self.status_message = message;
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
                        Err(err) => self.status_message = format!("Delete failed: {err}"),
                    },
                    DialogKind::Copy => {
                        let dest = self.inactive_pane().cwd.join(&name);
                        match fs_ops::copy_recursive(&src, &dest) {
                            Ok(()) => {
                                self.status_message =
                                    format!("Copied {} to {}", src.display(), dest.display());
                            }
                            Err(err) => self.status_message = format!("Copy failed: {err}"),
                        }
                    }
                    DialogKind::Move => {
                        let dest = self.inactive_pane().cwd.join(&name);
                        match fs_ops::move_path(&src, &dest) {
                            Ok(()) => {
                                self.status_message =
                                    format!("Moved {} to {}", src.display(), dest.display());
                            }
                            Err(err) => self.status_message = format!("Move failed: {err}"),
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
                        Err(err) => self.status_message = format!("MkDir failed: {err}"),
                    },
                    TextInputKind::NewFile => match std::fs::File::create(&target) {
                        Ok(_) => self.status_message = format!("Created file {name}"),
                        Err(err) => self.status_message = format!("New file failed: {err}"),
                    },
                }

                self.dialog = Dialog::None;
                self.left.reload()?;
                self.right.reload()?;
            }
            Dialog::None => {}
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
            Dialog::TextInput { .. } | Dialog::None => false,
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
}
