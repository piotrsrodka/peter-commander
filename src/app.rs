use std::env;

use anyhow::Result;

use crate::menu::{Action, MENU_BAR};
use crate::pane::Pane;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
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
}

impl App {
    pub fn new() -> Result<Self> {
        let cwd = env::current_dir()?;
        Ok(App {
            left: Pane::new(cwd.clone())?,
            right: Pane::new(cwd)?,
            active: Side::Left,
            should_quit: false,
            menu_open: false,
            menu_category: 0,
            menu_item: 0,
            status_message: String::new(),
        })
    }

    pub fn active_pane(&mut self) -> &mut Pane {
        match self.active {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
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
            Action::Open => self.active_pane().enter_selected()?,
            Action::Quit => self.quit(),
            other => {
                self.status_message = format!("{} is not implemented yet", other.label());
            }
        }
        Ok(())
    }
}
