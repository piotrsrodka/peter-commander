use std::env;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::bulk_rename::{self, Plan};
use crate::fs_ops;
use crate::job::Job;
use crate::logging;
use crate::menu::{Action, MENU_BAR};
use crate::pane::{Pane, SortKey};
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
    /// Delete, but to the desktop trash/recycle bin instead of for good —
    /// what F8 does while the "Move to trash" setting is on.
    Trash,
    /// Trash where the crate can only do it by *copying* into the home
    /// trash (see `fs_ops::trash_needs_copy`) — same operation, but the
    /// confirmation says so first.
    TrashByCopy,
    Copy,
    Move,
}

impl DialogKind {
    pub fn verb(&self) -> &'static str {
        match self {
            DialogKind::Delete => "Delete",
            DialogKind::Trash | DialogKind::TrashByCopy => "Trash",
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
    SelectFiles,
    UnselectFiles,
}

impl TextInputKind {
    pub fn title(&self) -> &'static str {
        match self {
            TextInputKind::MkDir => "New directory",
            TextInputKind::NewFile => "New file",
            TextInputKind::SelectFiles => "Select",
            TextInputKind::UnselectFiles => "Unselect",
        }
    }

    pub fn prompt(&self) -> &'static str {
        match self {
            TextInputKind::MkDir => "New directory name:",
            TextInputKind::NewFile => "New file name:",
            TextInputKind::SelectFiles => "Select files matching (e.g. *.jpg *.png):",
            TextInputKind::UnselectFiles => "Unselect files matching (e.g. *.jpg *.png):",
        }
    }
}

pub enum Dialog {
    None,
    Confirm {
        kind: DialogKind,
        /// `(display name, full source path)` for every item this confirms
        /// acting on — one entry for the common single-selection case, or
        /// every marked item when the active pane has tags.
        items: Vec<(String, PathBuf)>,
    },
    /// Shown after a Copy/Move confirm when some items already exist in the
    /// destination, so nothing is replaced without being asked. Carries the
    /// whole batch so Overwrite can run it unchanged.
    ConfirmOverwrite {
        kind: DialogKind,
        items: Vec<(String, PathBuf)>,
        /// Names from `items` whose destination already exists.
        conflicts: Vec<String>,
    },
    TextInput {
        kind: TextInputKind,
        input: String,
    },
    Rename {
        input: String,
        src: PathBuf,
    },
    /// Rename Selected, after the editor closed with a valid edit: shows what
    /// will change and — defaulting to Cancel — which existing files would
    /// be replaced, before anything is touched.
    ConfirmRenameSelected {
        dir: PathBuf,
        plan: Plan,
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
    MouseCapture,
    ClassicStyle,
    StartLeftInCwd,
    UseTrash,
    ColumnHeaders,
    PaneTotals,
}

impl SettingItem {
    pub const ALL: &'static [SettingItem] = &[
        SettingItem::WaitAfterShellCommand,
        SettingItem::HideHiddenFiles,
        SettingItem::InternalPreview,
        SettingItem::MouseCapture,
        SettingItem::ClassicStyle,
        SettingItem::StartLeftInCwd,
        SettingItem::UseTrash,
        SettingItem::ColumnHeaders,
        SettingItem::PaneTotals,
    ];

    /// The two sides of this setting's switch, left then right: each
    /// side's label and the value choosing it sets. Which side holds
    /// `true` is per setting, so labels can be swapped here freely without
    /// touching the stored flag's name or meaning.
    pub fn sides(&self) -> [(&'static str, bool); 2] {
        match self {
            SettingItem::WaitAfterShellCommand => {
                [("Return immediately", false), ("Wait for Enter", true)]
            }
            SettingItem::HideHiddenFiles => {
                [("Show hidden files", false), ("Hide hidden files", true)]
            }
            SettingItem::InternalPreview => {
                [("External pager (F3)", false), ("Built-in preview", true)]
            }
            SettingItem::MouseCapture => [("Terminal mouse", false), ("App mouse clicks", true)],
            SettingItem::ClassicStyle => [
                ("Terminal theme colors", false),
                ("Classic NC colors", true),
            ],
            SettingItem::StartLeftInCwd => [
                ("Restore last session", false),
                ("Start in launch dir", true),
            ],
            SettingItem::UseTrash => [
                ("F8 deletes permanently", false),
                ("F8 moves to trash", true),
            ],
            SettingItem::ColumnHeaders => [("Show column headers", true), ("No headers", false)],
            SettingItem::PaneTotals => [("Show status bar", true), ("Plain panes", false)],
        }
    }

    /// Stable identifier used in the persisted settings file, independent
    /// of the display label so relabeling doesn't break saved settings.
    pub fn key(&self) -> &'static str {
        match self {
            SettingItem::WaitAfterShellCommand => "wait_after_shell_command",
            SettingItem::HideHiddenFiles => "hide_hidden_files",
            SettingItem::InternalPreview => "internal_preview",
            SettingItem::MouseCapture => "mouse_capture",
            SettingItem::ClassicStyle => "classic_style",
            SettingItem::StartLeftInCwd => "start_left_in_cwd",
            SettingItem::PaneTotals => "pane_totals",
            SettingItem::UseTrash => "use_trash",
            SettingItem::ColumnHeaders => "column_headers",
        }
    }
}

