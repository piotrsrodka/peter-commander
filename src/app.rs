use std::env;

use anyhow::Result;

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
}

impl App {
    pub fn new() -> Result<Self> {
        let cwd = env::current_dir()?;
        Ok(App {
            left: Pane::new(cwd.clone())?,
            right: Pane::new(cwd)?,
            active: Side::Left,
            should_quit: false,
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
}
