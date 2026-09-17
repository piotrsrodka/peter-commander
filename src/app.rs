use std::env;
use std::path::PathBuf;

use anyhow::Result;

use crate::fs_ops;
use crate::logging;
use crate::menu::{Action, MENU_BAR};
use crate::pane::Pane;
use crate::preview;
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

    /// Delete and Move are destructive/hard to undo and default to "no";
    /// Copy is non-destructive and defaults to "yes".
    pub fn default_yes(&self) -> bool {
        matches!(self, DialogKind::Copy)
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
    Rename {
        input: String,
        src: PathBuf,
    },
    ConfirmQuit,
    Settings {
        selected: usize,
        /// Values as they were when the dialog opened, so Esc can revert
        /// toggles made during this session instead of just closing.
        original: Vec<bool>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingItem {
    WaitAfterShellCommand,
    HideHiddenFiles,
    InternalPreview,
}

impl SettingItem {
    pub const ALL: &'static [SettingItem] = &[
        SettingItem::WaitAfterShellCommand,
        SettingItem::HideHiddenFiles,
        SettingItem::InternalPreview,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            SettingItem::WaitAfterShellCommand => "Wait for Enter after running commands",
            SettingItem::HideHiddenFiles => "Hide hidden files/folders",
            SettingItem::InternalPreview => "F3 View uses the internal quick preview",
        }
    }

    /// Stable identifier used in the persisted settings file, independent
    /// of the display label so relabeling doesn't break saved settings.
    pub fn key(&self) -> &'static str {
        match self {
            SettingItem::WaitAfterShellCommand => "wait_after_shell_command",
            SettingItem::HideHiddenFiles => "hide_hidden_files",
            SettingItem::InternalPreview => "internal_preview",
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
    /// Whether the inactive pane shows a live preview of the active pane's
    /// selected entry instead of its own listing (F3, as in Total
    /// Commander's Quick View).
    pub quick_view: bool,
    /// While `quick_view` is on, whether Tab has moved keyboard focus onto
    /// the preview pane (so arrows scroll it) instead of the file listing.
    pub preview_focus: bool,
    /// Lines scrolled down within the current preview's in-memory buffer.
    pub preview_scroll: usize,
    /// The path the preview was last built for, so a change in selection
    /// (arrow keys, entering a directory, cd, ...) can reset the scroll
    /// position for the newly previewed entry.
    last_preview_target: Option<PathBuf>,
    /// Whether F3 (View) shows the internal quick preview or shells out to
    /// $PAGER/less, per the "F3 View uses the internal quick preview"
    /// setting.
    pub internal_preview: bool,
    /// Height (in text rows) of the preview pane in the last drawn frame,
    /// so scrolling can stop once the last line reaches the bottom of the
    /// visible area instead of scrolling it away entirely.
    pub preview_visible_lines: usize,
}

impl App {
    pub fn new() -> Result<Self> {
        let cwd = env::current_dir()?;
        let (left_dir, right_dir) = match state::load() {
            Some(last) => (last.left, last.right),
            None => (cwd.clone(), cwd),
        };
        let saved_settings = state::load_settings();
        let wait_after_shell_command = saved_settings
            .get(SettingItem::WaitAfterShellCommand.key())
            .copied()
            .unwrap_or(true);
        let hide_hidden_files = saved_settings
            .get(SettingItem::HideHiddenFiles.key())
            .copied()
            .unwrap_or(false);
        let internal_preview = saved_settings
            .get(SettingItem::InternalPreview.key())
            .copied()
            .unwrap_or(true);
        Ok(App {
            left: Pane::new(left_dir, hide_hidden_files)?,
            right: Pane::new(right_dir, hide_hidden_files)?,
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
            wait_after_shell_command,
            quick_view: false,
            preview_focus: false,
            preview_scroll: 0,
            last_preview_target: None,
            internal_preview,
            preview_visible_lines: 0,
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

    pub fn active_pane_ref(&self) -> &Pane {
        match self.active {
            Side::Left => &self.left,
            Side::Right => &self.right,
        }
    }

    /// Whether `action` would actually do something given the current
    /// selection, used to gray out inapplicable items in the pulldown menu.
    pub fn action_enabled(&self, action: Action) -> bool {
        let entry = self.active_pane_ref().selected_entry();
        let has_real_selection = entry.is_some_and(|e| e.name != "..");
        let selection_is_file = entry.is_some_and(|e| e.name != ".." && !e.is_dir);

        match action {
            Action::Open => entry.is_some_and(|e| e.is_dir),
            Action::Rename | Action::Copy | Action::Move | Action::Delete => has_real_selection,
            // View also works on directories (and "..") — quick-view shows
            // a name-only listing for those.
            Action::View => entry.is_some(),
            Action::Edit => selection_is_file,
            Action::MkDir | Action::NewFile | Action::Help | Action::Settings | Action::Quit => {
                true
            }
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

    pub fn toggle_preview_focus(&mut self) {
        self.preview_focus = !self.preview_focus;
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(1);
    }

    /// Stops at the point where the last line sits at the bottom of the
    /// preview pane, rather than letting it scroll away past the top.
    pub fn scroll_preview_down(&mut self) {
        let total_lines = preview::build_preview(self.active_pane_ref()).line_count();
        let max = total_lines.saturating_sub(self.preview_visible_lines);
        self.preview_scroll = (self.preview_scroll + 1).min(max);
    }

    /// Resets the preview scroll position whenever the previewed entry has
    /// changed since the last call (any kind of selection/cwd change).
    /// Called once per input loop iteration rather than at every individual
    /// call site that can move the selection, so nothing is missed.
    pub fn sync_preview_scroll(&mut self) {
        let target = self.active_pane_ref().preview_target();
        if target != self.last_preview_target {
            self.preview_scroll = 0;
            self.last_preview_target = target;
        }
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    pub fn open_menu(&mut self) {
        self.menu_open = true;
        self.menu_category = 0;
        self.menu_item = 0;
        self.snap_menu_item_to_enabled();
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
        self.snap_menu_item_to_enabled();
    }

    pub fn menu_right(&mut self) {
        self.menu_category = (self.menu_category + 1) % MENU_BAR.len();
        self.menu_item = 0;
        self.snap_menu_item_to_enabled();
    }

    /// Moves `self.menu_item` to the first enabled item at or after index 0,
    /// used whenever the menu/category first opens so the selection never
    /// starts on a grayed-out entry.
    fn snap_menu_item_to_enabled(&mut self) {
        let items = MENU_BAR[self.menu_category].items;
        if let Some(idx) = items.iter().position(|action| self.action_enabled(*action)) {
            self.menu_item = idx;
        }
    }

    pub fn menu_up(&mut self) {
        self.step_menu_item(false);
    }

    pub fn menu_down(&mut self) {
        self.step_menu_item(true);
    }

    /// Moves the selection one step up/down, skipping grayed-out (disabled)
    /// items so the cursor can never land on one. If every item in the
    /// category were disabled this would just leave the selection as-is,
    /// but MkDir/New File are always enabled so that can't happen in
    /// practice.
    fn step_menu_item(&mut self, forward: bool) {
        let items = MENU_BAR[self.menu_category].items;
        let len = items.len();
        if len == 0 {
            return;
        }
        let mut idx = self.menu_item;
        for _ in 0..len {
            idx = if forward {
                (idx + 1) % len
            } else if idx == 0 {
                len - 1
            } else {
                idx - 1
            };
            if self.action_enabled(items[idx]) {
                self.menu_item = idx;
                return;
            }
        }
    }

    pub fn confirm_menu_selection(&mut self) -> Result<()> {
        let action = MENU_BAR[self.menu_category].items[self.menu_item];
        self.menu_open = false;
        self.run_action(action)
    }

    pub fn run_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Open => self.open_selected()?,
            Action::Rename => self.request_rename(),
            Action::Copy => self.request_copy(),
            Action::Move => self.request_move(),
            Action::Delete => self.request_delete(),
            Action::MkDir => self.request_mkdir(),
            Action::NewFile => self.request_new_file(),
            Action::View => self.view_selected(),
            Action::Edit => self.request_external(ExternalRequest::Edit, "edit"),
            Action::Help => self.help_open = true,
            Action::Quit => self.dialog = Dialog::ConfirmQuit,
            Action::Settings => {
                let original = SettingItem::ALL
                    .iter()
                    .map(|item| self.setting_value(*item))
                    .collect();
                self.dialog = Dialog::Settings {
                    selected: 0,
                    original,
                };
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
            // Both panes are always kept in sync, so either one reflects
            // the current value.
            SettingItem::HideHiddenFiles => self.left.hide_hidden,
            SettingItem::InternalPreview => self.internal_preview,
        }
    }

    fn set_setting_value(&mut self, item: SettingItem, value: bool) {
        match item {
            SettingItem::WaitAfterShellCommand => self.wait_after_shell_command = value,
            SettingItem::HideHiddenFiles => {
                self.left.hide_hidden = value;
                self.right.hide_hidden = value;
                if let Err(err) = self.left.reload() {
                    self.set_error(format!("Cannot reload left pane: {err}"));
                }
                if let Err(err) = self.right.reload() {
                    self.set_error(format!("Cannot reload right pane: {err}"));
                }
            }
            SettingItem::InternalPreview => self.internal_preview = value,
        }
    }

    fn persist_settings(&self) {
        let pairs: Vec<(&str, bool)> = SettingItem::ALL
            .iter()
            .map(|item| (item.key(), self.setting_value(*item)))
            .collect();
        state::save_settings(&pairs);
    }

    pub fn settings_move_up(&mut self) {
        if let Dialog::Settings { selected, .. } = &mut self.dialog {
            *selected = if *selected == 0 {
                SettingItem::ALL.len() - 1
            } else {
                *selected - 1
            };
        }
    }

    pub fn settings_move_down(&mut self) {
        if let Dialog::Settings { selected, .. } = &mut self.dialog {
            *selected = (*selected + 1) % SettingItem::ALL.len();
        }
    }

    /// Toggles the selected checkbox in memory only — not persisted until
    /// `settings_save`, so `settings_cancel` can still revert it.
    pub fn settings_toggle_selected(&mut self) {
        if let Dialog::Settings { selected, .. } = &self.dialog {
            let item = SettingItem::ALL[*selected];
            let value = self.setting_value(item);
            self.set_setting_value(item, !value);
        }
    }

    /// Enter: persists whatever the checkboxes currently show and closes.
    pub fn settings_save(&mut self) {
        self.persist_settings();
        self.dialog = Dialog::None;
    }

    /// Esc/F9: reverts every setting to what it was when the dialog opened,
    /// discarding any toggles made in this session, then closes.
    pub fn settings_cancel(&mut self) {
        let Dialog::Settings { original, .. } = &self.dialog else {
            self.dialog = Dialog::None;
            return;
        };
        let original = original.clone();
        self.dialog = Dialog::None;
        for (item, value) in SettingItem::ALL.iter().zip(original.iter()) {
            self.set_setting_value(*item, *value);
        }
    }

    pub fn open_selected(&mut self) -> Result<()> {
        if let Some(message) = self.active_pane().enter_selected()? {
            self.set_error(message);
        }
        Ok(())
    }

    /// F2: on Linux, renaming *is* moving (both go through the same
    /// `rename(2)` syscall), so this just opens a text prompt pre-filled
    /// with the current name and reuses `fs_ops::move_path` within the same
    /// directory.
    fn request_rename(&mut self) {
        let pane = self.active_pane();
        let Some(entry) = pane.selected_entry() else {
            return;
        };
        let name = entry.name.clone();
        let Some(src) = pane.selected_path() else {
            return;
        };
        self.dialog = Dialog::Rename { input: name, src };
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

    /// F3: toggles the internal quick preview if that setting is on
    /// (keyboard focus stays on the file listing; Tab moves it onto the
    /// preview to scroll), closing it again on a second press regardless of
    /// which side currently has focus. Otherwise shells out to $PAGER/less
    /// as before.
    fn view_selected(&mut self) {
        if !self.internal_preview {
            self.request_external(ExternalRequest::View, "view");
            return;
        }
        if self.quick_view {
            self.quick_view = false;
            self.preview_focus = false;
            return;
        }
        if self.active_pane_ref().selected_entry().is_none() {
            return;
        }
        self.quick_view = true;
        self.preview_scroll = 0;
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
            Dialog::Rename { input, src } => {
                let new_name = input.clone();
                let src = src.clone();
                if new_name.trim().is_empty() {
                    self.status_message = "Name cannot be empty".to_string();
                    self.dialog = Dialog::None;
                    return Ok(());
                }
                match src.parent() {
                    Some(parent) => {
                        let dest = parent.join(&new_name);
                        match fs_ops::move_path(&src, &dest) {
                            Ok(()) => self.status_message = format!("Renamed to {new_name}"),
                            Err(err) => self.set_error(format!("Rename failed: {err}")),
                        }
                    }
                    None => self.set_error("Cannot rename: no parent directory".to_string()),
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
        matches!(
            self.dialog,
            Dialog::TextInput { .. } | Dialog::Rename { .. }
        )
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
            Dialog::TextInput { .. }
            | Dialog::Rename { .. }
            | Dialog::Settings { .. }
            | Dialog::None => false,
        }
    }

    pub fn text_input_push(&mut self, c: char) {
        match &mut self.dialog {
            Dialog::TextInput { input, .. } | Dialog::Rename { input, .. } => input.push(c),
            _ => {}
        }
    }

    pub fn text_input_backspace(&mut self) {
        match &mut self.dialog {
            Dialog::TextInput { input, .. } | Dialog::Rename { input, .. } => {
                input.pop();
            }
            _ => {}
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

        // `cd` is a shell builtin: run as `$SHELL -c "cd ..."` it would just
        // change a throwaway child process's directory and have no visible
        // effect at all. Handle it ourselves so it actually moves the pane.
        if let Some(target) = parse_cd_argument(&command) {
            match self.active_pane().change_dir(&target) {
                Ok(Some(message)) => self.set_error(message),
                Ok(None) => {}
                Err(err) => self.set_error(format!("cd failed: {err}")),
            }
            return true;
        }

        let cwd = self.active_pane().cwd.clone();
        self.external_request = Some(ExternalRequest::Shell { command, cwd });
        true
    }
}

/// Recognizes `cd`, `cd <path>`, `cd ~`, and `cd ~/path` on the command
/// line, returning the target directory (with `~` expanded, but otherwise
/// unresolved — resolution against the current directory happens in
/// `Pane::change_dir`). Returns `None` for anything else, including a word
/// that merely starts with "cd" (e.g. "cdiff").
fn parse_cd_argument(command: &str) -> Option<PathBuf> {
    let rest = command.strip_prefix("cd")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let arg = rest.trim();
    if arg.is_empty() || arg == "~" {
        return dirs::home_dir();
    }
    if let Some(suffix) = arg.strip_prefix("~/") {
        return Some(dirs::home_dir()?.join(suffix));
    }
    Some(PathBuf::from(arg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_cd_as_home() {
        assert_eq!(parse_cd_argument("cd"), dirs::home_dir());
        assert_eq!(parse_cd_argument("cd "), dirs::home_dir());
        assert_eq!(parse_cd_argument("cd ~"), dirs::home_dir());
    }

    #[test]
    fn parses_cd_with_path() {
        assert_eq!(parse_cd_argument("cd /tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(
            parse_cd_argument("cd some/relative/dir"),
            Some(PathBuf::from("some/relative/dir"))
        );
    }

    #[test]
    fn parses_cd_with_home_relative_path() {
        let expected = dirs::home_dir().map(|home| home.join("projects"));
        assert_eq!(parse_cd_argument("cd ~/projects"), expected);
    }

    #[test]
    fn does_not_match_non_cd_commands() {
        assert_eq!(parse_cd_argument("cdiff a b"), None);
        assert_eq!(parse_cd_argument("ls -la"), None);
        assert_eq!(parse_cd_argument("echo cd"), None);
    }
}