/// A request to suspend the TUI and hand the terminal to an external
/// process. `main` owns the `Terminal` so it performs the actual
/// suspend/spawn/resume; `App` just records what it wants run.
pub enum ExternalRequest {
    View(PathBuf),
    Edit(PathBuf),
    Shell {
        command: String,
        cwd: PathBuf,
    },
    RevealTerminal,
    /// Open `list_file` (the names, one per line) in $EDITOR, then hand
    /// the result to `App::finish_bulk_rename`.
    RenameSelected {
        list_file: PathBuf,
        dir: PathBuf,
        names: Vec<String>,
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
    /// Which button of the current dialog's `[ Primary ]  [ Cancel ]` row
    /// has keyboard focus — Left/Right toggle it, Enter activates whichever
    /// is focused. Reset by each `request_*`/dialog-opening call to that
    /// dialog's safe default (`false` = primary, matching `y`/`n` shortcuts
    /// still working independently of focus).
    pub dialog_cancel_focused: bool,
    pub external_request: Option<ExternalRequest>,
    pub help_open: bool,
    /// Which of the two help screen pages is shown (0 or 1) — Left/Right
    /// switch between them while help is open.
    pub help_page: usize,
    /// Command > Show Logs: a full-screen view of the recent contents of
    /// the error/status log file, closed by any key like help.
    pub logs_open: bool,
    /// A blocking "in your face" popup for the most recent error (e.g.
    /// "Access is denied" entering a locked directory) — unlike
    /// `status_message`, which only reaches the log file, this demands a
    /// keypress to dismiss so it can't be missed. Not shown for routine
    /// confirmations (`set_status`), only real failures (`set_error`).
    pub error_dialog: Option<String>,
    pub command_line: String,
    /// While Command > Quick Search is active: what's been typed so far
    /// (shown in place of the command line), the cursor jumping to the
    /// first entry whose name contains it.
    pub quick_search: Option<String>,
    /// The Copy/Move running on a worker thread, if any (only one at a
    /// time).
    pub job: Option<Job>,
    /// Whether that job's progress window is shown on top; `false` once
    /// it's been sent to the background.
    pub job_visible: bool,
    /// Which button of the progress window has focus: Background (`false`)
    /// or Cancel (`true`).
    pub job_cancel_focused: bool,
    /// Quit was confirmed while a job that can't stop mid-item (a Trash
    /// that copies) was running: it's been told to cancel, and the app
    /// quits as soon as it has finished the item it was on.
    pub quit_when_job_done: bool,
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
    /// Whether the terminal's mouse reporting should be claimed at all.
    /// When off, scroll/click inside PC do nothing, but the terminal's own
    /// native mouse behavior (its scrollback, text selection, any
    /// mouse-driven shortcuts like font zoom) works normally — the two are
    /// mutually exclusive at the terminal-protocol level, so this can't be
    /// "both at once". `main` reads this each loop iteration and issues
    /// the actual Enable/DisableMouseCapture sequence when it changes.
    pub mouse_capture: bool,
    /// Whether panes render with the fixed classic-DOS Norton Commander
    /// palette (blue background, cyan/white text) instead of following
    /// the terminal's own theme.
    pub classic_style: bool,
    /// Whether the left pane starts each new run in the directory `pc` was
    /// launched from, instead of restoring where it was left last session
    /// (the right pane always restores its last session directory either
    /// way). Only takes effect on the next launch, not live.
    pub start_left_in_cwd: bool,
    /// Whether each pane's bottom border shows a totals line — file/dir
    /// counts normally, or a byte-count summary of the marked files while
    /// any are tagged.
    pub pane_totals: bool,
    /// Whether F8 sends items to the desktop trash (recoverable) instead of
    /// deleting them for good. Shift+F8 always deletes permanently.
    pub use_trash: bool,
    /// Whether each pane starts with a Name/Size/Date header row (classic
    /// NC), which then also carries the sort arrow; otherwise the sort is
    /// shown on the pane's top border.
    pub column_headers: bool,
    /// Height (in text rows) of the preview pane in the last drawn frame,
    /// so scrolling can stop once the last line reaches the bottom of the
    /// visible area instead of scrolling it away entirely.
    pub preview_visible_lines: usize,
    /// Width (in columns) of the preview pane's content area in the last
    /// drawn frame, needed to compute how many rows text wraps to.
    pub preview_visible_width: usize,
    /// Height (in rows) of a file-listing pane in the last drawn frame,
    /// used to size a PgUp/PgDown page jump.
    pub pane_visible_lines: usize,
    /// Detached external viewers (e.g. an image viewer opened via
    /// `xdg-open`) that were spawned without waiting for them to exit, kept
    /// around only so their exit status can be reaped and avoid zombies.
    background_children: Vec<Child>,
    /// Persisted across frames (unlike a fresh `ListState::default()` each
    /// draw) so ratatui's own scroll-to-keep-selected-visible logic works
    /// incrementally instead of recomputing from offset zero every time —
    /// otherwise the highlighted row gets pinned to the bottom of a long
    /// listing while moving up, only settling into place near the top.
    pub left_list_state: ListState,
    pub right_list_state: ListState,
    /// Screen areas of the two panes in the last drawn frame (list or
    /// preview, whichever is showing), used to hit-test mouse events.
    pub left_pane_area: Rect,
    pub right_pane_area: Rect,
    /// Clickable regions of the last drawn F-key bar: each tile's screen
    /// area paired with the action pressing that F-key would run (`None`
    /// for F9, which opens the menu instead of running an action).
    pub fn_key_tiles: Vec<(Rect, Option<Action>)>,
    /// Clickable regions of the top menu bar's category labels (File,
    /// Options, Command), paired with each one's index into `MENU_BAR`.
    pub menu_bar_tiles: Vec<(Rect, usize)>,
    /// Clickable regions of the open dropdown's items, paired with each
    /// one's index into the current category's item list. Empty unless
    /// `menu_open`.
    pub menu_item_tiles: Vec<(Rect, usize)>,
    /// When the last scroll-wheel event was processed, to debounce
    /// duplicate events some terminals/compositors emit for a single
    /// physical wheel click (see `debounced_scroll`).
    last_scroll_at: Option<Instant>,
}

impl App {
    pub fn new() -> Result<Self> {
        let cwd = env::current_dir()?;
        let saved_settings = state::load_settings();
        let start_left_in_cwd = saved_settings
            .get(SettingItem::StartLeftInCwd.key())
            .copied()
            .unwrap_or(true);
        let pane_totals = saved_settings
            .get(SettingItem::PaneTotals.key())
            .copied()
            .unwrap_or(true);
        let (left_dir, right_dir) = match state::load() {
            Some(last) if start_left_in_cwd => (cwd.clone(), last.right),
            Some(last) => (last.left, last.right),
            None => (cwd.clone(), cwd),
        };
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
        let mouse_capture = saved_settings
            .get(SettingItem::MouseCapture.key())
            .copied()
            .unwrap_or(true);
        let classic_style = saved_settings
            .get(SettingItem::ClassicStyle.key())
            .copied()
            .unwrap_or(false);
        let use_trash = saved_settings
            .get(SettingItem::UseTrash.key())
            .copied()
            .unwrap_or(true);
        let column_headers = saved_settings
            .get(SettingItem::ColumnHeaders.key())
            .copied()
            .unwrap_or(true);
        let mut app = App {
            left: Pane::new(left_dir, hide_hidden_files)?,
            right: Pane::new(right_dir, hide_hidden_files)?,
            active: Side::Left,
            should_quit: false,
            menu_open: false,
            menu_category: 0,
            menu_item: 0,
            status_message: String::new(),
            dialog: Dialog::None,
            dialog_cancel_focused: false,
            external_request: None,
            help_open: false,
            help_page: 0,
            logs_open: false,
            error_dialog: None,
            command_line: String::new(),
            quick_search: None,
            job: None,
            job_visible: false,
            job_cancel_focused: false,
            quit_when_job_done: false,
            wait_after_shell_command,
            quick_view: false,
            preview_focus: false,
            preview_scroll: 0,
            last_preview_target: None,
            internal_preview,
            mouse_capture,
            classic_style,
            start_left_in_cwd,
            pane_totals,
            use_trash,
            column_headers,
            preview_visible_lines: 0,
            preview_visible_width: 0,
            pane_visible_lines: 0,
            background_children: Vec::new(),
            left_list_state: ListState::default(),
            right_list_state: ListState::default(),
            left_pane_area: Rect::default(),
            right_pane_area: Rect::default(),
            fn_key_tiles: Vec::new(),
            menu_bar_tiles: Vec::new(),
            menu_item_tiles: Vec::new(),
            last_scroll_at: None,
        };
        app.restore_sorts();
        Ok(app)
    }

