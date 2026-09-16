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

pub enum Dialog {
    None,
    Confirm {
        kind: DialogKind,
        name: String,
        src: PathBuf,
    },
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
        let Dialog::Confirm { kind, name, src } = &self.dialog else {
            return Ok(());
        };
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
        Ok(())
    }

    pub fn cancel_dialog(&mut self) {
        self.dialog = Dialog::None;
    }

    pub fn dialog_default_yes(&self) -> bool {
        match &self.dialog {
            Dialog::Confirm { kind, .. } => kind.default_yes(),
            Dialog::None => false,
        }
    }
}