    pub fn save_state(&self) {
        state::save(&self.left.cwd, &self.right.cwd);
        state::save_sorts(
            (self.left.sort_key, self.left.sort_reversed),
            (self.right.sort_key, self.right.sort_reversed),
        );
    }

    /// Restores each pane's sort from last session.
    fn restore_sorts(&mut self) {
        let Some((left, right)) = state::load_sorts() else {
            return;
        };
        if let Err(err) = self.left.apply_sort(left.0, left.1) {
            self.set_error(format!("Cannot reload left pane: {err}"));
        }
        if let Err(err) = self.right.apply_sort(right.0, right.1) {
            self.set_error(format!("Cannot reload right pane: {err}"));
        }
    }

    /// The menu text for `action`: the Sort item matching the active
    /// pane's current sort gets its direction arrow appended.
    pub fn menu_label(&self, action: Action) -> String {
        let key = match action {
            Action::SortByName => SortKey::Name,
            Action::SortByExtension => SortKey::Extension,
            Action::SortBySize => SortKey::Size,
            Action::SortByDate => SortKey::Modified,
            _ => return action.label().to_string(),
        };
        let pane = self.active_pane_ref();
        if pane.sort_key == key {
            let arrow = if pane.sort_descending() {
                '\u{2193}'
            } else {
                '\u{2191}'
            };
            format!("{} {arrow}", action.label())
        } else {
            action.label().to_string()
        }
    }

    /// Records an unexpected failure (I/O errors etc.) to the log — for
    /// routine confirmations ("Deleted x.txt"), use `set_status` instead.
    pub fn set_error(&mut self, message: String) {
        logging::log_error(&message);
        self.status_message = message.clone();
        self.error_dialog = Some(message);
    }

    /// Records a routine status confirmation (e.g. "Deleted x.txt") to the
    /// log — there's no dedicated on-screen status line any more; Command >
    /// Show Logs is where these are actually read.
    fn set_status(&mut self, message: String) {
        logging::log_info(&message);
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

    /// Unlike `active_pane`, gets a specific side regardless of keyboard
    /// focus — for mouse interaction, which acts on whatever's under the
    /// pointer rather than whichever pane Tab last selected.
    pub fn pane_mut(&mut self, side: Side) -> &mut Pane {
        match side {
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

    pub fn inactive_pane_ref(&self) -> &Pane {
        match self.active {
            Side::Left => &self.right,
            Side::Right => &self.left,
        }
    }

    /// Whether `action` would actually do something given the current
    /// selection, used to gray out inapplicable items in the pulldown menu.
    pub fn action_enabled(&self, action: Action) -> bool {
        let entry = self.active_pane_ref().selected_entry();
        let has_real_selection = entry.is_some_and(|e| e.name != "..");
        let selection_is_file = entry.is_some_and(|e| e.name != ".." && !e.is_dir);

        match action {
            // Also enabled for executables: Open runs `open_selected`,
            // which already runs them directly (same as pressing Enter).
            Action::Open => entry.is_some_and(|e| e.is_dir || e.is_executable),
            Action::Rename
            | Action::Copy
            | Action::Move
            | Action::Delete
            | Action::DeletePermanently
            | Action::SelectFile => has_real_selection,
            // View also works on directories (and "..") — quick-view shows
            // a name-only listing for those.
            Action::View => entry.is_some(),
            Action::Edit => selection_is_file,
            // One item is plain F2 Rename; bulk needs at least two marked.
            Action::RenameSelected => self.active_pane_ref().marked.len() >= 2,
            Action::ShowProgress => self.job.is_some(),
            Action::Separator => false,
            Action::MkDir
            | Action::NewFile
            | Action::SelectFiles
            | Action::UnselectFiles
            | Action::InvertSelection
            | Action::QuickSearch
            | Action::SortByName
            | Action::SortByExtension
            | Action::SortBySize
            | Action::SortByDate
            | Action::Help
            | Action::Settings
            | Action::ShowTerminal
            | Action::ShowLogs
            | Action::Quit => true,
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

    /// Some terminals/compositors (seen especially with Wayland's
    /// high-resolution wheel reporting) occasionally emit two scroll
    /// events for what was physically a single wheel click, arriving only
    /// a few milliseconds apart — nothing a human scrolls that fast.
    /// Returns true (and the caller should ignore the event) if it looks
    /// like one of those duplicates rather than a genuine new click.
    pub fn debounced_scroll(&mut self) -> bool {
        let now = Instant::now();
        let is_duplicate = self
            .last_scroll_at
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(20));
        self.last_scroll_at = Some(now);
        is_duplicate
    }

    /// Rows the current preview actually renders to once wrapped to its
    /// pane's width — the right unit for clamping scroll, since a long
    /// logical line can span several visual rows.
    fn preview_wrapped_line_count(&self) -> usize {
        preview::build_preview(self.active_pane_ref())
            .wrapped_line_count(self.preview_visible_width as u16)
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(1);
    }

    /// Stops at the point where the last line sits at the bottom of the
    /// preview pane, rather than letting it scroll away past the top.
    pub fn scroll_preview_down(&mut self) {
        let max = self
            .preview_wrapped_line_count()
            .saturating_sub(self.preview_visible_lines);
        self.preview_scroll = (self.preview_scroll + 1).min(max);
    }

    pub fn scroll_preview_to_top(&mut self) {
        self.preview_scroll = 0;
    }

    /// End: same bottom-clamping as `scroll_preview_down` — lands with the
    /// last line at the bottom of the preview, not scrolled past it.
    pub fn scroll_preview_to_bottom(&mut self) {
        self.preview_scroll = self
            .preview_wrapped_line_count()
            .saturating_sub(self.preview_visible_lines);
    }

    pub fn scroll_preview_page_up(&mut self) {
        let page = self.preview_visible_lines.max(1);
        self.preview_scroll = self.preview_scroll.saturating_sub(page);
    }

    pub fn scroll_preview_page_down(&mut self) {
        let page = self.preview_visible_lines.max(1);
        let max = self
            .preview_wrapped_line_count()
            .saturating_sub(self.preview_visible_lines);
        self.preview_scroll = (self.preview_scroll + page).min(max);
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
        // A copying Trash can be minutes into one big item, and killing it
        // would leave a half copy in the trash: let it finish that item
        // with its window showing what's happening, then quit (`poll_job`).
        if let Some(job) = &self.job
            && job.kind == DialogKind::TrashByCopy
        {
            job.cancel();
            self.quit_when_job_done = true;
            self.job_visible = true;
            return;
        }
        self.should_quit = true;
        self.cancel_job_and_wait();
    }

    /// On the way out: stop a running Copy/Move and wait for its worker, so
    /// it gets to remove the half-copied file instead of being killed
    /// mid-write.
    pub fn cancel_job_and_wait(&mut self) {
        if let Some(job) = self.job.take() {
            let verb = job.kind.verb();
            job.cancel();
            let outcome = job.join();
            logging::log_info(&format!(
                "{verb} cancelled on quit after {} item(s)",
                outcome.done
            ));
        }
    }

    /// Called every loop iteration: once the worker has finished, reports
    /// the outcome (an error popup only for real failures) and refreshes
    /// both panes.
    pub fn poll_job(&mut self) -> Result<()> {
        if !self.job.as_ref().is_some_and(Job::is_finished) {
            return Ok(());
        }
        let Some(job) = self.job.take() else {
            return Ok(());
        };
        self.job_visible = false;
        let kind = job.kind;
        let items = job.items.clone();
        let src_dir = job.src_dir.clone();
        let dest_dir = job.dest_dir.clone();
        let outcome = job.join();
        let verb_done = match kind {
            DialogKind::Move => "Moved",
            DialogKind::TrashByCopy => "Moved to trash",
            _ => "Copied",
        };

        if outcome.cancelled {
            self.set_status(format!(
                "{} cancelled after {} of {} item(s)",
                kind.verb(),
                outcome.done,
                items.len()
            ));
        } else if outcome.errors.is_empty() {
            let message = match items.as_slice() {
                [(name, _)] if kind == DialogKind::TrashByCopy => format!("{verb_done} {name}"),
                _ if kind == DialogKind::TrashByCopy => {
                    format!(
                        "{verb_done} {} items from {}",
                        outcome.done,
                        src_dir.display()
                    )
                }
                [(name, src)] => {
                    format!(
                        "{verb_done} {} to {}",
                        src.display(),
                        dest_dir.join(name).display()
                    )
                }
                _ => format!(
                    "{verb_done} {} items from {} to {}",
                    outcome.done,
                    src_dir.display(),
                    dest_dir.display()
                ),
            };
            self.set_status(message);
        } else {
            let hint = if kind == DialogKind::TrashByCopy {
                " (Shift+F8 deletes permanently instead)"
            } else {
                ""
            };
            self.set_error(format!(
                "{} failed for {} of {} item(s): {}{hint}",
                kind.verb(),
                outcome.errors.len(),
                items.len(),
                outcome.errors.join("; ")
            ));
        }
        self.left.reload()?;
        self.right.reload()?;
        if self.quit_when_job_done {
            self.should_quit = true;
        }
        Ok(())
    }

    /// Esc, or Enter on [ Background ]: hide the progress window and let
    /// the job carry on while the panels are usable again.
    pub fn hide_job(&mut self) {
        // Waiting to quit: keep showing why nothing is happening yet.
        if !self.quit_when_job_done {
            self.job_visible = false;
        }
    }

    pub fn toggle_job_focus(&mut self) {
        self.job_cancel_focused = !self.job_cancel_focused;
    }

    /// Enter in the progress window: whichever button has focus.
    pub fn activate_job_button(&mut self) {
        if !self.job_cancel_focused {
            self.hide_job();
        } else if let Some(job) = &self.job {
            job.cancel();
        }
    }

    fn sort_active_pane(&mut self, key: SortKey) {
        if let Err(err) = self.active_pane().set_sort(key) {
            self.set_error(format!("Cannot reload pane: {err}"));
        }
    }

    /// Quick search: typing a character that no entry matches is ignored
    /// (as in Midnight Commander), so the query always points somewhere.
    pub fn quick_search_push(&mut self, c: char) {
        let Some(query) = &self.quick_search else {
            return;
        };
        let candidate = format!("{query}{c}");
        let start = self.active_pane_ref().selected;
        if let Some(idx) = self.active_pane_ref().find_match(&candidate, start, true) {
            self.active_pane().selected = idx;
            self.quick_search = Some(candidate);
        }
    }

    pub fn quick_search_backspace(&mut self) {
        if let Some(query) = &mut self.quick_search {
            query.pop();
        }
    }

    /// Up/Down during quick search: the previous/next entry that matches.
    pub fn quick_search_step(&mut self, forward: bool) {
        let Some(query) = self.quick_search.clone() else {
            return;
        };
        let pane = self.active_pane_ref();
        let len = pane.entries.len().max(1);
        let start = if forward {
            (pane.selected + 1) % len
        } else {
            (pane.selected + len - 1) % len
        };
        if let Some(idx) = pane.find_match(&query, start, forward) {
            self.active_pane().selected = idx;
        }
    }

    pub fn end_quick_search(&mut self) {
        self.quick_search = None;
    }

    /// File > Rename Selected: the marked items (two or more — one is just F2)
    /// go one name per line into a temp file that `main` opens in $EDITOR.
    fn request_bulk_rename(&mut self) {
        let pane = self.active_pane_ref();
        let names: Vec<String> = pane
            .marked_items()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        if names.len() < 2 {
            return;
        }
        if names
            .iter()
            .any(|name| name.contains('\n') || name.contains('\r'))
        {
            self.set_error(
                "Rename selected cannot handle names containing line breaks".to_string(),
            );
            return;
        }
        let dir = pane.cwd.clone();
        let list_file = env::temp_dir().join(format!("pc-bulk-rename-{}.txt", std::process::id()));
        if let Err(err) = std::fs::write(&list_file, bulk_rename::list_text(&names)) {
            self.set_error(format!(
                "Rename selected: cannot write {}: {err}",
                list_file.display()
            ));
            return;
        }
        self.external_request = Some(ExternalRequest::RenameSelected {
            list_file,
            dir,
            names,
        });
    }

    /// After the editor closed: validate the edit and, if anything
    /// changed, ask before renaming. A failed editor (e.g. vim's `:cq`)
    /// aborts without renaming anything.
    pub fn finish_bulk_rename(
        &mut self,
        list_file: &Path,
        dir: PathBuf,
        names: &[String],
        editor_ok: bool,
    ) {
        let edited = std::fs::read_to_string(list_file);
        let _ = std::fs::remove_file(list_file);
        if !editor_ok {
            self.set_status("Rename selected aborted".to_string());
            return;
        }
        let edited = match edited {
            Ok(text) => text,
            Err(err) => {
                self.set_error(format!(
                    "Rename selected: cannot read the edited list: {err}"
                ));
                return;
            }
        };
        match bulk_rename::plan(&dir, names, &edited) {
            Err(message) => {
                self.set_error(format!("Rename selected: {message} — nothing was renamed"))
            }
            Ok(plan) if plan.renames.is_empty() => {
                self.set_status("Rename selected: no names changed".to_string());
            }
            Ok(plan) => {
                self.dialog_cancel_focused = !plan.overwrites.is_empty();
                self.dialog = Dialog::ConfirmRenameSelected { dir, plan };
            }
        }
    }

    pub fn open_menu(&mut self) {
        self.open_menu_at(0);
    }

    /// Opens the menu directly on `category` — used when clicking a menu
    /// bar label, which should jump straight to that category rather than
    /// always starting from File like `open_menu` does.
    pub fn open_menu_at(&mut self, category: usize) {
        self.menu_open = true;
        self.menu_category = category.min(MENU_BAR.len() - 1);
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
            Action::Delete => {
                let kind = if !self.use_trash {
                    DialogKind::Delete
                } else if fs_ops::trash_needs_copy(&self.active_pane_ref().cwd) {
                    DialogKind::TrashByCopy
                } else {
                    DialogKind::Trash
                };
                self.request_delete(kind);
            }
            Action::DeletePermanently => self.request_delete(DialogKind::Delete),
            Action::MkDir => self.request_mkdir(),
            Action::NewFile => self.request_new_file(),
            Action::View => self.view_selected(),
            Action::Edit => self.request_external(ExternalRequest::Edit, "edit"),
            Action::Help => {
                self.help_open = true;
                self.help_page = 0;
            }
            Action::ShowTerminal => self.request_reveal_terminal(),
            Action::ShowLogs => self.logs_open = true,
            Action::QuickSearch => {
                self.quick_search = Some(String::new());
            }
            Action::SortByName => self.sort_active_pane(SortKey::Name),
            Action::SortByExtension => self.sort_active_pane(SortKey::Extension),
            Action::SortBySize => self.sort_active_pane(SortKey::Size),
            Action::SortByDate => self.sort_active_pane(SortKey::Modified),
            Action::ShowProgress => {
                if self.job.is_some() {
                    self.job_visible = true;
                    self.job_cancel_focused = false;
                }
            }
            Action::RenameSelected => self.request_bulk_rename(),
            Action::Separator => {}
            Action::SelectFile => self.active_pane().toggle_mark_selected(),
            Action::SelectFiles => self.request_select_files(TextInputKind::SelectFiles),
            Action::UnselectFiles => self.request_select_files(TextInputKind::UnselectFiles),
            Action::InvertSelection => {
                let marked = self.active_pane().invert_marks();
                self.set_status(format!("{marked} file(s) selected"));
            }
            Action::Quit => {
                self.dialog = Dialog::ConfirmQuit;
                self.dialog_cancel_focused = false;
            }
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
            SettingItem::MouseCapture => self.mouse_capture,
            SettingItem::ClassicStyle => self.classic_style,
            SettingItem::StartLeftInCwd => self.start_left_in_cwd,
            SettingItem::PaneTotals => self.pane_totals,
            SettingItem::UseTrash => self.use_trash,
            SettingItem::ColumnHeaders => self.column_headers,
        }
    }

    fn set_setting_value(&mut self, item: SettingItem, value: bool) {
        match item {
            SettingItem::WaitAfterShellCommand => self.wait_after_shell_command = value,
            SettingItem::MouseCapture => self.mouse_capture = value,
            SettingItem::ClassicStyle => self.classic_style = value,
            SettingItem::StartLeftInCwd => self.start_left_in_cwd = value,
            SettingItem::PaneTotals => self.pane_totals = value,
            SettingItem::UseTrash => self.use_trash = value,
            SettingItem::ColumnHeaders => self.column_headers = value,
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

    /// Toggles the selected switch in memory only — not persisted until
    /// `settings_save`, so `settings_cancel` can still revert it.
    pub fn settings_toggle_selected(&mut self) {
        if let Dialog::Settings { selected, .. } = &self.dialog {
            let item = SettingItem::ALL[*selected];
            let value = self.setting_value(item);
            self.set_setting_value(item, !value);
        }
    }

    /// Left/Right arrows: snap the selected switch to its left or right
    /// side directly (whichever value that side stands for, see
    /// `SettingItem::sides`), rather than toggling — pressing the arrow for
    /// the side that's already active is a no-op.
    pub fn settings_select_left(&mut self) {
        self.settings_select_side(0);
    }

    pub fn settings_select_right(&mut self) {
        self.settings_select_side(1);
    }

    fn settings_select_side(&mut self, side: usize) {
        if let Dialog::Settings { selected, .. } = &self.dialog {
            let item = SettingItem::ALL[*selected];
            self.set_setting_value(item, item.sides()[side].1);
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

    /// Enter: opens the selected directory; for a file with the executable
    /// permission bit set, runs it exactly as if its name had been typed on
    /// the command line (`./name`, or just `name` on Windows, which has no
    /// such convention); for a known media file (image, PDF, audio, video,
    /// HTML), hands it to the desktop's default viewer (`xdg-open`, `open`
    /// on macOS, or Explorer on Windows). Anything else does nothing, same
    /// as before.
    pub fn open_selected(&mut self) -> Result<()> {
        let pane = self.active_pane_ref();
        if let Some(entry) = pane.selected_entry()
            && entry.is_executable
        {
            let command = if cfg!(windows) {
                format!("\"{}\"", entry.name)
            } else {
                format!("./{}", shell_words::quote(&entry.name))
            };
            self.external_request = Some(ExternalRequest::Shell {
                command,
                cwd: pane.cwd.clone(),
            });
            return Ok(());
        }

        if let Some(entry) = pane.selected_entry()
            && !entry.is_dir
            && is_media_file(&entry.name)
            && let Some(path) = pane.selected_path()
        {
            self.open_externally(&path);
            return Ok(());
        }

        if let Some(message) = self.active_pane().enter_selected()? {
            self.set_error(message);
        }
        Ok(())
    }

    /// Spawns the desktop's default viewer on `path` without waiting for it
    /// — a GUI viewer can stay open indefinitely, and PC shouldn't block
    /// (or need its terminal suspended) while the user looks at it. Its
    /// stdio is discarded so any stray output from it can't corrupt our
    /// screen, since we're not suspending the alternate screen for this.
    fn open_externally(&mut self, path: &Path) {
        // `xdg-open`/`open` are real executables we can spawn directly;
        // Windows has no such standalone program (`start` is a cmd.exe
        // builtin, fiddly to invoke correctly), so Explorer is used
        // instead — handed a file path, it opens it with the associated
        // default app, the same as double-clicking it.
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(windows) {
            "explorer"
        } else {
            "xdg-open"
        };
        match Command::new(opener)
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => self.background_children.push(child),
            Err(err) => self.set_error(format!("Failed to launch {opener}: {err}")),
        }
    }

    /// Reaps any detached external viewers that have exited, so they don't
    /// pile up as zombie processes. Cheap to call every loop iteration —
    /// `try_wait` never blocks.
    pub fn reap_finished_children(&mut self) {
        self.background_children
            .retain_mut(|child| !matches!(child.try_wait(), Ok(Some(_))));
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
        self.dialog_cancel_focused = false;
    }

    fn request_copy(&mut self) {
        self.request_transfer(DialogKind::Copy);
    }

    fn request_move(&mut self) {
        self.request_transfer(DialogKind::Move);
    }

    fn request_transfer(&mut self, kind: DialogKind) {
        if self.job.is_some() {
            self.set_error("Another copy/move is still running (Ctrl+B shows it)".to_string());
            return;
        }
        let pane = self.active_pane();
        let items = pane.marked_items();
        let items = if items.is_empty() {
            let Some(src) = pane.selected_path() else {
                return;
            };
            let Some(entry) = pane.selected_entry() else {
                return;
            };
            vec![(entry.name.clone(), src)]
        } else {
            items
        };
        self.dialog = Dialog::Confirm { kind, items };
        self.dialog_cancel_focused = !kind.default_yes();
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
            self.set_status(format!("Cannot {verb} a directory"));
            return;
        }
        if let Some(path) = pane.selected_path() {
            self.external_request = Some(make(path));
        }
    }

    /// F8 (`Trash` or `Delete`, per the setting) and Shift+F8 (always
    /// `Delete`).
    fn request_delete(&mut self, kind: DialogKind) {
        let pane = self.active_pane();
        let items = pane.marked_items();
        let items = if items.is_empty() {
            let Some(path) = pane.selected_path() else {
                return;
            };
            let Some(entry) = pane.selected_entry() else {
                return;
            };
            vec![(entry.name.clone(), path)]
        } else {
            items
        };
        self.dialog = Dialog::Confirm { kind, items };
        self.dialog_cancel_focused = !kind.default_yes();
    }

    pub fn confirm_dialog(&mut self) -> Result<()> {
        match &self.dialog {
            Dialog::Confirm { kind, items } => {
                let kind = *kind;
                let items = items.clone();
                if matches!(kind, DialogKind::Copy | DialogKind::Move) {
                    let dest_dir = self.inactive_pane_ref().cwd.clone();
                    let conflicts = existing_destinations(&items, &dest_dir);
                    if !conflicts.is_empty() {
                        self.dialog = Dialog::ConfirmOverwrite {
                            kind,
                            items,
                            conflicts,
                        };
                        // Replacing files is destructive, so like Delete
                        // this defaults to "no".
                        self.dialog_cancel_focused = true;
                        return Ok(());
                    }
                }
                self.run_file_operation(kind, items)?;
            }
            Dialog::ConfirmOverwrite { kind, items, .. } => {
                let kind = *kind;
                let items = items.clone();
                self.run_file_operation(kind, items)?;
            }
            Dialog::Rename { input, src } => {
                let new_name = input.clone();
                let src = src.clone();
                if new_name.trim().is_empty() {
                    self.set_status("Name cannot be empty".to_string());
                    self.dialog = Dialog::None;
                    return Ok(());
                }
                match src.parent() {
                    Some(parent) => {
                        let dest = parent.join(&new_name);
                        if fs_ops::rename_target_taken(&src, &dest) {
                            self.set_error(format!("Rename failed: \"{new_name}\" already exists"));
                        } else {
                            match fs_ops::move_path(&src, &dest) {
                                Ok(()) => self.set_status(format!("Renamed to {new_name}")),
                                Err(err) => self.set_error(format!("Rename failed: {err}")),
                            }
                        }
                    }
                    None => self.set_error("Cannot rename: no parent directory".to_string()),
                }

                self.dialog = Dialog::None;
                self.left.reload()?;
                self.right.reload()?;
            }
            Dialog::TextInput { kind, input }
                if matches!(
                    kind,
                    TextInputKind::SelectFiles | TextInputKind::UnselectFiles
                ) =>
            {
                let select = *kind == TextInputKind::SelectFiles;
                let patterns = input.clone();
                let changed = self.active_pane().mark_matching(&patterns, select);
                let verb = if select { "Selected" } else { "Unselected" };
                self.set_status(format!("{verb} {changed} file(s) matching {patterns}"));
                self.dialog = Dialog::None;
            }
            Dialog::TextInput { kind, input } => {
                let kind = *kind;
                let name = input.clone();
                if name.trim().is_empty() {
                    self.set_status("Name cannot be empty".to_string());
                    self.dialog = Dialog::None;
                    return Ok(());
                }
                let target = self.active_pane().cwd.join(&name);

                match kind {
                    TextInputKind::MkDir => match std::fs::create_dir(&target) {
                        Ok(()) => self.set_status(format!("Created directory {name}")),
                        Err(err) => self.set_error(format!("MkDir failed: {err}")),
                    },
                    // create_new, not create: File::create truncates an
                    // existing file of that name to zero bytes.
                    TextInputKind::NewFile => match std::fs::File::create_new(&target) {
                        Ok(_) => self.set_status(format!("Created file {name}")),
                        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                            self.set_error(format!("New file failed: \"{name}\" already exists"))
                        }
                        Err(err) => self.set_error(format!("New file failed: {err}")),
                    },
                    TextInputKind::SelectFiles | TextInputKind::UnselectFiles => {}
                }

                self.dialog = Dialog::None;
                self.left.reload()?;
                self.right.reload()?;
            }
            Dialog::ConfirmRenameSelected { dir, plan } => {
                let (done, errors) = bulk_rename::execute(dir, plan);
                if errors.is_empty() {
                    self.set_status(format!("Renamed {done} item(s) in {}", dir.display()));
                } else {
                    self.set_error(format!(
                        "Rename selected: {done} renamed, {} failed: {}",
                        errors.len(),
                        errors.join("; ")
                    ));
                }
                self.active_pane().clear_marks();
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

    /// Runs a confirmed Copy/Move/Delete/Trash; shared by the plain confirm
    /// and the overwrite prompt. Copy/Move start a background job with its
    /// progress window on top; Delete/Trash run right here.
    fn run_file_operation(
        &mut self,
        kind: DialogKind,
        items: Vec<(String, PathBuf)>,
    ) -> Result<()> {
        let src_dir = self.active_pane_ref().cwd.clone();
        let wants_job = matches!(
            kind,
            DialogKind::Copy | DialogKind::Move | DialogKind::TrashByCopy
        );
        self.dialog = Dialog::None;
        if wants_job && self.job.is_some() {
            self.set_error("Another operation is still running (Ctrl+B shows it)".to_string());
            return Ok(());
        }
        // A job already holds its own list, so the marks can go now.
        self.active_pane().clear_marks();

        // Copy/Move (and a Trash that has to copy) run on a worker thread
        // with a progress window on top.
        if wants_job {
            let dest_dir = if kind == DialogKind::TrashByCopy {
                fs_ops::home_trash_dir().unwrap_or_else(|| src_dir.clone())
            } else {
                self.inactive_pane_ref().cwd.clone()
            };
            self.job = Some(Job::start(kind, items, src_dir, dest_dir));
            self.job_visible = true;
            self.job_cancel_focused = false;
            return Ok(());
        }

        let trashing = matches!(kind, DialogKind::Trash | DialogKind::TrashByCopy);
        let mut done = 0usize;
        let mut errors: Vec<String> = Vec::new();
        for (name, src) in &items {
            let result = if trashing {
                trash::delete(src).map_err(anyhow::Error::from)
            } else {
                fs_ops::delete_recursive(src)
            };
            match result {
                Ok(()) => done += 1,
                Err(err) => errors.push(format!("{name}: {err}")),
            }
        }

        if errors.is_empty() {
            let verb_done = if trashing {
                "Moved to trash"
            } else {
                "Deleted"
            };
            let message = match items.as_slice() {
                [(name, _)] => format!("{verb_done} {name}"),
                _ => format!("{verb_done} {done} items from {}", src_dir.display()),
            };
            self.set_status(message);
        } else {
            // Never fall back to a permanent delete on its own — point at
            // the explicit way to do that instead.
            let hint = if trashing {
                " (Shift+F8 deletes permanently instead)"
            } else {
                ""
            };
            self.set_error(format!(
                "{} failed for {} of {} item(s): {}{hint}",
                kind.verb(),
                errors.len(),
                items.len(),
                errors.join("; ")
            ));
        }

        self.left.reload()?;
        self.right.reload()?;
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
        self.dialog_cancel_focused = false;
    }

    /// File > Select/Unselect Files: asks for wildcard patterns, starting
    /// from `*` (everything), as in Norton Commander.
    fn request_select_files(&mut self, kind: TextInputKind) {
        self.dialog = Dialog::TextInput {
            kind,
            input: "*".to_string(),
        };
        self.dialog_cancel_focused = false;
    }

    pub fn request_new_file(&mut self) {
        self.dialog = Dialog::TextInput {
            kind: TextInputKind::NewFile,
            input: String::new(),
        };
        self.dialog_cancel_focused = false;
    }

    /// Left/Right in a dialog: swap which button — the primary action or
    /// Cancel — Enter would activate. There are only ever two, so this is a
    /// plain flip rather than tracking an index.
    pub fn toggle_dialog_focus(&mut self) {
        self.dialog_cancel_focused = !self.dialog_cancel_focused;
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

/// Extensions handed off to the desktop's default viewer on Enter, rather
/// than doing nothing. Deliberately just images/PDF/audio/video/HTML —
/// anything else (including plain text) stays as-is rather than risking
/// guessing wrong about what the user wants to happen.
const MEDIA_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "tiff", "tif", "ico", "pdf", "mp4", "mov",
    "mkv", "avi", "webm", "mp3", "wav", "flac", "ogg", "m4a", "html", "htm",
];

fn is_media_file(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
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

/// Names of the items whose Copy/Move target in `dest_dir` already exists.
/// An item that *is* its own target (both panes in the same directory) is
/// left out: that isn't an overwrite, and `fs_ops` refuses it with its own
/// message.
fn existing_destinations(items: &[(String, PathBuf)], dest_dir: &Path) -> Vec<String> {
    items
        .iter()
        .filter(|(name, src)| {
            let dest = dest_dir.join(name);
            std::fs::symlink_metadata(&dest).is_ok() && !fs_ops::is_same_path(src, &dest)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_setting_switch_has_one_side_per_value() {
        for item in SettingItem::ALL {
            let [(_, left), (_, right)] = item.sides();
            assert_ne!(
                left, right,
                "{item:?} needs one side for true and one for false"
            );
        }
    }

    #[test]
    fn lists_only_items_whose_destination_exists() {
        let dir = std::env::temp_dir().join("pc_test_existing_destinations");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        for name in ["new.txt", "both.txt"] {
            std::fs::write(dir.join("src").join(name), "src").unwrap();
        }
        std::fs::write(dir.join("dest/both.txt"), "dest").unwrap();
        let items: Vec<(String, PathBuf)> = ["new.txt", "both.txt"]
            .iter()
            .map(|name| (name.to_string(), dir.join("src").join(name)))
            .collect();

        assert_eq!(
            existing_destinations(&items, &dir.join("dest")),
            vec!["both.txt"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copying_onto_itself_is_not_an_overwrite_conflict() {
        let dir = std::env::temp_dir().join("pc_test_existing_destinations_same");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let items = vec![("a.txt".to_string(), dir.join("a.txt"))];

        assert!(existing_destinations(&items, &dir).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
