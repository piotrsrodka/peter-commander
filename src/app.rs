use std::env;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::bulk_rename::{self, Plan};
use crate::clipboard::Clipboard;
use crate::editor::{self, Command as EditorCommand, Editor};
use crate::fs_ops;
use crate::image_preview::ImagePreview;
use crate::job::Job;
use crate::logging;
use crate::menu::{self, Action, MenuCategory};
use crate::pane::{Pane, SortKey};
use crate::preview;
use crate::state;
use crate::transfer::{self, Overwrite, TextField, Transfer};

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
    /// The editor's File > Save As (and a save whose file has gone).
    SaveAs,
}

impl TextInputKind {
    pub fn title(&self) -> &'static str {
        match self {
            TextInputKind::MkDir => "New directory",
            TextInputKind::NewFile => "New file",
            TextInputKind::SelectFiles => "Select",
            TextInputKind::UnselectFiles => "Unselect",
            TextInputKind::SaveAs => "Save as",
        }
    }

    pub fn prompt(&self) -> &'static str {
        match self {
            TextInputKind::MkDir => "New directory name:",
            TextInputKind::NewFile => "New file name:",
            TextInputKind::SelectFiles => "Select files matching (e.g. *.jpg *.png):",
            TextInputKind::UnselectFiles => "Unselect files matching (e.g. *.jpg *.png):",
            TextInputKind::SaveAs => "Save as (a name is next to the file):",
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
    /// Copy/Move, with an editable target. Turns into the overwrite prompt
    /// when that target has items in the way, so nothing is replaced without
    /// being asked.
    Transfer(Transfer),
    TextInput {
        kind: TextInputKind,
        field: TextField,
        /// The focus is on the field rather than the buttons; see
        /// `Transfer::editing`.
        editing: bool,
    },
    Rename {
        field: TextField,
        editing: bool,
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
    /// Save As (or a first save) onto a file that's already there: replace
    /// it? No is the default, as for every overwrite.
    ConfirmSaveAs {
        target: PathBuf,
    },
    /// Closing the built-in editor (or quitting) with unsaved changes:
    /// Save, Don't Save or Cancel, as in Notepad — never silently lost.
    UnsavedChanges {
        /// Quit once the editor is closed, if this was Quit.
        quit_after: bool,
        /// The focused button: 0 Save, 1 Don't Save, 2 Cancel.
        focused: usize,
    },
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
    BuiltinEditor,
    EditorFocusMode,
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
        SettingItem::BuiltinEditor,
        SettingItem::EditorFocusMode,
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
            SettingItem::BuiltinEditor => {
                [("Built-in editor (F4)", true), ("External editor", false)]
            }
            SettingItem::EditorFocusMode => {
                [("Regular editor view", false), ("Focus mode editor", true)]
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
            SettingItem::BuiltinEditor => "builtin_editor",
            SettingItem::EditorFocusMode => "editor_focus_mode",
            SettingItem::MouseCapture => "mouse_capture",
            SettingItem::ClassicStyle => "classic_style",
            SettingItem::StartLeftInCwd => "start_left_in_cwd",
            SettingItem::PaneTotals => "pane_totals",
            SettingItem::UseTrash => "use_trash",
            SettingItem::ColumnHeaders => "column_headers",
        }
    }
}

/// Wrap Lines, for the editor and the quick view alike: kept with the
/// settings but not in the Settings dialog, since it's switched from the
/// menus (Options, and the editor's Edit).
const WRAP_LINES_KEY: &str = "wrap_lines";

/// What the unsaved-changes prompt was opened for, done once it's answered
/// — or, when Save there has to ask for a name first (Save As), once that's
/// saved.
enum AfterPrompt {
    /// Esc: close the editor.
    Close,
    /// Quit the program.
    Quit,
    /// F4 on another file: that one, already opened (so a file the editor
    /// can't open never costs the changes asked about).
    Edit(Box<Editor>),
    /// F3: close the editor and show the quick view.
    QuickView,
    /// Command > Open Editor: an untitled one in its place.
    Untitled,
}

/// What's been seen of the edited file on disk, beyond what the editor
/// itself knows; it goes with the file (see `App::install_editor`).
#[derive(Debug, Default)]
struct EditedFileWatch {
    /// Where it went, if it was renamed or moved in the app (from the
    /// list): Save then suggests that name.
    moved_to: Option<PathBuf>,
    /// The note about it gone (or replaced) has been shown; not again.
    noted: bool,
    /// Seen missing once: looked at again before it counts as gone, as a
    /// program saving it by deleting and writing anew leaves a moment
    /// without it.
    missing_once: bool,
}

/// The F1 page about the built-in editor, which F1 opens straight onto from
/// the editor: an index into `ui::HELP_PAGES` (which checks that it is).
pub const EDITOR_HELP_PAGE: usize = 2;

/// Ctrl+F in F3's quick view of a text file: the editor's own search, run on
/// an editor holding the text the preview shows (never shown itself, never
/// saved) — its find bar, matches and selection are what the preview
/// highlights.
pub struct PreviewFind {
    /// The previewed file this is for; another one starts afresh.
    pub target: PathBuf,
    pub text: Editor,
    /// The preview only loads the start of a big file, and so only that is
    /// searched.
    pub truncated: bool,
    /// The text it searches, as the preview had it: a file that changes
    /// under the quick view gets a fresh find (see `refresh_from_disk`).
    source: std::rc::Rc<Vec<String>>,
}

/// What the other pane showed before F4 put the editor there, brought back
/// when it closes: typically F3's quick view of the same file, scrolled
/// where it was.
#[derive(Debug, Clone, Copy)]
struct ViewBeforeEditor {
    quick_view: bool,
    preview_focus: bool,
    preview_scroll: usize,
    preview_scroll_x: usize,
    /// Wrap Lines then: switched in the editor meanwhile, the scroll is
    /// brought over to the new rows.
    wrap_lines: bool,
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
    /// Show an image full-screen on the Linux console, where `xdg-open`
    /// has no desktop to open it in.
    #[cfg(target_os = "linux")]
    ShowImage(PathBuf),
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
    /// Which F1 help page is shown (an index into `ui::HELP_PAGES`, below
    /// `ui::HELP_PAGE_COUNT`) — Left/Right flip between them while help is
    /// open.
    pub help_page: usize,
    /// Command > Show Logs: a full-screen view of the recent contents of
    /// the error/status log file, closed by any key like help.
    pub logs_open: bool,
    /// Command > About: program name, version and author, closed by any
    /// key like help.
    pub about_open: bool,
    /// A blocking "in your face" popup for the most recent error (e.g.
    /// "Access is denied" entering a locked directory) — unlike
    /// `status_message`, which only reaches the log file, this demands a
    /// keypress to dismiss so it can't be missed. Not shown for routine
    /// confirmations (`set_status`), only real failures (`set_error`).
    pub error_dialog: Option<String>,
    /// The popup in `error_dialog` is a plain note rather than a failure
    /// (`set_notice`): drawn like any other dialog, not in alarm red.
    pub error_dialog_is_notice: bool,
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
    /// How far the quick view is scrolled sideways, in cells — only while
    /// Wrap Lines is off.
    pub preview_scroll_x: usize,
    /// The path the preview was last built for, so a change in selection
    /// (arrow keys, entering a directory, cd, ...) can reset the scroll
    /// position for the newly previewed entry.
    last_preview_target: Option<PathBuf>,
    /// Pictures for the quick-view pane.
    pub image_preview: ImagePreview,
    /// Whether F3 (View) shows the internal quick preview or shells out to
    /// $PAGER/less, per the "F3 View uses the internal quick preview"
    /// setting.
    pub internal_preview: bool,
    /// Whether F4 edits in the built-in editor rather than $EDITOR, per the
    /// "Built-in editor" setting.
    pub builtin_editor: bool,
    /// The "Focus mode editor" setting: the full-screen editor hides the
    /// menu bar until F9 or Alt+letter calls it up.
    pub editor_focus_mode: bool,
    /// Edit > Wrap Lines, for every file the built-in editor opens.
    pub wrap_lines: bool,
    /// The file open in the built-in editor, if any. While it's open it has
    /// the keyboard (after any dialog or the menu on top of it).
    pub editor: Option<Editor>,
    /// Tab in the editor (in its pane): the keyboard is the file list's,
    /// as if no editor were open, until Tab gives it back. The two never
    /// share a key.
    pub list_focus: bool,
    /// What F3 or F4 on the list beside the editor asked for, to do once
    /// the unsaved-changes prompt is answered.
    after_prompt: Option<AfterPrompt>,
    /// F2 from the editor itself: the rename is of the edited file, which
    /// the editor then follows (renames from the list it doesn't).
    renaming_edited_file: bool,
    /// What's been seen of the edited file on disk (see `check_edited_file`).
    watch: EditedFileWatch,
    /// Find in the quick view, for the file it shows.
    pub preview_find: Option<PreviewFind>,
    /// What the quick view's find looked for last, kept from one previewed
    /// file to the next for F3.
    preview_last_search: String,
    view_before_editor: Option<ViewBeforeEditor>,
    pub clipboard: Clipboard,
    /// Screen areas of the editor in the last drawn frame — its whole box,
    /// and just the text inside — for the mouse.
    pub editor_area: Rect,
    pub editor_text_area: Rect,
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
    /// Options, Command), paired with each one's index into the menu bar.
    pub menu_bar_tiles: Vec<(Rect, usize)>,
    /// Clickable regions of the open dropdown's items, paired with each
    /// one's index into the current category's item list. Empty unless
    /// `menu_open`.
    pub menu_item_tiles: Vec<(Rect, usize)>,
    /// When the last scroll-wheel event was processed, to debounce
    /// duplicate events some terminals/compositors emit for a single
    /// physical wheel click (see `debounced_scroll`).
    last_scroll_at: Option<Instant>,
    /// When the panes were last checked for changes on disk.
    last_refresh_check: Option<Instant>,
    /// A note from that check, waiting for whatever's open to close.
    pending_notice: Option<String>,
    /// `pce`: the editor is all there is (see `App::editor_only`). The
    /// one mark of that mode; what differs in it asks this.
    pub editor_only: bool,
    /// `pce`: a save over the file in place failed part-way, so the file
    /// may be damaged — and unless a later save worked, `pce` says so and
    /// ends with a failure, for git, crontab or sudoedit not to take it.
    pub damaged_file: Option<PathBuf>,
}

/// Directories named on the command line (`pc LEFT [RIGHT]`), already
/// checked and absolute. Each one overrides where that pane would otherwise
/// start.
#[derive(Debug, Default)]
pub struct StartDirs {
    pub left: Option<PathBuf>,
    pub right: Option<PathBuf>,
}

impl App {
    /// For tests: with no saved state to start from (see `state::test_dir`)
    /// and PC's own clipboard, so a test can never touch the desktop's.
    #[cfg(test)]
    pub fn new() -> Result<Self> {
        let mut app = Self::with_start_dirs(StartDirs::default())?;
        app.clipboard = Clipboard::internal();
        Ok(app)
    }

    pub fn with_start_dirs(start_dirs: StartDirs) -> Result<Self> {
        Self::build(start_dirs, false)
    }

    /// `pce`: the built-in editor alone, on `editor`, full screen — no
    /// panes to show, and none of their saved state read or written (the
    /// two kept behind the editor are just the current directory, for Save
    /// As of an untitled text to start from). Closing it ends the program.
    pub fn editor_only(mut editor: Editor) -> Result<Self> {
        let mut app = Self::build(StartDirs::default(), true)?;
        editor.full_screen = true;
        app.install_editor(editor);
        app.view_before_editor = None;
        Ok(app)
    }

    fn build(start_dirs: StartDirs, editor_only: bool) -> Result<Self> {
        // `pce` doesn't need the directory it's run in to be readable, or
        // still there: its panes are never listed.
        let cwd = if editor_only {
            env::current_dir().unwrap_or_default()
        } else {
            env::current_dir()?
        };
        let saved_settings = state::load_settings();
        let start_left_in_cwd = saved_settings
            .get(SettingItem::StartLeftInCwd.key())
            .copied()
            .unwrap_or(true);
        let pane_totals = saved_settings
            .get(SettingItem::PaneTotals.key())
            .copied()
            .unwrap_or(true);
        let last_dirs = if editor_only { None } else { state::load() };
        let (left_dir, right_dir) = match last_dirs {
            Some(last) if start_left_in_cwd => (cwd.clone(), last.right),
            Some(last) => (last.left, last.right),
            None => (cwd.clone(), cwd),
        };
        // An explicitly named directory beats both the launch dir and the
        // "Restore last session" setting.
        let left_dir = start_dirs.left.unwrap_or(left_dir);
        let right_dir = start_dirs.right.unwrap_or(right_dir);
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
        let builtin_editor = saved_settings
            .get(SettingItem::BuiltinEditor.key())
            .copied()
            .unwrap_or(true);
        let editor_focus_mode = saved_settings
            .get(SettingItem::EditorFocusMode.key())
            .copied()
            .unwrap_or(false);
        let wrap_lines = saved_settings.get(WRAP_LINES_KEY).copied().unwrap_or(false);
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
            left: if editor_only {
                Pane::unlisted(left_dir)
            } else {
                Pane::new(left_dir, hide_hidden_files)?
            },
            right: if editor_only {
                Pane::unlisted(right_dir)
            } else {
                Pane::new(right_dir, hide_hidden_files)?
            },
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
            about_open: false,
            error_dialog: None,
            error_dialog_is_notice: false,
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
            preview_scroll_x: 0,
            last_preview_target: None,
            image_preview: ImagePreview::default(),
            internal_preview,
            builtin_editor,
            editor_focus_mode,
            wrap_lines,
            editor: None,
            list_focus: false,
            after_prompt: None,
            renaming_edited_file: false,
            watch: EditedFileWatch::default(),
            preview_find: None,
            preview_last_search: String::new(),
            view_before_editor: None,
            clipboard: Clipboard::new(),
            editor_area: Rect::default(),
            editor_text_area: Rect::default(),
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
            last_refresh_check: None,
            pending_notice: None,
            editor_only,
            damaged_file: None,
        };
        if !editor_only {
            app.restore_sorts();
        }
        Ok(app)
    }

    /// Where the panes were, for the next start — not for `pce`, which
    /// has none of its own.
    pub fn save_state(&self) {
        if self.editor_only {
            return;
        }
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
        self.error_dialog_is_notice = false;
    }

    /// Something worth a moment's notice that isn't a failure (a paste cut
    /// to its first line): the same popup as `set_error`, but titled and
    /// colored as an ordinary note.
    pub fn set_notice(&mut self, message: String) {
        logging::log_info(&message);
        self.status_message = message.clone();
        self.error_dialog = Some(message);
        self.error_dialog_is_notice = true;
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

        // While editing, the menus don't offer what would act on the panes
        // behind the editor (see `menu::EDITOR_MENU_BAR`); this keeps any
        // other way to it shut as well. With the file list's keyboard
        // (Tab), the usual menus are back, all but Copy and Move, whose
        // target — the other pane — is under the editor (or the quick view).
        if self.ruled_out_by_covered_pane(action) {
            return false;
        }
        if self.editor_has_focus()
            && !matches!(
                action,
                Action::Save
                    | Action::SaveAs
                    | Action::Rename
                    | Action::Close
                    | Action::WrapLines
                    | Action::SelectAll
                    | Action::Find
                    | Action::FindNext
                    | Action::Help
                    | Action::Settings
                    | Action::About
                    | Action::ShowLogs
                    | Action::Quit
            )
        {
            return false;
        }

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
            Action::Save
            | Action::SaveAs
            | Action::Close
            | Action::SelectAll
            | Action::Find
            | Action::FindNext => self.editor.is_some(),
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
            | Action::WrapLines
            | Action::Settings
            | Action::About
            | Action::ShowTerminal
            | Action::ShowLogs
            | Action::OpenEditor
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

    /// Rows the current preview actually renders to (wrapped, with Wrap
    /// Lines on) — the right unit for clamping scroll, since a long logical
    /// line can span several visual rows.
    fn preview_wrapped_line_count(&self) -> usize {
        preview::build_preview(self.active_pane_ref())
            .screen_row_count(self.preview_visible_width, self.wrap_lines)
    }

    /// Left/Right with the focus on a preview that isn't wrapped: scrolls it
    /// sideways, a tab stop at a time, no further than the widest line
    /// needs to show its end.
    pub fn scroll_preview_sideways(&mut self, right: bool) {
        if self.wrap_lines {
            return;
        }
        let step = crate::editor::TAB_WIDTH;
        if right {
            let max = preview::build_preview(self.active_pane_ref())
                .widest_line()
                .saturating_sub(self.preview_visible_width);
            self.preview_scroll_x = (self.preview_scroll_x + step).min(max);
        } else {
            self.preview_scroll_x = self.preview_scroll_x.saturating_sub(step);
        }
    }

    /// Edit > Wrap Lines or Options > Wrap Lines, while the quick view shows
    /// text: the same line stays at the top, though the rows above it now
    /// count differently.
    fn rescroll_preview_for_wrap(&mut self, was_wrapped: bool, scroll: usize) -> usize {
        let content = preview::build_preview(self.active_pane_ref());
        let width = self.preview_visible_width;
        let line = content.line_at_row(scroll, width, was_wrapped);
        let max = content
            .screen_row_count(width, self.wrap_lines)
            .saturating_sub(self.preview_visible_lines);
        content.first_row_of(line, width, self.wrap_lines).min(max)
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

    /// The quick view's find, for the file it shows now: made on first use
    /// (carrying the last search over), and only for text. `false` when
    /// there's nothing to search.
    fn ensure_preview_find(&mut self) -> bool {
        let pane = self.active_pane_ref();
        let Some(target) = pane.preview_target() else {
            return false;
        };
        if self
            .preview_find
            .as_ref()
            .is_some_and(|find| find.target == target)
        {
            return true;
        }
        let truncated = pane
            .selected_entry()
            .is_some_and(|entry| entry.size > preview::PREVIEW_READ_LIMIT as u64);
        let preview::PreviewContent::Text(lines) = preview::build_preview(pane) else {
            return false;
        };
        let Ok(mut text) = Editor::from_bytes(target.clone(), lines.join("\n").as_bytes()) else {
            return false;
        };
        text.set_last_search(&self.preview_last_search);
        self.preview_find = Some(PreviewFind {
            target,
            text,
            truncated,
            source: lines,
        });
        true
    }

    /// Whether the quick view has the keyboard and something to find, so
    /// that F3 is Find Next there rather than closing it (and its F-key
    /// tile says so).
    pub fn preview_next_mode(&self) -> bool {
        if self.editor.is_some() || !self.quick_view || !self.preview_focus {
            return false;
        }
        if self.preview_find_bar_open() {
            return true;
        }
        !self.preview_last_search.is_empty()
            && matches!(
                preview::build_preview(self.active_pane_ref()),
                preview::PreviewContent::Text(_)
            )
    }

    pub fn preview_find_bar_open(&self) -> bool {
        self.preview_find
            .as_ref()
            .is_some_and(|find| find.text.find.is_some())
    }

    /// Ctrl+F with the focus on the quick view: its find bar, for a text
    /// file. Returns whether there was one to open (not for a listing, a
    /// binary file or a picture).
    pub fn open_preview_find(&mut self) -> bool {
        if !self.ensure_preview_find() {
            return false;
        }
        self.run_preview_find(|text| text.apply(EditorCommand::Find));
        true
    }

    /// A key while the quick view's find bar is open. Typing and the bar's
    /// keys go to it; `false` for any other key, for the caller to treat
    /// as usual (scrolling the preview, say).
    pub fn preview_find_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if !self.preview_find_bar_open() {
            return false;
        }
        let Some(key) = editor::find_bar_key(code, modifiers) else {
            return false;
        };
        self.run_preview_find(|text| text.find_key(key));
        true
    }

    /// F3/Shift+F3 in the quick view: the next/previous match, the bar open
    /// or not.
    pub fn preview_find_next(&mut self, forward: bool) {
        if !self.ensure_preview_find() {
            return;
        }
        let command = if forward {
            EditorCommand::FindNext
        } else {
            EditorCommand::FindPrevious
        };
        self.run_preview_find(|text| text.apply(command));
    }

    /// Esc in the quick view with a match highlighted (the bar closed):
    /// lets go of it. Returns whether there was one.
    pub fn clear_preview_match(&mut self) -> bool {
        let Some(find) = &mut self.preview_find else {
            return false;
        };
        if find.text.selection().is_none() {
            return false;
        }
        find.text.deselect();
        true
    }

    /// Runs a find step, then remembers the search and brings the match it
    /// landed on into view.
    fn run_preview_find(&mut self, step: impl FnOnce(&mut Editor)) {
        let Some(find) = &mut self.preview_find else {
            return;
        };
        step(&mut find.text);
        if !find.text.last_search().is_empty() {
            self.preview_last_search = find.text.last_search().to_string();
        }
        self.scroll_preview_to_match();
    }

    /// Scrolls the quick view so the current match shows: down or up to its
    /// row, and — not wrapping — sideways to its columns.
    fn scroll_preview_to_match(&mut self) {
        let Some((start, end)) = self
            .preview_find
            .as_ref()
            .and_then(|find| find.text.selection())
        else {
            return;
        };
        let content = preview::build_preview(self.active_pane_ref());
        let preview::PreviewContent::Text(lines) = &content else {
            return;
        };
        let width = self.preview_visible_width;
        // The find bar takes the preview's last row.
        let bar = usize::from(self.preview_find_bar_open());
        let rows = self.preview_visible_lines.saturating_sub(bar).max(1);
        let wrap = self
            .wrap_lines
            .then(|| crate::editor::wrap_width_for(width));
        let layout = crate::editor::LineLayout::new(&lines[start.line], wrap);
        let sub = layout.starts.partition_point(|&at| at <= start.col) - 1;
        let row = content.first_row_of(start.line, width, self.wrap_lines) + sub;
        if row < self.preview_scroll {
            self.preview_scroll = row;
        } else if row >= self.preview_scroll + rows {
            self.preview_scroll = row + 1 - rows;
        }
        if !self.wrap_lines {
            let from = layout.x_of(0, start.col);
            let to = layout.x_of(
                0,
                if end.line == start.line {
                    end.col
                } else {
                    start.col
                },
            );
            if from < self.preview_scroll_x {
                self.preview_scroll_x = from;
            } else if to > self.preview_scroll_x + width {
                self.preview_scroll_x = to.saturating_sub(width).min(from);
            }
        }
    }

    /// Called every loop iteration: twice a second, brings both listings up
    /// to date with the disk (see `refresh_from_disk`).
    pub fn auto_refresh(&mut self, now: Instant) {
        const EVERY: Duration = Duration::from_millis(500);
        if self
            .last_refresh_check
            .is_some_and(|at| now.duration_since(at) < EVERY)
        {
            return;
        }
        self.last_refresh_check = Some(now);
        self.refresh_from_disk();
    }

    /// Lists a pane again when its directory changed since it was listed
    /// (one look at the directory itself, nothing more, when it hasn't) —
    /// the cursor on the same name, marks that still exist kept. This goes
    /// on under dialogs, the menu and quick search too: what they act on was
    /// taken when they opened (the items to copy or delete, the file to
    /// rename), and quick search goes on from the entry the cursor is on,
    /// found again by name.
    ///
    /// A directory that's gone sends its pane up to the nearest one that
    /// isn't, with a note saying so — held back while something else is
    /// open, so popups don't pile up. Not while a dialog that works in the
    /// pane's directory when it's confirmed (MkDir, New File, a Copy/Move
    /// target typed relative to it) is open, though: that would quietly
    /// move its work up a level, so the pane waits until it closes.
    pub fn refresh_from_disk(&mut self) {
        let busy = self.something_open();
        // Save As of an untitled text names it relative to the active pane.
        let works_in_pane_dir = matches!(
            self.dialog,
            Dialog::TextInput {
                kind: TextInputKind::MkDir | TextInputKind::NewFile | TextInputKind::SaveAs,
                ..
            } | Dialog::Transfer(_)
        );
        // `pce` shows no panes, so has none to keep up to date.
        let sides: &[Side] = if self.editor_only {
            &[]
        } else {
            &[Side::Left, Side::Right]
        };
        for &side in sides {
            match self.pane_mut(side).changed_on_disk() {
                Ok(false) => {}
                // A failure is reported once: the listing is then up to
                // date with what can be seen of the directory.
                Ok(true) => {
                    if let Err(err) = self.pane_mut(side).reload_keeping_selection() {
                        self.set_error(format!("Cannot reload pane: {err}"));
                    }
                }
                Err(_) if works_in_pane_dir => {}
                Err(_) => {
                    let gone = self.pane_mut(side).cwd.clone();
                    if let Some(now) = self.pane_mut(side).leave_for_nearest_existing() {
                        self.pending_notice = Some(format!(
                            "{} is gone; showing {} instead",
                            gone.display(),
                            now.display()
                        ));
                    }
                }
            }
        }
        self.check_edited_file(false);
        if !busy && let Some(note) = self.pending_notice.take() {
            self.set_notice(note);
        }
        // A file changed under the quick view shows anew by itself (see
        // `preview::read_file`); a search in its old text is dropped.
        if let Some(find) = &self.preview_find {
            let same = match preview::build_preview(self.active_pane_ref()) {
                preview::PreviewContent::Text(lines) => {
                    std::rc::Rc::ptr_eq(&lines, &find.source) || *lines == *find.source
                }
                _ => false,
            };
            if !same {
                self.preview_find = None;
            }
        }
    }

    /// Resets the preview scroll position whenever the previewed entry has
    /// changed since the last call (any kind of selection/cwd change).
    /// Called once per input loop iteration rather than at every individual
    /// call site that can move the selection, so nothing is missed.
    pub fn sync_preview_scroll(&mut self) {
        let target = self.active_pane_ref().preview_target();
        if target != self.last_preview_target {
            self.preview_scroll = 0;
            self.preview_scroll_x = 0;
            self.preview_find = None;
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
        // The edited file moved or put in the trash: the editor says so.
        if kind == DialogKind::Move {
            for (name, src) in &items {
                if !src.exists() {
                    self.edited_file_moved(src, &dest_dir.join(name));
                }
            }
        }
        self.check_edited_file(true);
        // Not `?`: a pane that can't be listed any more (its directory
        // deleted meanwhile) mustn't end the program — least of all with
        // unsaved changes in the editor.
        if let Err(err) = self.left.reload() {
            self.set_error(format!("Cannot reload left pane: {err}"));
        }
        if let Err(err) = self.right.reload() {
            self.set_error(format!("Cannot reload right pane: {err}"));
        }
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

    /// What the file list can't do while the editor or the F3 quick view
    /// covers the other pane: Copy and Move, whose target is that pane's
    /// directory, out of sight. One rule for the menu, the F-key bar and
    /// the keys.
    pub fn ruled_out_by_covered_pane(&self, action: Action) -> bool {
        (self.editor.is_some() || self.quick_view) && matches!(action, Action::Copy | Action::Move)
    }

    /// Whether the editor is open and has the keyboard (rather than the
    /// file list beside it; see `list_focus`).
    pub fn editor_has_focus(&self) -> bool {
        self.editor.is_some() && !self.list_focus
    }

    /// Tab between the editor in its pane and the file list beside it.
    pub fn toggle_editor_focus(&mut self) {
        self.set_list_focus(!self.list_focus);
    }

    /// The keyboard to the file list beside the editor (`true`) or to the
    /// editor — by Tab or a click. A quick search on the list ends with it.
    pub fn set_list_focus(&mut self, list: bool) {
        if self.editor.is_some() && self.list_focus != list {
            self.list_focus = list;
            self.quick_search = None;
            self.reset_menu();
        }
    }

    /// The menu bar as it is now: with the Edit menu while editing (the
    /// usual one while the file list has the keyboard), Wrap Lines in
    /// Options while the quick view is open.
    pub fn menu_bar(&self) -> &'static [MenuCategory] {
        if self.editor_only {
            return menu::PCE_MENU_BAR;
        }
        menu::menu_bar(self.editor_has_focus(), self.quick_view)
    }

    /// The menu category whose title starts with `letter` (case
    /// insensitive), if any — used for the Alt+F/E/O/C mnemonic shortcuts.
    pub fn menu_category_for(&self, letter: char) -> Option<usize> {
        let lower = letter.to_ascii_lowercase();
        self.menu_bar()
            .iter()
            .position(|category| category.title.to_ascii_lowercase().starts_with(lower))
    }

    /// The keybinding shown beside `action` in the pulldown. While editing,
    /// Ctrl+S and Ctrl+F are the editor's (Save, Find), so the panes' own
    /// uses of them aren't shown.
    /// Wrap Lines shows its on/off mark there instead, at the right edge
    /// like the keys.
    pub fn menu_shortcut(&self, action: Action) -> Option<&'static str> {
        if action == Action::WrapLines {
            return Some(if self.wrap_lines { "[x]" } else { "[ ]" });
        }
        if self.editor_has_focus() && matches!(action, Action::Settings | Action::QuickSearch) {
            return None;
        }
        action.shortcut()
    }

    /// The menu bar changes as the editor opens or closes (see
    /// `menu::menu_bar`): no menu stays open across that, and nothing points
    /// into the other bar's categories or items.
    fn reset_menu(&mut self) {
        self.menu_open = false;
        self.menu_category = 0;
        self.menu_item = 0;
        self.menu_bar_tiles.clear();
        self.menu_item_tiles.clear();
    }

    pub fn open_menu(&mut self) {
        self.open_menu_at(0);
    }

    /// Opens the menu directly on `category` — used when clicking a menu
    /// bar label, which should jump straight to that category rather than
    /// always starting from File like `open_menu` does.
    pub fn open_menu_at(&mut self, category: usize) {
        self.menu_open = true;
        self.menu_category = category.min(self.menu_bar().len() - 1);
        self.menu_item = 0;
        self.snap_menu_item_to_enabled();
    }

    pub fn close_menu(&mut self) {
        self.menu_open = false;
    }

    pub fn menu_left(&mut self) {
        if self.menu_category == 0 {
            self.menu_category = self.menu_bar().len() - 1;
        } else {
            self.menu_category -= 1;
        }
        self.menu_item = 0;
        self.snap_menu_item_to_enabled();
    }

    pub fn menu_right(&mut self) {
        self.menu_category = (self.menu_category + 1) % self.menu_bar().len();
        self.menu_item = 0;
        self.snap_menu_item_to_enabled();
    }

    /// Moves `self.menu_item` to the first enabled item at or after index 0,
    /// used whenever the menu/category first opens so the selection never
    /// starts on a grayed-out entry.
    fn snap_menu_item_to_enabled(&mut self) {
        let items = self.menu_bar()[self.menu_category].items;
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
        let items = self.menu_bar()[self.menu_category].items;
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
        let action = self.menu_bar()[self.menu_category].items[self.menu_item];
        self.menu_open = false;
        self.run_action(action)
    }

    pub fn run_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Open => self.open_selected()?,
            Action::Rename if self.editor_has_focus() => self.request_rename_edited_file(),
            // The file list has the keyboard beside the editor: F3 closes
            // the editor for the quick view, F4 opens (or goes back to) the
            // file under the cursor, and Copy/Move, whose target is the
            // other pane, wait for the editor to close.
            Action::View if self.editor.is_some() => self.quick_view_from_list(),
            Action::Edit if self.editor.is_some() => self.edit_from_list(),
            Action::Copy | Action::Move if self.ruled_out_by_covered_pane(action) => {
                let verb = if action == Action::Copy {
                    "Copy"
                } else {
                    "Move"
                };
                let (what, close) = if self.editor.is_some() {
                    ("the editor", "the editor")
                } else {
                    ("the quick view", "it")
                };
                self.set_notice(format!(
                    "{verb} needs the other pane, which {what} has: close {close} first (Esc)"
                ));
            }
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
            Action::Edit if self.builtin_editor => self.open_editor(),
            Action::Edit => self.request_external(ExternalRequest::Edit, "edit"),
            Action::Save => {
                self.save_editor();
            }
            Action::SaveAs => self.open_save_as(None),
            Action::OpenEditor => self.edit_untitled(),
            Action::Close => self.close_editor(),
            Action::WrapLines => self.toggle_wrap_lines(),
            Action::SelectAll => self.apply_to_editor(EditorCommand::SelectAll),
            Action::Find => self.apply_to_editor(EditorCommand::Find),
            Action::FindNext => self.apply_to_editor(EditorCommand::FindNext),
            Action::Help => {
                self.help_open = true;
                self.help_page = 0;
            }
            Action::ShowTerminal => self.request_reveal_terminal(),
            Action::ShowLogs => self.logs_open = true,
            Action::About => self.about_open = true,
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
            Action::Quit if self.editor.as_ref().is_some_and(Editor::is_modified) => {
                self.dialog = Dialog::UnsavedChanges {
                    quit_after: true,
                    focused: 0,
                };
            }
            // `pce` quits as an editor does, without "Really quit?".
            Action::Quit if self.editor_only => self.quit(),
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
            SettingItem::BuiltinEditor => self.builtin_editor,
            SettingItem::EditorFocusMode => self.editor_focus_mode,
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
            SettingItem::BuiltinEditor => self.builtin_editor = value,
            SettingItem::EditorFocusMode => self.editor_focus_mode = value,
        }
    }

    fn persist_settings(&self) {
        let mut pairs: Vec<(&str, bool)> = SettingItem::ALL
            .iter()
            .map(|item| (item.key(), self.setting_value(*item)))
            .collect();
        pairs.push((WRAP_LINES_KEY, self.wrap_lines));
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

    /// Enter: opens the selected directory; for a known media file (image,
    /// PDF, audio, video, HTML), hands it to the desktop's default viewer
    /// (`xdg-open`, `open` on macOS, or Explorer on Windows) — except an
    /// image on a bare Linux console, which PC shows itself (`tty_image`),
    /// as there's no desktop there; for any other file with the executable
    /// permission bit set, runs it exactly as if its name had been typed on
    /// the command line (`./name`, or just `name` on Windows, which has no
    /// such convention). Media go first because some filesystems (vfat,
    /// exFAT, NTFS mounts) mark every file executable. Anything else does
    /// nothing, same as before.
    pub fn open_selected(&mut self) -> Result<()> {
        if self.show_image_on_console() {
            return Ok(());
        }
        let pane = self.active_pane_ref();

        if let Some(entry) = pane.selected_entry()
            && !entry.is_dir
            && is_media_file(&entry.name)
            && let Some(path) = pane.selected_path()
        {
            self.open_externally(&path);
            return Ok(());
        }

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
        self.dialog = Dialog::Rename {
            field: TextField::new(name),
            editing: true,
            src,
        };
        self.dialog_cancel_focused = false;
    }

    /// F2 while editing: the usual Rename dialog, for the file being edited
    /// (whatever the panes show). The editor goes on with the renamed file —
    /// its text, cursor, undo and unsaved changes as they were; nothing is
    /// saved.
    fn request_rename_edited_file(&mut self) {
        let Some(editor) = &self.editor else {
            return;
        };
        // No file to rename yet: naming it is saving it.
        if editor.untitled {
            self.open_save_as(None);
            return;
        }
        self.dialog = Dialog::Rename {
            field: TextField::new(editor.file_name()),
            editing: true,
            src: editor.path.clone(),
        };
        self.dialog_cancel_focused = false;
        self.renaming_edited_file = true;
    }

    /// The F-key bar while editing, where it shows (the editor in a pane):
    /// each key's label and whether it does anything there. F3 is Find
    /// Next and F4 full screen, so they're labeled that way; the panes'
    /// own F5–F8 are shown dimmed.
    pub fn editor_fn_key(&self, key: &menu::FnKey) -> (&'static str, bool) {
        match key.action {
            // Only with something to look for (Ctrl+F alone starts that).
            Some(Action::View) => (
                "Next",
                self.editor.as_ref().is_some_and(Editor::can_find_next),
            ),
            Some(Action::Edit) => ("Full", true),
            Some(Action::Copy | Action::Move | Action::MkDir | Action::Delete) => {
                (key.label, false)
            }
            _ => (key.label, true),
        }
    }

    /// A click on an F-key tile while editing: what that key does in the
    /// editor, or nothing for a dimmed one.
    pub fn click_editor_fn_key(&mut self, action: Option<Action>) -> Result<()> {
        let command = match action {
            None => EditorCommand::Menu(None),
            Some(Action::Help) => EditorCommand::Help,
            Some(Action::Rename) => EditorCommand::Rename,
            Some(Action::View) => EditorCommand::FindNext,
            Some(Action::Edit) => EditorCommand::ToggleFullScreen,
            Some(Action::Quit) => EditorCommand::Quit,
            Some(_) => return Ok(()),
        };
        self.run_editor_command(command)
    }

    /// After a rename: a pane whose cursor was on the renamed entry
    /// (`was_on`, left and right) keeps it there under the new name. The
    /// edited file renamed with F2 from the editor itself is followed there;
    /// renamed from the list beside it, it isn't — Save then asks where to
    /// save, suggesting the new name (see `save_editor`).
    fn after_rename(&mut self, src: &Path, dest: &Path, was_on: [bool; 2]) {
        if let Some(name) = dest.file_name() {
            let name = name.to_string_lossy();
            for (side, was_on) in [Side::Left, Side::Right].into_iter().zip(was_on) {
                if was_on {
                    self.pane_mut(side).select_name(&name);
                }
            }
        }
        if std::mem::take(&mut self.renaming_edited_file) {
            if let Some(editor) = &mut self.editor
                && editor.path == src
            {
                editor.path = dest.to_path_buf();
                editor.disk_id = fs_ops::file_identity(dest);
            }
        } else {
            self.edited_file_moved(src, dest);
        }
    }

    /// The edited file (or a directory it's in) was renamed or moved from
    /// `src` to `dest` in the app: noted, for Save to suggest it.
    fn edited_file_moved(&mut self, src: &Path, dest: &Path) {
        if let Some(editor) = &self.editor
            && let Ok(rest) = editor.path.strip_prefix(src)
        {
            self.watch.moved_to = Some(if rest.as_os_str().is_empty() {
                dest.to_path_buf()
            } else {
                dest.join(rest)
            });
            self.check_edited_file(true);
        }
    }

    /// Whether a popup, the menu or a quick search is up, which a note
    /// shouldn't land on top of (see `notice_when_free`).
    fn something_open(&self) -> bool {
        self.menu_open
            || self.quick_search.is_some()
            || self.error_dialog.is_some()
            || !matches!(self.dialog, Dialog::None)
    }

    /// A note now, or once whatever's open has closed.
    fn notice_when_free(&mut self, note: String) {
        if self.something_open() {
            self.pending_notice = Some(note);
        } else {
            self.set_notice(note);
        }
    }

    /// The edited file isn't where it was (renamed, moved, deleted — here
    /// or by another program — or replaced by another file): the editor
    /// keeps its text and its old name, says so once, and Save asks for a
    /// name. Gone without a trace — no new name for it known, or that gone
    /// too — the text counts as unsaved, so closing asks first; that's
    /// checked on every look, after the note too. One look at the file,
    /// cheap enough for every refresh. Unless `certain` (the app itself
    /// just moved or deleted it), a file missing once is looked at again
    /// before it counts as gone: some programs save by deleting the file
    /// and writing it anew.
    fn check_edited_file(&mut self, certain: bool) {
        let Some(editor) = &mut self.editor else {
            return;
        };
        if editor.untitled || editor.new_file {
            return;
        }
        let what = match std::fs::metadata(&editor.path) {
            Ok(meta) => {
                self.watch.missing_once = false;
                let now = fs_ops::identity_of(&meta);
                // Without a remembered identity (opened before it could be
                // told), only its being gone is noticed.
                if editor.disk_id.is_none_or(|was| was.same_file(&now)) {
                    return;
                }
                "was replaced on disk".to_string()
            }
            Err(_) => {
                if !certain && !std::mem::replace(&mut self.watch.missing_once, true) {
                    return;
                }
                let elsewhere = self.watch.moved_to.as_ref().is_some_and(|to| to.exists());
                if !elsewhere {
                    editor.mark_unsaved();
                }
                match &self.watch.moved_to {
                    Some(to) => format!("was renamed to {}", to.display()),
                    None => "was renamed or deleted".to_string(),
                }
            }
        };
        if std::mem::replace(&mut self.watch.noted, true) {
            return;
        }
        let name = editor.file_name();
        self.notice_when_free(format!("\"{name}\" {what}: Save will ask for a name"));
    }

    /// F3 with the file list's keyboard beside the editor: the editor makes
    /// way for the quick view of the file under the cursor — after asking
    /// about unsaved changes.
    fn quick_view_from_list(&mut self) {
        if self.editor.as_ref().is_some_and(Editor::is_modified) {
            self.ask_before_leaving(AfterPrompt::QuickView);
            return;
        }
        self.leave_for_quick_view();
    }

    fn leave_for_quick_view(&mut self) {
        self.drop_editor();
        if self.active_pane_ref().selected_entry().is_some() {
            self.quick_view = true;
            self.preview_focus = false;
            self.preview_scroll = 0;
            self.preview_scroll_x = 0;
        }
    }

    /// The unsaved-changes prompt, with what to do once it's answered.
    fn ask_before_leaving(&mut self, then: AfterPrompt) {
        self.after_prompt = Some(then);
        self.dialog = Dialog::UnsavedChanges {
            quit_after: false,
            focused: 0,
        };
    }

    /// F4 with the file list's keyboard beside the editor: on the edited
    /// file, back to the editor; on another one, that file in the editor
    /// instead (asking first about unsaved changes).
    fn edit_from_list(&mut self) {
        let pane = self.active_pane_ref();
        let Some(entry) = pane.selected_entry() else {
            return;
        };
        if entry.is_dir {
            self.set_status("Cannot edit a directory".to_string());
            return;
        }
        let Some(path) = pane.selected_path() else {
            return;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.path == path)
        {
            self.list_focus = false;
            return;
        }
        // The External editor setting holds for the list too: $EDITOR gets
        // the file, and the built-in editor (and its text) stays as it is.
        if !self.builtin_editor {
            self.request_external(ExternalRequest::Edit, "edit");
            return;
        }
        self.edit_path(path);
    }

    /// `path` in the built-in editor. With another file already being
    /// edited, that one makes way, after asking about its unsaved changes —
    /// but only once the new one has opened: a file the editor won't take
    /// (binary, too big, ...) says so, and the one being edited stays.
    fn edit_path(&mut self, path: PathBuf) {
        let editor = match Editor::open(&path) {
            Ok(editor) => editor,
            Err(message) => {
                self.set_error(message);
                return;
            }
        };
        if self.editor.as_ref().is_some_and(Editor::is_modified) {
            self.ask_before_leaving(AfterPrompt::Edit(Box::new(editor)));
            return;
        }
        self.install_editor(editor);
    }

    /// Command > Open Editor: the built-in editor (whatever the setting) on
    /// no file at all, "Untitled", in the other pane like F4. A file being
    /// edited makes way, after asking about its unsaved changes.
    fn edit_untitled(&mut self) {
        if self.editor.as_ref().is_some_and(Editor::is_modified) {
            self.ask_before_leaving(AfterPrompt::Untitled);
            return;
        }
        self.install_editor(Editor::untitled());
    }

    /// `editor` takes the other pane (or the place of the editor there,
    /// keeping its full screen and what to go back to on closing), with the
    /// keyboard.
    fn install_editor(&mut self, mut editor: Editor) {
        editor.set_wrap(self.wrap_lines);
        match &self.editor {
            Some(old) => editor.full_screen = old.full_screen,
            None => {
                self.view_before_editor = Some(ViewBeforeEditor {
                    quick_view: self.quick_view,
                    preview_focus: self.preview_focus,
                    preview_scroll: self.preview_scroll,
                    preview_scroll_x: self.preview_scroll_x,
                    wrap_lines: self.wrap_lines,
                });
                self.quick_view = false;
                self.preview_focus = false;
                self.preview_find = None;
            }
        }
        self.editor = Some(editor);
        self.list_focus = false;
        self.watch = EditedFileWatch::default();
        self.reset_menu();
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
        let dest_dir = self.inactive_pane_ref().cwd.clone();
        self.dialog = Dialog::Transfer(Transfer::new(kind, items, dest_dir));
        self.dialog_cancel_focused = !kind.default_yes();
    }

    /// F3: toggles the internal quick preview if that setting is on
    /// (keyboard focus stays on the file listing; Tab moves it onto the
    /// preview to scroll), closing it again on a second press regardless of
    /// which side currently has focus. Otherwise shells out to $PAGER/less
    /// as before.
    fn view_selected(&mut self) {
        // The console can't show a picture inside a pane, only full-screen.
        if !self.quick_view && self.show_image_on_console() {
            return;
        }
        if !self.internal_preview {
            self.request_external(ExternalRequest::View, "view");
            return;
        }
        if self.quick_view {
            self.quick_view = false;
            self.preview_focus = false;
            self.preview_find = None;
            return;
        }
        if self.active_pane_ref().selected_entry().is_none() {
            return;
        }
        self.quick_view = true;
        self.preview_scroll = 0;
    }

    /// On a bare Linux console, queues the full-screen viewer for the selected
    /// entry if it's an image. Returns whether it did.
    fn show_image_on_console(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        {
            let pane = self.active_pane_ref();
            if self.selected_is_image()
                && crate::tty_image::on_console()
                && let Some(path) = pane.selected_path()
            {
                self.external_request = Some(ExternalRequest::ShowImage(path));
                return true;
            }
        }
        false
    }

    fn selected_is_image(&self) -> bool {
        self.active_pane_ref()
            .selected_entry()
            .is_some_and(|entry| !entry.is_dir && preview::is_image(&entry.name))
    }

    /// Whether the quick-view pane is about to show its first picture, so
    /// the terminal has to be asked how to draw one. Never on a bare console,
    /// whose pictures are full-screen.
    pub fn image_preview_needs_detection(&self) -> bool {
        if !self.quick_view || self.image_preview.is_detected() || !self.selected_is_image() {
            return false;
        }
        #[cfg(target_os = "linux")]
        if crate::tty_image::on_console() {
            return false;
        }
        true
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

    /// F4 with the built-in editor on: the selected file opens in the other
    /// pane, where F3 shows its preview (which it replaces). A file the
    /// editor can't save back exactly — binary, not UTF-8, huge — isn't
    /// opened at all.
    fn open_editor(&mut self) {
        let pane = self.active_pane_ref();
        let Some(entry) = pane.selected_entry() else {
            return;
        };
        if entry.is_dir {
            self.set_status("Cannot edit a directory".to_string());
            return;
        }
        let Some(path) = pane.selected_path() else {
            return;
        };
        match Editor::open(&path) {
            Ok(editor) => self.install_editor(editor),
            Err(message) => self.set_error(message),
        }
    }

    /// A key while the editor has the keyboard. With the find bar open,
    /// typing goes there, and of the editor's own keys only those that make
    /// sense alongside it still work.
    pub fn editor_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        let Some(editor) = &mut self.editor else {
            return Ok(());
        };
        // In its pane, Tab hands the keyboard to the file list beside it, as
        // Tab does between the list and F3's preview; full screen, with no
        // list in sight, it's four spaces as ever.
        if code == KeyCode::Tab && !editor.full_screen {
            self.toggle_editor_focus();
            return Ok(());
        }
        let command = if editor.find.is_some() {
            if let Some(key) = editor::find_bar_key(code, modifiers) {
                editor.find_key(key);
                return Ok(());
            }
            match editor::command_for_key(code, modifiers) {
                Some(
                    command @ (EditorCommand::Save
                    | EditorCommand::ToggleFullScreen
                    | EditorCommand::Find
                    | EditorCommand::Paste
                    | EditorCommand::Help
                    | EditorCommand::Quit
                    | EditorCommand::Menu(_)),
                ) => command,
                _ => return Ok(()),
            }
        } else {
            match editor::command_for_key(code, modifiers) {
                Some(command) => command,
                None => return Ok(()),
            }
        };
        self.run_editor_command(command)
    }

    fn run_editor_command(&mut self, command: EditorCommand) -> Result<()> {
        let Some(editor) = &mut self.editor else {
            return Ok(());
        };
        match command {
            EditorCommand::Copy => {
                if let Some(text) = editor.selected_text() {
                    self.clipboard.copy(text);
                }
            }
            EditorCommand::Cut => {
                if let Some(text) = editor.cut() {
                    self.clipboard.copy(text);
                }
            }
            EditorCommand::Paste => {
                let text = self.clipboard.paste();
                self.editor_paste(&text);
            }
            EditorCommand::Save => {
                self.save_editor();
            }
            // Esc goes back a step at a time: a selection (a match F3 found,
            // or one made by hand) is let go of first, and only with nothing
            // selected does Esc close the editor.
            EditorCommand::Close if editor.selection().is_some() => editor.deselect(),
            EditorCommand::Close => self.close_editor(),
            // `pce` has no pane to go back to.
            EditorCommand::ToggleFullScreen if self.editor_only => {}
            EditorCommand::ToggleFullScreen => {
                editor.full_screen = !editor.full_screen;
                self.list_focus = false;
            }
            EditorCommand::Help => {
                self.help_open = true;
                self.help_page = EDITOR_HELP_PAGE;
            }
            EditorCommand::Rename => self.request_rename_edited_file(),
            EditorCommand::Quit => self.run_action(Action::Quit)?,
            EditorCommand::Menu(None) => self.open_menu(),
            EditorCommand::Menu(Some(letter)) => {
                if let Some(idx) = self.menu_category_for(letter) {
                    self.open_menu_at(idx);
                }
            }
            other => editor.apply(other),
        }
        Ok(())
    }

    fn apply_to_editor(&mut self, command: EditorCommand) {
        if let Some(editor) = &mut self.editor {
            editor.apply(command);
        }
    }

    /// Ctrl+V, or text the terminal pasted (bracketed paste).
    pub fn editor_paste(&mut self, text: &str) {
        if let Some(editor) = &mut self.editor {
            editor.insert_text(text);
        }
    }

    /// Edit > Wrap Lines, or Options > Wrap Lines: switches it for the open
    /// file and the quick view alike, and remembers it.
    fn toggle_wrap_lines(&mut self) {
        self.wrap_lines = !self.wrap_lines;
        if self.quick_view {
            self.preview_scroll =
                self.rescroll_preview_for_wrap(!self.wrap_lines, self.preview_scroll);
            self.preview_scroll_x = 0;
        }
        if let Some(editor) = &mut self.editor {
            editor.set_wrap(self.wrap_lines);
        }
        self.persist_settings();
    }

    /// Ctrl+S: writes the file back. A failure is reported and the editor
    /// stays open with the changes still marked unsaved. Returns whether it
    /// saved. Where it would write somewhere else than the user thinks —
    /// the file gone or replaced under the editor (see
    /// `check_edited_file`), or no file yet (Untitled) — it asks for a name
    /// (Save As) instead, and returns `false`.
    pub fn save_editor(&mut self) -> bool {
        let Some(editor) = &self.editor else {
            return false;
        };
        let path = editor.path.clone();
        if editor.untitled {
            self.open_save_as(None);
            return false;
        }
        // Not made yet (`pce new.txt`): made now — unless something else
        // took the name meanwhile, which isn't replaced unasked.
        if editor.new_file {
            if std::fs::symlink_metadata(&path).is_ok() {
                self.confirm_save_over(path);
                return false;
            }
            return self.write_editor_to(&path, false);
        }
        let still_it = match (editor.disk_id, fs_ops::file_identity(&path)) {
            (Some(was), Some(now)) => was.same_file(&now),
            (None, now) => now.is_some(),
            (Some(_), None) => false,
        };
        if !still_it {
            let suggestion = self.watch.moved_to.clone().unwrap_or(path);
            self.open_save_as(Some(&suggestion));
            return false;
        }
        self.write_editor_to(&path, false)
    }

    /// File > Save As (or a save that has to ask where): a name, in a field
    /// filled in with the file's own (or `suggestion`), next to the file
    /// unless a path says otherwise.
    fn open_save_as(&mut self, suggestion: Option<&Path>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let path = suggestion.unwrap_or(&editor.path);
        let text = match (path.parent(), editor.path.parent(), path.file_name()) {
            _ if editor.untitled => "untitled.txt".to_string(),
            (Some(dir), Some(own_dir), Some(name)) if dir == own_dir => {
                name.to_string_lossy().into_owned()
            }
            _ => path.display().to_string(),
        };
        self.dialog = Dialog::TextInput {
            kind: TextInputKind::SaveAs,
            field: TextField::new(text),
            editing: true,
        };
        self.dialog_cancel_focused = false;
    }

    /// The name typed in Save As, as a path: `~` for home, and a relative
    /// one next to the edited file — for an untitled one, in the directory
    /// the user is in (the active pane's).
    fn save_as_target(&self, typed: &str) -> Option<PathBuf> {
        let editor = self.editor.as_ref()?;
        let typed = crate::transfer::expand_home(typed.trim())?;
        let base = if editor.untitled {
            // `pce` run in a directory that's gone: only a full path will do.
            Some(self.active_pane_ref().cwd.as_path()).filter(|dir| dir.is_dir())
        } else {
            editor.path.parent()
        };
        if base.is_none() && !typed.is_absolute() {
            return None;
        }
        let target = if typed.is_absolute() {
            typed
        } else {
            base.map_or_else(|| typed.clone(), |dir| dir.join(&typed))
        };
        // `..` and links resolved where the directory is there, so the
        // file is recognised in a pane listing that directory.
        Some(match (target.parent(), target.file_name()) {
            (Some(dir), Some(name)) => match dir.canonicalize() {
                Ok(dir) => crate::pane::strip_verbatim_prefix(dir).join(name),
                Err(_) => target.clone(),
            },
            _ => target,
        })
    }

    /// Saving onto `target`, which is there: asks first, No by default.
    fn confirm_save_over(&mut self, target: PathBuf) {
        self.dialog = Dialog::ConfirmSaveAs { target };
        self.dialog_cancel_focused = true;
    }

    /// Writes the editor's text to `path`, which from then on is the file
    /// it edits. With `show`, the panes put their cursor on it (Save As).
    fn write_editor_to(&mut self, path: &Path, show: bool) -> bool {
        let Some(editor) = &mut self.editor else {
            return false;
        };
        // A file that isn't there yet is made for this: only if it still
        // isn't (else Replace is asked), and with the permissions of the
        // file the text came from, so a private one's copy stays private.
        let new = std::fs::symlink_metadata(path).is_err();
        let mut damaged = false;
        let written = if new {
            let mode = if editor.untitled { None } else { editor.mode };
            fs_ops::write_new_file(path, &editor.to_bytes(), mode)
        } else if self.editor_only && editor.path == path {
            // `pce` as `$EDITOR`: whoever handed over the file may read it
            // back through the descriptor it holds (crontab -e insists).
            fs_ops::overwrite_in_place(path, &editor.to_bytes()).map_err(|failed| {
                damaged = failed.touched;
                failed.error
            })
        } else {
            fs_ops::write_file_safely(path, &editor.to_bytes())
        };
        if new
            && written
                .as_ref()
                .is_err_and(|err| err.kind() == std::io::ErrorKind::AlreadyExists)
        {
            self.confirm_save_over(path.to_path_buf());
            return false;
        }
        match written {
            Ok(()) => {
                editor.path = path.to_path_buf();
                editor.untitled = false;
                editor.new_file = false;
                let meta = std::fs::metadata(path).ok();
                editor.disk_id = meta.as_ref().map(fs_ops::identity_of);
                editor.mode = meta.as_ref().and_then(fs_ops::permission_bits);
                editor.mark_saved();
                editor.message = Some("Saved".to_string());
                self.watch = EditedFileWatch::default();
                self.damaged_file = None;
                self.set_status(format!("Saved {}", path.display()));
                // The new size and date, in whichever pane shows the file —
                // on the same entry, even if sorting by size or date moved it.
                let sides: &[Side] = if self.editor_only {
                    &[]
                } else {
                    &[Side::Left, Side::Right]
                };
                for &side in sides {
                    let pane = self.pane_mut(side);
                    let result = pane.reload_keeping_selection();
                    if show
                        && path.parent() == Some(pane.cwd.as_path())
                        && let Some(name) = path.file_name()
                    {
                        pane.select_name(&name.to_string_lossy());
                    }
                    if let Err(err) = result {
                        self.set_error(format!("Cannot reload pane: {err}"));
                    }
                }
                true
            }
            Err(err) if damaged => {
                editor.message = Some("Not saved".to_string());
                self.damaged_file = Some(path.to_path_buf());
                self.set_error(format!(
                    "Cannot save {}: {err}. The file on disk may be damaged now: \
                     save again, or use Save As",
                    path.display()
                ));
                false
            }
            Err(err) => {
                editor.message = Some("Not saved".to_string());
                self.set_error(format!("Cannot save {}: {err}", path.display()));
                false
            }
        }
    }

    /// Esc in the editor: closes it, first asking about unsaved changes.
    fn close_editor(&mut self) {
        if self.editor.as_ref().is_some_and(Editor::is_modified) {
            self.dialog = Dialog::UnsavedChanges {
                quit_after: false,
                focused: 0,
            };
        } else {
            self.drop_editor();
        }
    }

    /// The editor goes, and the pane it took shows again what it showed
    /// before — the quick view of the selected entry, if that was open (its
    /// scroll resets by itself should the selection have moved meanwhile;
    /// see `sync_preview_scroll`).
    fn drop_editor(&mut self) {
        if self.editor_only {
            // Nothing behind it: closing the editor is the end.
            self.should_quit = true;
            return;
        }
        self.editor = None;
        self.list_focus = false;
        self.after_prompt = None;
        self.watch = EditedFileWatch::default();
        self.reset_menu();
        if let Some(view) = self.view_before_editor.take() {
            let entry_there = self.active_pane_ref().selected_entry().is_some();
            self.quick_view = view.quick_view && entry_there;
            self.preview_focus = view.preview_focus && self.quick_view;
            self.preview_scroll = if view.wrap_lines == self.wrap_lines {
                view.preview_scroll
            } else {
                self.rescroll_preview_for_wrap(view.wrap_lines, view.preview_scroll)
            };
            self.preview_scroll_x = if self.wrap_lines {
                0
            } else {
                view.preview_scroll_x
            };
        }
    }

    pub fn dialog_is_unsaved_changes(&self) -> bool {
        matches!(self.dialog, Dialog::UnsavedChanges { .. })
    }

    /// Left/Right (or Tab) across Save / Don't Save / Cancel.
    pub fn step_unsaved_focus(&mut self, forward: bool) {
        if let Dialog::UnsavedChanges { focused, .. } = &mut self.dialog {
            *focused = if forward {
                (*focused + 1) % 3
            } else {
                (*focused + 2) % 3
            };
        }
    }

    /// Enter in the unsaved-changes prompt. Save that fails keeps the editor
    /// open (the error says why), and doesn't quit either.
    pub fn activate_unsaved_button(&mut self) {
        let Dialog::UnsavedChanges {
            quit_after,
            focused,
        } = self.dialog
        else {
            return;
        };
        self.dialog = Dialog::None;
        let then = self.after_prompt.take().unwrap_or(if quit_after {
            AfterPrompt::Quit
        } else {
            AfterPrompt::Close
        });
        match focused {
            0 if !self.save_editor() => {
                // Save had to ask for a name (Untitled, or the file gone):
                // once saved under one, this goes on; Cancel there ends it.
                if matches!(
                    self.dialog,
                    Dialog::TextInput {
                        kind: TextInputKind::SaveAs,
                        ..
                    } | Dialog::ConfirmSaveAs { .. }
                ) {
                    self.after_prompt = Some(then);
                }
                return;
            }
            0 | 1 => {}
            _ => return,
        }
        self.carry_on(then);
    }

    /// What the unsaved-changes prompt was for, once its question is out of
    /// the way.
    fn carry_on(&mut self, then: AfterPrompt) {
        match then {
            AfterPrompt::Edit(editor) => self.install_editor(*editor),
            AfterPrompt::Untitled => self.install_editor(Editor::untitled()),
            AfterPrompt::QuickView => self.leave_for_quick_view(),
            AfterPrompt::Close => self.drop_editor(),
            AfterPrompt::Quit => {
                self.drop_editor();
                // The file was the question; a running copy is still worth
                // the usual warning.
                if self.job.is_some() {
                    self.dialog = Dialog::ConfirmQuit;
                    self.dialog_cancel_focused = false;
                } else {
                    self.quit();
                }
            }
        }
    }

    /// Save As (or its Replace) went ahead onto `target`: saved there, the
    /// unsaved-changes prompt it came from (if any) carries on; taken
    /// meanwhile, Replace is asked (and that carries on); failed, it all
    /// stops there, the editor as it was.
    fn finish_save_as(&mut self, target: &Path) {
        if self.write_editor_to(target, true) {
            if let Some(then) = self.after_prompt.take() {
                self.carry_on(then);
            }
        } else if !matches!(self.dialog, Dialog::ConfirmSaveAs { .. }) {
            self.after_prompt = None;
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
                self.run_delete(kind, items)?;
            }
            Dialog::Transfer(_) => self.confirm_transfer(),
            Dialog::Rename { field, src, .. } => {
                // Whatever the outcome, this question is answered.
                let follow = std::mem::take(&mut self.renaming_edited_file);
                let mut renamed = None;
                let was_on = [&self.left, &self.right]
                    .map(|pane| pane.selected_path().as_deref() == Some(src.as_path()));
                let new_name = field.text().to_string();
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
                                Ok(()) => {
                                    self.set_status(format!("Renamed to {new_name}"));
                                    renamed = Some((src.clone(), dest));
                                }
                                Err(err) => self.set_error(format!("Rename failed: {err}")),
                            }
                        }
                    }
                    None => self.set_error("Cannot rename: no parent directory".to_string()),
                }

                self.dialog = Dialog::None;
                // Not `?`: the editor may be open with unsaved changes.
                for side in [Side::Left, Side::Right] {
                    if let Err(err) = self.pane_mut(side).reload() {
                        self.set_error(format!("Cannot reload pane: {err}"));
                    }
                }
                if let Some((src, dest)) = renamed {
                    self.renaming_edited_file = follow;
                    self.after_rename(&src, &dest, was_on);
                }
            }
            Dialog::TextInput { kind, field, .. }
                if matches!(
                    kind,
                    TextInputKind::SelectFiles | TextInputKind::UnselectFiles
                ) =>
            {
                let select = *kind == TextInputKind::SelectFiles;
                let patterns = field.text().to_string();
                let changed = self.active_pane().mark_matching(&patterns, select);
                let verb = if select { "Selected" } else { "Unselected" };
                self.set_status(format!("{verb} {changed} file(s) matching {patterns}"));
                self.dialog = Dialog::None;
            }
            Dialog::TextInput {
                kind: TextInputKind::SaveAs,
                field,
                ..
            } => {
                let typed = field.text().to_string();
                self.dialog = Dialog::None;
                if typed.trim().is_empty() {
                    self.set_status("Name cannot be empty".to_string());
                    return Ok(());
                }
                let Some(target) = self.save_as_target(&typed) else {
                    self.after_prompt = None;
                    self.set_error(format!(
                        "Cannot save as \"{}\": no current directory to put it in; \
                         give the full path",
                        typed.trim()
                    ));
                    return Ok(());
                };
                // Its own file, still the same one: just a save. (Without
                // a known identity — not on Unix or Windows — the name
                // alone decides.)
                let own = self.editor.as_ref().is_some_and(|editor| {
                    !editor.untitled
                        && editor.path == target
                        && match (editor.disk_id, fs_ops::file_identity(&target)) {
                            (Some(was), Some(now)) => was.same_file(&now),
                            (None, now) => now.is_some(),
                            (Some(_), None) => false,
                        }
                });
                // A dangling symlink counts as taken: saving would quietly
                // create the file it points to, somewhere else.
                let taken = std::fs::symlink_metadata(&target).is_ok();
                if taken && !own {
                    self.confirm_save_over(target);
                } else {
                    self.finish_save_as(&target);
                }
            }
            Dialog::TextInput { kind, field, .. } => {
                let kind = *kind;
                let name = field.text().to_string();
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
                    TextInputKind::SelectFiles
                    | TextInputKind::UnselectFiles
                    | TextInputKind::SaveAs => {}
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
            Dialog::ConfirmSaveAs { target } => {
                let target = target.clone();
                self.dialog = Dialog::None;
                self.finish_save_as(&target);
            }
            Dialog::UnsavedChanges { .. } => self.activate_unsaved_button(),
            Dialog::Settings { .. } | Dialog::None => {}
        }
        Ok(())
    }

    /// Enter on Copy/Move or Overwrite. Anything wrong with the target keeps
    /// the dialog open under the error box, so the path can be corrected
    /// without starting over.
    fn confirm_transfer(&mut self) {
        let base = self.active_pane_ref().cwd.clone();
        let Dialog::Transfer(transfer) = &mut self.dialog else {
            return;
        };
        let kind = transfer.kind;
        if let Some(overwrite) = transfer.overwrite.take() {
            self.start_job(kind, overwrite.items, overwrite.dest_dir);
            return;
        }
        let (items, dest_dir) = match transfer.resolve(&base) {
            Ok(resolved) => resolved,
            Err(message) => {
                self.set_error(message);
                return;
            }
        };
        if items
            .iter()
            .any(|(name, src)| fs_ops::is_same_path(src, &dest_dir.join(name)))
        {
            // Copying within one directory only needs a new name, so go
            // straight to the field for it.
            transfer.editing = true;
            self.set_error(
                "Source and destination are the same: change the name or path".to_string(),
            );
            return;
        }
        let conflicts = existing_destinations(&items, &dest_dir);
        if conflicts.is_empty() {
            self.start_job(kind, items, dest_dir);
            return;
        }
        transfer.overwrite = Some(Overwrite {
            items,
            dest_dir,
            conflicts,
        });
        // Replacing files is destructive, so like Delete this defaults to
        // "no", on the buttons where that can be seen.
        transfer.editing = false;
        self.dialog_cancel_focused = true;
    }

    /// Copy/Move (and a Trash that has to copy) run on a worker thread with a
    /// progress window on top.
    fn start_job(&mut self, kind: DialogKind, items: Vec<(String, PathBuf)>, dest_dir: PathBuf) {
        self.dialog = Dialog::None;
        if self.job.is_some() {
            self.set_error("Another operation is still running (Ctrl+B shows it)".to_string());
            return;
        }
        let src_dir = self.active_pane_ref().cwd.clone();
        // A job already holds its own list, so the marks can go now.
        self.active_pane().clear_marks();
        self.job = Some(Job::start(kind, items, src_dir, dest_dir));
        self.job_visible = true;
        self.job_cancel_focused = false;
    }

    /// Runs a confirmed Delete/Trash: right here, or as a job for a Trash
    /// that has to copy.
    fn run_delete(&mut self, kind: DialogKind, items: Vec<(String, PathBuf)>) -> Result<()> {
        let src_dir = self.active_pane_ref().cwd.clone();
        if kind == DialogKind::TrashByCopy {
            let dest_dir = fs_ops::home_trash_dir().unwrap_or_else(|| src_dir.clone());
            self.start_job(kind, items, dest_dir);
            return Ok(());
        }
        self.dialog = Dialog::None;
        self.active_pane().clear_marks();

        let trashing = match kind {
            DialogKind::Trash | DialogKind::TrashByCopy => true,
            DialogKind::Delete => false,
            // Never reach the delete loop below: their sources must survive.
            DialogKind::Copy | DialogKind::Move => return Ok(()),
        };
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

        // Not `?`: the editor may be open beside the list, with changes.
        for side in [Side::Left, Side::Right] {
            if let Err(err) = self.pane_mut(side).reload() {
                self.set_error(format!("Cannot reload pane: {err}"));
            }
        }
        self.check_edited_file(true);
        Ok(())
    }

    pub fn cancel_dialog(&mut self) {
        self.dialog = Dialog::None;
        self.after_prompt = None;
        self.renaming_edited_file = false;
    }

    /// The field of whichever dialog has one, and whether the focus is on it.
    fn dialog_field(&mut self) -> Option<(&mut TextField, &mut bool)> {
        match &mut self.dialog {
            Dialog::Transfer(transfer) => Some((&mut transfer.field, &mut transfer.editing)),
            Dialog::TextInput { field, editing, .. } | Dialog::Rename { field, editing, .. } => {
                Some((field, editing))
            }
            _ => None,
        }
    }

    pub fn dialog_has_field(&self) -> bool {
        matches!(
            self.dialog,
            Dialog::Transfer(_) | Dialog::TextInput { .. } | Dialog::Rename { .. }
        )
    }

    fn dialog_field_focused(&mut self) -> bool {
        self.dialog_field().is_some_and(|(_, editing)| *editing)
    }

    /// Enter acts on the highlighted button. From the field, where none is
    /// highlighted, it goes ahead with the dialog's action — except on an
    /// overwrite prompt whose target wasn't changed: replacing files takes an
    /// explicit Overwrite on the buttons.
    pub fn activate_dialog_button(&mut self) -> Result<()> {
        if self.dialog_field_focused() {
            if matches!(&self.dialog, Dialog::Transfer(t) if t.overwrite.is_some()) {
                return Ok(());
            }
            return self.confirm_dialog();
        }
        if self.dialog_cancel_focused {
            self.cancel_dialog();
            return Ok(());
        }
        self.confirm_dialog()
    }

    /// The field takes over Left/Right. Which button was chosen is kept, so
    /// Down comes back to it.
    pub fn focus_dialog_field(&mut self) {
        if let Some((_, editing)) = self.dialog_field() {
            *editing = true;
        }
    }

    pub fn focus_dialog_buttons(&mut self) {
        if let Some((_, editing)) = self.dialog_field() {
            *editing = false;
        }
    }

    pub fn dialog_field_left(&mut self) {
        if self.dialog_field_focused() {
            if let Some((field, _)) = self.dialog_field() {
                field.move_left();
            }
        } else {
            self.toggle_dialog_focus();
        }
    }

    pub fn dialog_field_right(&mut self) {
        if self.dialog_field_focused() {
            if let Some((field, _)) = self.dialog_field() {
                field.move_right();
            }
        } else {
            self.toggle_dialog_focus();
        }
    }

    pub fn dialog_field_home(&mut self) {
        if let Some((field, editing)) = self.dialog_field() {
            *editing = true;
            field.move_home();
        }
    }

    pub fn dialog_field_end(&mut self) {
        if let Some((field, editing)) = self.dialog_field() {
            *editing = true;
            field.move_end();
        }
    }

    pub fn dialog_field_insert(&mut self, c: char) {
        if let Some((field, _)) = self.dialog_field() {
            field.insert(c);
            self.after_field_edit();
        }
    }

    pub fn dialog_field_backspace(&mut self) {
        if let Some((field, _)) = self.dialog_field()
            && field.backspace()
        {
            self.after_field_edit();
        }
    }

    pub fn dialog_field_delete(&mut self) {
        if let Some((field, _)) = self.dialog_field()
            && field.delete()
        {
            self.after_field_edit();
        }
    }

    /// A changed Copy/Move target is a new question: whatever the overwrite
    /// prompt was asking about no longer applies, and Enter checks the new
    /// path. Cancel goes with the prompt that picked it; a button the user
    /// chose stays.
    fn after_field_edit(&mut self) {
        self.focus_dialog_field();
        if let Dialog::Transfer(transfer) = &mut self.dialog
            && transfer.overwrite.take().is_some()
        {
            self.dialog_cancel_focused = false;
        }
    }

    pub fn request_mkdir(&mut self) {
        self.dialog = Dialog::TextInput {
            kind: TextInputKind::MkDir,
            field: TextField::new(String::new()),
            editing: true,
        };
        self.dialog_cancel_focused = false;
    }

    /// File > Select/Unselect Files: asks for wildcard patterns, starting
    /// from `*` (everything), as in Norton Commander.
    fn request_select_files(&mut self, kind: TextInputKind) {
        self.dialog = Dialog::TextInput {
            kind,
            field: TextField::new("*".to_string()),
            editing: true,
        };
        self.dialog_cancel_focused = false;
    }

    pub fn request_new_file(&mut self) {
        self.dialog = Dialog::TextInput {
            kind: TextInputKind::NewFile,
            field: TextField::new(String::new()),
            editing: true,
        };
        self.dialog_cancel_focused = false;
    }

    /// Left/Right in a dialog: swap which button — the primary action or
    /// Cancel — Enter would activate. There are only ever two, so this is a
    /// plain flip rather than tracking an index.
    pub fn toggle_dialog_focus(&mut self) {
        self.dialog_cancel_focused = !self.dialog_cancel_focused;
    }

    pub fn command_line_push(&mut self, c: char) {
        self.command_line.push(c);
    }

    pub fn command_line_backspace(&mut self) {
        self.command_line.pop();
    }

    /// Esc with the panes' keyboard, one thing at a time: a find match
    /// highlighted in the quick view goes first, then what's typed on the
    /// command line, then the quick view itself.
    pub fn escape(&mut self) {
        if self.clear_preview_match() {
            return;
        }
        if !self.command_line.is_empty() {
            self.command_line_clear();
            return;
        }
        // The file list's keyboard beside the editor: Esc closes it, as it
        // does from the editor itself.
        if self.editor.is_some() {
            self.close_editor();
            return;
        }
        if self.quick_view {
            self.quick_view = false;
            self.preview_focus = false;
            self.preview_find = None;
        }
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
    has_extension(name, MEDIA_EXTENSIONS)
}

/// Whether `name`'s extension, in any case, is one of `extensions`.
pub(crate) fn has_extension(name: &str, extensions: &[&str]) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| extensions.contains(&ext.to_lowercase().as_str()))
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
    if arg.is_empty() {
        return dirs::home_dir();
    }
    transfer::expand_home(arg)
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

    /// An app with the left pane active on `left` and the other on `right`.
    fn app_with_panes(left: &Path, right: &Path) -> App {
        let mut app = App::new().unwrap();
        app.left = Pane::new(left.to_path_buf(), false).unwrap();
        app.right = Pane::new(right.to_path_buf(), false).unwrap();
        app.active = Side::Left;
        app
    }

    fn select(app: &mut App, name: &str) {
        let index = app
            .left
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .unwrap();
        app.left.selected = index;
    }

    fn wait_for_job(app: &mut App) {
        while app.job.is_some() {
            app.poll_job().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.dialog_field_insert(c);
        }
    }

    fn overwrite_conflicts(app: &App) -> Option<Vec<String>> {
        match &app.dialog {
            Dialog::Transfer(transfer) => transfer.overwrite.as_ref().map(|o| o.conflicts.clone()),
            _ => None,
        }
    }

    #[test]
    fn copy_within_one_directory_works_once_the_name_is_changed() {
        let dir = std::env::temp_dir().join("pc_test_transfer_same_dir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let mut app = app_with_panes(&dir, &dir);
        select(&mut app, "a.txt");

        app.request_copy();
        app.confirm_dialog().unwrap();
        assert!(app.error_dialog.take().is_some());
        assert!(app.dialog_has_field() && app.job.is_none());

        // "a.txt" -> "a2.txt": into the name, before the extension.
        for _ in 0..4 {
            app.dialog_field_left();
        }
        type_text(&mut app, "2");
        app.confirm_dialog().unwrap();
        wait_for_job(&mut app);

        assert_eq!(std::fs::read(dir.join("a2.txt")).unwrap(), b"x");
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn overwrite_prompt_can_be_escaped_by_editing_the_name() {
        let dir = std::env::temp_dir().join("pc_test_transfer_conflict");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::fs::write(dir.join("src/a"), "new").unwrap();
        std::fs::write(dir.join("dest/a"), "old").unwrap();
        let mut app = app_with_panes(&dir.join("src"), &dir.join("dest"));
        select(&mut app, "a");

        app.request_copy();
        app.confirm_dialog().unwrap();
        assert_eq!(overwrite_conflicts(&app), Some(vec!["a".to_string()]));
        assert!(app.dialog_cancel_focused);

        type_text(&mut app, "b");
        assert_eq!(overwrite_conflicts(&app), None);
        assert!(!app.dialog_cancel_focused);
        app.confirm_dialog().unwrap();
        wait_for_job(&mut app);

        assert_eq!(std::fs::read(dir.join("dest/ab")).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("dest/a")).unwrap(), b"old");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_name_that_collides_asks_and_overwrite_replaces_it() {
        let dir = std::env::temp_dir().join("pc_test_transfer_new_conflict");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::fs::write(dir.join("src/a"), "new").unwrap();
        std::fs::write(dir.join("dest/ab"), "old").unwrap();
        let mut app = app_with_panes(&dir.join("src"), &dir.join("dest"));
        select(&mut app, "a");

        app.request_copy();
        type_text(&mut app, "b");
        app.confirm_dialog().unwrap();
        assert_eq!(overwrite_conflicts(&app), Some(vec!["ab".to_string()]));
        assert_eq!(std::fs::read(dir.join("dest/ab")).unwrap(), b"old");

        app.confirm_dialog().unwrap();
        wait_for_job(&mut app);
        assert_eq!(std::fs::read(dir.join("dest/ab")).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn enter_in_the_field_copies_but_never_overwrites_unasked() {
        let dir = std::env::temp_dir().join("pc_test_transfer_enter");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::fs::write(dir.join("src/a"), "new").unwrap();
        std::fs::write(dir.join("dest/a"), "old").unwrap();
        std::fs::write(dir.join("dest/ab"), "old").unwrap();
        let mut app = app_with_panes(&dir.join("src"), &dir.join("dest"));
        select(&mut app, "a");

        // Enter from the field on a target that's taken: asks, on the buttons.
        app.request_copy();
        app.focus_dialog_field();
        app.activate_dialog_button().unwrap();
        assert_eq!(overwrite_conflicts(&app), Some(vec!["a".to_string()]));
        assert!(app.dialog_cancel_focused);

        // Back in the field with the target unchanged: Enter does nothing.
        app.focus_dialog_field();
        app.activate_dialog_button().unwrap();
        assert!(overwrite_conflicts(&app).is_some());
        assert_eq!(std::fs::read(dir.join("dest/a")).unwrap(), b"old");

        // Another taken name: asks again rather than replacing it.
        type_text(&mut app, "b");
        app.activate_dialog_button().unwrap();
        assert_eq!(overwrite_conflicts(&app), Some(vec!["ab".to_string()]));
        assert_eq!(std::fs::read(dir.join("dest/ab")).unwrap(), b"old");

        // A free name: Enter copies straight from the field.
        type_text(&mut app, "c");
        app.activate_dialog_button().unwrap();
        wait_for_job(&mut app);
        assert_eq!(std::fs::read(dir.join("dest/abc")).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn down_returns_to_the_button_that_was_chosen() {
        let dir = std::env::temp_dir().join("pc_test_transfer_focus");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "x").unwrap();
        let mut app = app_with_panes(&dir, &dir);
        select(&mut app, "a");

        // Copy starts on Copy; choose Cancel, go up and back down.
        app.request_copy();
        assert!(!app.dialog_cancel_focused);
        app.dialog_field_right();
        assert!(app.dialog_cancel_focused);
        app.focus_dialog_field();
        app.dialog_field_left();
        app.focus_dialog_buttons();
        assert!(
            app.dialog_cancel_focused,
            "Left in the field moved the cursor"
        );

        // Typing in between doesn't change the choice either.
        app.focus_dialog_field();
        app.dialog_field_insert('b');
        app.focus_dialog_buttons();
        assert!(app.dialog_cancel_focused);

        // And the buttons switch again once back on them.
        app.dialog_field_left();
        assert!(!app.dialog_cancel_focused);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_starts_in_the_field_and_edits_anywhere_in_the_name() {
        let dir = std::env::temp_dir().join("pc_test_rename_cursor");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let mut app = app_with_panes(&dir, &dir);
        select(&mut app, "a.txt");

        app.request_rename();
        for _ in 0..4 {
            app.dialog_field_left();
        }
        assert!(!app.dialog_cancel_focused, "Left moved the cursor");
        type_text(&mut app, "2");
        app.activate_dialog_button().unwrap();

        assert!(dir.join("a2.txt").exists() && !dir.join("a.txt").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rename_buttons_are_reached_with_down() {
        let dir = std::env::temp_dir().join("pc_test_rename_buttons");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "x").unwrap();
        let mut app = app_with_panes(&dir, &dir);
        select(&mut app, "a");

        app.request_rename();
        type_text(&mut app, "b");
        app.focus_dialog_buttons();
        app.dialog_field_right();
        assert!(app.dialog_cancel_focused);
        app.activate_dialog_button().unwrap();

        assert!(!app.dialog_has_field());
        assert!(dir.join("a").exists() && !dir.join("ab").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn new_directory_edits_mid_name_too() {
        let dir = std::env::temp_dir().join("pc_test_mkdir_cursor");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = app_with_panes(&dir, &dir);

        app.request_mkdir();
        type_text(&mut app, "ac");
        app.dialog_field_left();
        type_text(&mut app, "b");
        app.activate_dialog_button().unwrap();

        assert!(dir.join("abc").is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An app on a fresh temp directory holding `files`, the first one
    /// selected, with the built-in editor on.
    fn app_with_files(name: &str, files: &[(&str, &[u8])]) -> (App, PathBuf) {
        let dir = std::env::temp_dir().join(format!("pc_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for (file, contents) in files {
            std::fs::write(dir.join(file), contents).unwrap();
        }
        let mut app = app_with_panes(&dir, &dir);
        app.builtin_editor = true;
        app.wrap_lines = false;
        if let Some((first, _)) = files.first() {
            select(&mut app, first);
        }
        (app, dir)
    }

    fn keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.editor_key(KeyCode::Char(c), KeyModifiers::NONE)
                .unwrap();
        }
    }

    #[test]
    fn copy_and_move_wait_while_the_quick_view_covers_the_other_pane() {
        let (mut app, dir) = app_with_files("quick_view_copy", &[("a.txt", b"hello\n")]);
        assert!(app.action_enabled(Action::Copy));
        app.run_action(Action::View).unwrap();
        assert!(app.quick_view);
        for action in [Action::Copy, Action::Move] {
            assert!(!app.action_enabled(action), "{action:?} greyed out");
            app.run_action(action).unwrap();
            assert!(
                matches!(app.dialog, Dialog::None),
                "{action:?} asks nothing"
            );
            let note = app.error_dialog.take().unwrap();
            assert!(note.contains("quick view"), "{note}");
        }
        // Other things on the list still work beside the quick view.
        assert!(app.action_enabled(Action::MkDir));
        // With the quick view closed, Copy is back.
        app.run_action(Action::View).unwrap();
        assert!(!app.quick_view);
        assert!(app.action_enabled(Action::Copy));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f4_edits_in_the_other_pane_and_esc_asks_before_dropping_changes() {
        let (mut app, dir) = app_with_files("editor_close", &[("a.txt", b"hello\n")]);
        app.quick_view = true;
        app.run_action(Action::Edit).unwrap();
        assert!(app.editor.is_some());
        assert!(!app.quick_view, "the editor replaces the quick view");

        app.editor_key(KeyCode::F(4), KeyModifiers::NONE).unwrap();
        assert!(app.editor.as_ref().unwrap().full_screen);
        app.editor_key(KeyCode::F(4), KeyModifiers::NONE).unwrap();
        assert!(!app.editor.as_ref().unwrap().full_screen);

        keys(&mut app, "ą");
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(matches!(
            app.dialog,
            Dialog::UnsavedChanges {
                quit_after: false,
                focused: 0
            }
        ));
        // Cancel keeps editing; Don't Save closes without touching the file.
        app.step_unsaved_focus(false);
        app.activate_unsaved_button();
        assert!(app.editor.is_some());
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        app.step_unsaved_focus(true);
        app.activate_unsaved_button();
        assert!(app.editor.is_none());
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"hello\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ctrl_s_saves_and_the_pane_shows_the_new_size() {
        let (mut app, dir) = app_with_files("editor_save", &[("a.txt", b"one\r\ntwo\r\n")]);
        app.run_action(Action::Edit).unwrap();
        app.editor_key(KeyCode::End, KeyModifiers::NONE).unwrap();
        keys(&mut app, " więcej");
        app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        let saved = "one więcej\r\ntwo\r\n";
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), saved);
        let editor = app.editor.as_ref().unwrap();
        assert!(!editor.is_modified());
        let entry = &app.left.entries[app.left.selected];
        assert_eq!(entry.size, saved.len() as u64);
        // Nothing left to ask about.
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.editor.is_none());
        assert!(matches!(app.dialog, Dialog::None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quitting_with_unsaved_changes_offers_to_save_first() {
        let (mut app, dir) = app_with_files("editor_quit", &[("a.txt", b"x")]);
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "y");
        app.editor_key(KeyCode::Char('q'), KeyModifiers::CONTROL)
            .unwrap();
        assert!(matches!(
            app.dialog,
            Dialog::UnsavedChanges {
                quit_after: true,
                ..
            }
        ));
        app.activate_unsaved_button();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"yx");
        assert!(app.should_quit);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_save_keeps_the_editor_open_and_modified() {
        let (mut app, dir) = app_with_files("editor_save_fails", &[("a.txt", b"x")]);
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "y");
        // The file's place is taken by a directory: neither writing in place
        // nor renaming over it can work.
        std::fs::remove_file(dir.join("a.txt")).unwrap();
        std::fs::create_dir(dir.join("a.txt")).unwrap();
        app.run_action(Action::Quit).unwrap();
        app.activate_unsaved_button();
        assert!(!app.should_quit);
        // Not the same file there any more: Save asks where to save.
        assert!(matches!(
            app.dialog,
            Dialog::TextInput {
                kind: TextInputKind::SaveAs,
                ..
            }
        ));
        assert!(app.editor.as_ref().unwrap().is_modified());
        // Saving onto that name anyway: there's a directory in the way.
        app.confirm_dialog().unwrap();
        assert!(matches!(app.dialog, Dialog::ConfirmSaveAs { .. }));
        app.confirm_dialog().unwrap();
        assert!(app.error_dialog.is_some());
        assert!(app.editor.as_ref().unwrap().is_modified());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_the_editor_cannot_round_trip_are_not_opened() {
        let (mut app, dir) = app_with_files(
            "editor_refuse",
            &[("bin.dat", b"\x7fELF\0\0"), ("latin2.txt", b"\xb1\xea")],
        );
        app.run_action(Action::Edit).unwrap();
        assert!(app.editor.is_none());
        assert!(app.error_dialog.take().unwrap().contains("External editor"));
        select(&mut app, "latin2.txt");
        app.run_action(Action::Edit).unwrap();
        assert!(app.editor.is_none());
        assert!(app.error_dialog.take().unwrap().contains("UTF-8"));
        select(&mut app, "sub");
        app.run_action(Action::Edit).unwrap();
        assert!(app.editor.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn external_editor_setting_keeps_f4_on_editor_env() {
        let (mut app, dir) = app_with_files("editor_external", &[("a.txt", b"x")]);
        app.builtin_editor = false;
        app.run_action(Action::Edit).unwrap();
        assert!(app.editor.is_none());
        assert!(matches!(
            app.external_request,
            Some(ExternalRequest::Edit(_))
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn menu_opens_over_the_editor_but_altgr_letters_are_typed() {
        let (mut app, dir) = app_with_files("editor_menu", &[("a.txt", b"")]);
        app.run_action(Action::Edit).unwrap();
        assert_eq!(app.menu_bar()[1].title, "Edit");
        app.editor_key(KeyCode::Char('e'), KeyModifiers::ALT)
            .unwrap();
        assert!(app.menu_open);
        assert_eq!(app.menu_category, 1);
        app.close_menu();
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ] {
            app.editor_key(KeyCode::Char('ł'), modifiers).unwrap();
        }
        assert!(!app.menu_open);
        assert_eq!(app.editor.as_ref().unwrap().lines(), ["łłł"]);
        app.editor_key(KeyCode::F(9), KeyModifiers::NONE).unwrap();
        assert!(app.menu_open);
        // Pane actions are grayed out behind the editor; its own aren't.
        assert!(!app.action_enabled(Action::Copy));
        assert!(app.action_enabled(Action::Save));
        app.close_menu();
        app.editor = None;
        assert_eq!(app.menu_bar()[1].title, "Options");
        assert!(!app.action_enabled(Action::Save));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Gives this test thread a settings directory of its own (empty), in
    /// place of none at all; see `state::test_dir`.
    fn own_settings_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pc_test_cfg_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        state::test_dir::set(Some(dir.clone()));
        dir
    }

    #[test]
    fn wrap_lines_menu_item_switches_wrapping_and_is_remembered() {
        let config = own_settings_dir("wrap");
        let (mut app, dir) = app_with_files("editor_wrap", &[("a.txt", b"")]);
        app.run_action(Action::Edit).unwrap();
        assert_eq!(app.menu_shortcut(Action::WrapLines), Some("[ ]"));
        app.run_action(Action::WrapLines).unwrap();
        assert_eq!(app.menu_shortcut(Action::WrapLines), Some("[x]"));
        let saved = std::fs::read_to_string(config.join("settings")).unwrap();
        assert!(saved.contains("wrap_lines=true"), "{saved}");
        // The next start, and the next file opened, wrap too.
        let mut again = App::new().unwrap();
        assert!(again.wrap_lines);
        again.left = Pane::new(dir.clone(), false).unwrap();
        select(&mut again, "a.txt");
        again.run_action(Action::Edit).unwrap();
        assert_eq!(again.menu_shortcut(Action::WrapLines), Some("[x]"));
        state::test_dir::set(None);
        std::fs::remove_dir_all(&config).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn editor_settings_default_and_persist() {
        let config = own_settings_dir("editor_settings");
        let mut app = App::new().unwrap();
        assert!(app.builtin_editor, "the built-in editor is the default");
        assert!(!app.editor_focus_mode, "the regular view is the default");
        assert!(!app.wrap_lines, "no wrapping by default, as in Notepad");
        app.set_setting_value(SettingItem::BuiltinEditor, false);
        app.set_setting_value(SettingItem::EditorFocusMode, true);
        app.persist_settings();
        let saved = std::fs::read_to_string(config.join("settings")).unwrap();
        for line in [
            "builtin_editor=false",
            "editor_focus_mode=true",
            "wrap_lines=false",
        ] {
            assert!(saved.lines().any(|l| l == line), "{line} in {saved}");
        }
        let app = App::new().unwrap();
        assert!(!app.builtin_editor);
        assert!(app.editor_focus_mode);
        state::test_dir::set(None);
        std::fs::remove_dir_all(&config).unwrap();
    }

    #[test]
    fn with_the_find_bar_open_only_its_keys_and_a_few_commands_work() {
        let (mut app, dir) = app_with_files("editor_find_keys", &[("a.txt", b"kot pies\n")]);
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "x");
        app.editor_key(KeyCode::Char('f'), KeyModifiers::CONTROL)
            .unwrap();
        keys(&mut app, "pies");
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.find.as_ref().unwrap().field.text(), "pies");
        assert_eq!(editor.lines(), ["xkot pies"], "letters go to the bar");
        let selection = editor.selection();
        // Undo and Select All belong to the text, not to the bar.
        for letter in ['z', 'a', 'y', 'x', 'v'] {
            app.editor_key(KeyCode::Char(letter), KeyModifiers::CONTROL)
                .unwrap();
        }
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.lines(), ["xkot pies"]);
        assert_eq!(editor.selection(), selection);
        assert!(editor.find.is_some());
        // Saving does work.
        app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"xkot pies\n");
        assert!(!app.editor.as_ref().unwrap().is_modified());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copy_cut_and_paste_go_through_the_apps_clipboard() {
        let (mut app, dir) = app_with_files("editor_clipboard", &[("a.txt", b"ala ma")]);
        app.run_action(Action::Edit).unwrap();
        let ctrl = |app: &mut App, letter| {
            app.editor_key(KeyCode::Char(letter), KeyModifiers::CONTROL)
                .unwrap();
        };
        ctrl(&mut app, 'a');
        ctrl(&mut app, 'c');
        app.editor_key(KeyCode::End, KeyModifiers::NONE).unwrap();
        keys(&mut app, " ");
        ctrl(&mut app, 'v');
        assert_eq!(app.editor.as_ref().unwrap().lines(), ["ala ma ala ma"]);
        // Cut with nothing selected keeps the clipboard as it was.
        ctrl(&mut app, 'x');
        assert_eq!(app.clipboard.paste(), "ala ma");
        app.editor_key(KeyCode::Home, KeyModifiers::SHIFT).unwrap();
        ctrl(&mut app, 'x');
        assert_eq!(app.editor.as_ref().unwrap().lines(), [""]);
        assert_eq!(app.clipboard.paste(), "ala ma ala ma");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_the_editor_brings_back_the_quick_view_it_replaced() {
        let (mut app, dir) = app_with_files("editor_back_to_view", &[("a.txt", b"x\n")]);
        app.run_action(Action::View).unwrap();
        app.preview_focus = true;
        app.preview_scroll = 3;
        app.run_action(Action::Edit).unwrap();
        assert!(!app.quick_view && !app.preview_focus);
        // Closed full screen, Esc goes back to the view all the same.
        app.editor_key(KeyCode::F(4), KeyModifiers::NONE).unwrap();
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.editor.is_none());
        assert!(app.quick_view && app.preview_focus);
        assert_eq!(app.preview_scroll, 3);

        // Through the unsaved-changes prompt, with either answer.
        for (button, saved) in [(1, "x\n"), (0, "yx\n")] {
            app.run_action(Action::Edit).unwrap();
            keys(&mut app, "y");
            app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
            for _ in 0..button {
                app.step_unsaved_focus(true);
            }
            app.activate_unsaved_button();
            assert!(app.editor.is_none());
            assert!(app.quick_view && app.preview_focus, "button {button}");
            assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), saved);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn closing_the_editor_opened_from_the_listing_goes_back_to_it() {
        let (mut app, dir) = app_with_files("editor_back_to_list", &[("a.txt", b"x")]);
        app.run_action(Action::Edit).unwrap();
        app.editor_key(KeyCode::F(4), KeyModifiers::NONE).unwrap();
        keys(&mut app, "y");
        app.run_action(Action::Close).unwrap();
        assert!(matches!(app.dialog, Dialog::UnsavedChanges { .. }));
        app.activate_unsaved_button();
        assert!(app.editor.is_none());
        assert!(!app.quick_view && !app.preview_focus);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// F2 in the editor, the field cleared and `name` typed in its place.
    fn rename_edited_file_to(app: &mut App, name: &str) {
        app.editor_key(KeyCode::F(2), KeyModifiers::NONE).unwrap();
        assert!(matches!(app.dialog, Dialog::Rename { .. }));
        while matches!(&app.dialog, Dialog::Rename { field, .. } if !field.text().is_empty()) {
            app.dialog_field_backspace();
        }
        type_text(app, name);
        app.confirm_dialog().unwrap();
    }

    #[test]
    fn f2_renames_the_edited_file_keeping_its_unsaved_changes() {
        for full_screen in [false, true] {
            let (mut app, dir) =
                app_with_files("editor_rename", &[("a.txt", b"one\n"), ("b.txt", b"x")]);
            app.run_action(Action::Edit).unwrap();
            app.editor.as_mut().unwrap().full_screen = full_screen;
            app.editor_key(KeyCode::End, KeyModifiers::NONE).unwrap();
            keys(&mut app, " two");
            app.editor_key(KeyCode::F(2), KeyModifiers::NONE).unwrap();
            match &app.dialog {
                Dialog::Rename { field, .. } => assert_eq!(field.text(), "a.txt"),
                _ => panic!("no Rename dialog"),
            }
            app.cancel_dialog();

            rename_edited_file_to(&mut app, "c.txt");
            assert!(app.error_dialog.is_none(), "{:?}", app.error_dialog);
            assert!(!dir.join("a.txt").exists());
            // Renamed, not saved: the file still has the old text.
            assert_eq!(std::fs::read(dir.join("c.txt")).unwrap(), b"one\n");
            let editor = app.editor.as_ref().unwrap();
            assert_eq!(editor.path, dir.join("c.txt"));
            assert_eq!(editor.lines(), ["one two"]);
            assert!(editor.is_modified());
            assert_eq!(editor.cursor(), crate::editor::Pos::new(0, 7));
            assert_eq!(editor.full_screen, full_screen);
            let left = &app.left;
            assert_eq!(left.entries[left.selected].name, "c.txt");
            // Undo still knows the edits; saving writes the new name.
            app.editor_key(KeyCode::Char('z'), KeyModifiers::CONTROL)
                .unwrap();
            assert_eq!(app.editor.as_ref().unwrap().lines(), ["one "]);
            app.editor_key(KeyCode::Char('y'), KeyModifiers::CONTROL)
                .unwrap();
            app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
                .unwrap();
            assert_eq!(std::fs::read(dir.join("c.txt")).unwrap(), b"one two\n");
            assert!(!dir.join("a.txt").exists());
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn renaming_the_edited_file_onto_another_is_refused() {
        let (mut app, dir) =
            app_with_files("editor_rename_taken", &[("a.txt", b"a"), ("b.txt", b"b")]);
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "x");
        rename_edited_file_to(&mut app, "b.txt");
        assert!(app.error_dialog.take().unwrap().contains("already exists"));
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.path, dir.join("a.txt"));
        assert!(editor.is_modified());
        assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"b");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn after_a_rename_closing_returns_to_the_quick_view_of_the_new_name() {
        let (mut app, dir) = app_with_files("editor_rename_view", &[("a.txt", b"x")]);
        app.run_action(Action::View).unwrap();
        app.run_action(Action::Edit).unwrap();
        rename_edited_file_to(&mut app, "renamed.txt");
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.editor.is_none());
        assert!(app.quick_view);
        assert_eq!(app.left.preview_target(), Some(dir.join("renamed.txt")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_f_key_bar_while_editing_has_what_the_editor_does() {
        let (mut app, dir) = app_with_files("editor_fn_keys", &[("a.txt", b"kot kot")]);
        app.run_action(Action::Edit).unwrap();
        let state = |app: &App, key: &str| {
            let fn_key = menu::FN_KEYS.iter().find(|k| k.key == key).unwrap();
            app.editor_fn_key(fn_key)
        };
        for key in ["F5", "F6", "F7", "F8"] {
            assert!(!state(&app, key).1, "{key} is dimmed");
        }
        for key in ["F1", "F2", "F9", "F10"] {
            assert!(state(&app, key).1, "{key} is active");
        }
        assert_eq!(state(&app, "F4"), ("Full", true));
        // Next only once there's something to look for.
        assert_eq!(state(&app, "F3"), ("Next", false));
        app.editor_key(KeyCode::Char('f'), KeyModifiers::CONTROL)
            .unwrap();
        assert_eq!(state(&app, "F3"), ("Next", true));
        keys(&mut app, "kot");
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert_eq!(state(&app, "F3"), ("Next", true));

        // Clicked: a dimmed key does nothing, the others what they say.
        app.click_editor_fn_key(Some(Action::Copy)).unwrap();
        assert!(app.job.is_none() && matches!(app.dialog, Dialog::None));
        app.click_editor_fn_key(Some(Action::View)).unwrap();
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.selected_text().as_deref(), Some("kot"));
        app.click_editor_fn_key(Some(Action::Edit)).unwrap();
        assert!(app.editor.as_ref().unwrap().full_screen);
        app.click_editor_fn_key(Some(Action::Rename)).unwrap();
        assert!(matches!(app.dialog, Dialog::Rename { .. }));

        // File > Rename renames the edited file too, not the pane's entry.
        app.dialog = Dialog::None;
        assert!(app.action_enabled(Action::Rename));
        app.run_action(Action::Rename).unwrap();
        let edited = app.editor.as_ref().unwrap().path.clone();
        assert!(matches!(&app.dialog, Dialog::Rename { src, .. } if *src == edited));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Gives `path` (a file or a directory) a modification time of its own,
    /// `seconds` from now: a change the panes notice, with no waiting.
    fn touch(path: &Path, seconds: u64) {
        let at = std::time::SystemTime::now() + Duration::from_secs(seconds);
        std::fs::File::open(path).unwrap().set_modified(at).unwrap();
    }

    #[test]
    fn a_changed_directory_is_listed_again_keeping_cursor_and_marks() {
        let (mut app, dir) = app_with_files(
            "refresh_changed",
            &[("b.txt", b"b"), ("c.txt", b"c"), ("d.txt", b"d")],
        );
        select(&mut app, "c.txt");
        app.left.marked.insert("d.txt".to_string());
        app.left.marked.insert("b.txt".to_string());
        // Unchanged: nothing is read (a stand-in entry would vanish).
        app.left.entries.push(app.left.entries[1].clone());
        let listed = app.left.entries.len();
        app.refresh_from_disk();
        assert_eq!(app.left.entries.len(), listed, "no change, no reload");
        app.left.entries.pop();

        std::fs::write(dir.join("a.txt"), "new").unwrap();
        std::fs::remove_file(dir.join("b.txt")).unwrap();
        touch(&dir, 10);
        app.refresh_from_disk();
        let names: Vec<&str> = app.left.entries.iter().map(|e| e.name.as_str()).collect();
        assert!(
            names.contains(&"a.txt") && !names.contains(&"b.txt"),
            "{names:?}"
        );
        assert_eq!(app.left.entries[app.left.selected].name, "c.txt");
        assert_eq!(
            app.left.marked,
            std::collections::HashSet::from(["d.txt".to_string()])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn listings_follow_the_disk_under_dialogs_and_the_menu() {
        let (mut app, dir) = app_with_files("refresh_dialog", &[("a.txt", b"a")]);
        app.request_mkdir();
        std::fs::write(dir.join("b.txt"), "b").unwrap();
        touch(&dir, 10);
        app.refresh_from_disk();
        assert!(app.left.entries.iter().any(|e| e.name == "b.txt"));
        assert!(matches!(app.dialog, Dialog::TextInput { .. }), "still open");
        app.cancel_dialog();

        app.open_menu_at(2);
        std::fs::write(dir.join("c.txt"), "c").unwrap();
        touch(&dir, 20);
        app.refresh_from_disk();
        assert!(app.left.entries.iter().any(|e| e.name == "c.txt"));
        assert!(app.menu_open && app.menu_category == 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quick_search_goes_on_from_its_match_after_a_reload() {
        let (mut app, dir) = app_with_files(
            "refresh_quick_search",
            &[("kot.txt", b"k"), ("pies.txt", b"p"), ("zebra.txt", b"z")],
        );
        app.run_action(Action::QuickSearch).unwrap();
        app.quick_search_push('p');
        assert_eq!(app.left.entries[app.left.selected].name, "pies.txt");
        // A file listed before it shifts every row.
        std::fs::write(dir.join("aaa.txt"), "a").unwrap();
        touch(&dir, 10);
        app.refresh_from_disk();
        assert_eq!(app.left.entries[app.left.selected].name, "pies.txt");
        assert_eq!(app.quick_search.as_deref(), Some("p"));
        app.quick_search_push('i');
        assert_eq!(app.left.entries[app.left.selected].name, "pies.txt");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_vanished_directory_under_a_dialog_is_left_with_the_note_held_back() {
        let (mut app, dir) = app_with_files("refresh_gone_dialog", &[("a.txt", b"a")]);
        app.left.change_dir(&dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/x.txt"), "x").unwrap();
        app.left.reload().unwrap();
        select(&mut app, "x.txt");
        app.run_action(Action::Delete).unwrap();
        assert!(matches!(app.dialog, Dialog::Confirm { .. }));
        std::fs::remove_dir_all(dir.join("sub")).unwrap();
        app.refresh_from_disk();
        assert_eq!(app.left.cwd, dir, "moved up at once");
        assert!(app.error_dialog.is_none(), "but no popup over the dialog");
        app.cancel_dialog();
        app.refresh_from_disk();
        assert!(app.error_dialog.take().unwrap().contains("is gone"));

        // MkDir confirmed in a directory moved away under it would land a
        // level up: the pane waits for it to close.
        std::fs::create_dir(dir.join("sub")).unwrap();
        app.left.change_dir(&dir.join("sub")).unwrap();
        app.request_mkdir();
        std::fs::remove_dir_all(dir.join("sub")).unwrap();
        app.refresh_from_disk();
        assert_eq!(app.left.cwd, dir.join("sub"));
        app.cancel_dialog();
        app.refresh_from_disk();
        assert_eq!(app.left.cwd, dir);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_that_is_gone_sends_its_pane_up_saying_so_once() {
        let (mut app, dir) = app_with_files("refresh_gone", &[("a.txt", b"a")]);
        let gone = dir.join("sub").join("deeper");
        std::fs::create_dir_all(&gone).unwrap();
        app.left.change_dir(&gone).unwrap();
        std::fs::remove_dir_all(dir.join("sub")).unwrap();
        app.refresh_from_disk();
        // Up past every directory that went with it.
        assert_eq!(app.left.cwd, dir);
        assert!(!app.should_quit);
        assert!(app.error_dialog_is_notice);
        assert!(app.error_dialog.take().unwrap().contains("is gone"));
        app.refresh_from_disk();
        assert!(app.error_dialog.is_none(), "said once");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_quick_view_follows_a_file_that_changes() {
        let (mut app, dir) = app_with_files("refresh_preview", &[("a.txt", b"kot\n")]);
        app.run_action(Action::View).unwrap();
        app.preview_focus = true;
        assert!(app.open_preview_find());
        app.preview_find_key(KeyCode::Char('k'), KeyModifiers::NONE);
        let read = |app: &App| match preview::build_preview(app.active_pane_ref()) {
            preview::PreviewContent::Text(lines) => lines,
            _ => panic!("not text"),
        };
        // Asked again, the same text comes back without reading the file.
        let first = read(&app);
        assert!(std::rc::Rc::ptr_eq(&first, &read(&app)));
        // Unchanged, the search stays.
        app.refresh_from_disk();
        assert!(app.preview_find.is_some());

        std::fs::write(dir.join("a.txt"), "pies\n").unwrap();
        touch(&dir.join("a.txt"), 10);
        app.refresh_from_disk();
        assert_eq!(read(&app)[0], "pies");
        assert!(app.preview_find.is_none(), "a search in the old text goes");
        assert!(app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// File > Save As, `name` typed over what the field offers.
    fn save_as(app: &mut App, name: &str) {
        app.run_action(Action::SaveAs).unwrap();
        while matches!(&app.dialog, Dialog::TextInput { field, .. } if !field.text().is_empty()) {
            app.dialog_field_backspace();
        }
        type_text(app, name);
        app.confirm_dialog().unwrap();
    }

    #[test]
    fn save_as_writes_a_new_file_and_goes_on_with_it() {
        let (mut app, dir) = app_with_files("editor_save_as", &[("a.txt", b"one\r\ntwo\r\n")]);
        app.run_action(Action::Edit).unwrap();
        app.editor_key(KeyCode::End, KeyModifiers::NONE).unwrap();
        keys(&mut app, "!");
        app.run_action(Action::SaveAs).unwrap();
        match &app.dialog {
            Dialog::TextInput {
                kind: TextInputKind::SaveAs,
                field,
                ..
            } => assert_eq!(field.text(), "a.txt"),
            _ => panic!("no Save As dialog"),
        }
        app.cancel_dialog();
        // Relative: next to the file; the line endings come along.
        save_as(&mut app, "sub/b.txt");
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.path, dir.join("sub/b.txt"));
        assert!(!editor.is_modified());
        assert_eq!(
            std::fs::read(dir.join("sub/b.txt")).unwrap(),
            b"one!\r\ntwo\r\n"
        );
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"one\r\ntwo\r\n");
        // A name that's taken: asked first; No (the default) leaves it.
        save_as(&mut app, &dir.join("a.txt").display().to_string());
        assert!(matches!(app.dialog, Dialog::ConfirmSaveAs { .. }));
        assert!(app.dialog_cancel_focused);
        app.cancel_dialog();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"one\r\ntwo\r\n");
        // A directory that isn't there: an error, nothing changes.
        save_as(&mut app, "nowhere/c.txt");
        assert!(app.error_dialog.take().is_some());
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("sub/b.txt"));
        // Into the listed directory: the cursor goes onto it.
        save_as(&mut app, "../d.txt");
        assert_eq!(app.left.entries[app.left.selected].name, "d.txt");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_as_understands_a_home_path() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let (app, dir) = app_with_files("editor_save_as_home", &[("a.txt", b"x")]);
        let mut app = app;
        app.run_action(Action::Edit).unwrap();
        assert_eq!(app.save_as_target("~/x.txt"), Some(home.join("x.txt")));
        assert_eq!(app.save_as_target("y.txt"), Some(dir.join("y.txt")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn new_file(app: &mut App, name: &str) {
        app.run_action(Action::NewFile).unwrap();
        type_text(app, name);
        app.confirm_dialog().unwrap();
    }

    #[test]
    fn new_file_makes_an_empty_file_whatever_the_editor() {
        for builtin in [true, false] {
            let (mut app, dir) = app_with_files("new_file_on_disk", &[("a.txt", b"x")]);
            app.builtin_editor = builtin;
            new_file(&mut app, "b.txt");
            assert!(app.editor.is_none());
            assert_eq!(std::fs::read(dir.join("b.txt")).unwrap(), b"");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn open_editor_starts_an_untitled_text_saved_where_the_user_is() {
        let (mut app, dir) = app_with_files("open_editor", &[("a.txt", b"x")]);
        app.builtin_editor = false;
        app.run_action(Action::View).unwrap();
        app.run_action(Action::OpenEditor).unwrap();
        let editor = app
            .editor
            .as_ref()
            .expect("the built-in editor all the same");
        assert!(editor.untitled && !editor.is_modified());
        assert_eq!(editor.file_name(), "Untitled");
        assert!(!app.quick_view && !app.list_focus);
        // Untouched, it closes without a word (back to the quick view).
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.editor.is_none() && matches!(app.dialog, Dialog::None));
        assert!(app.quick_view);

        // Typed in, closing asks; Save there asks for a name.
        app.run_action(Action::OpenEditor).unwrap();
        keys(&mut app, "ala");
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.dialog_is_unsaved_changes());
        app.activate_unsaved_button();
        match &app.dialog {
            Dialog::TextInput {
                kind: TextInputKind::SaveAs,
                field,
                ..
            } => assert_eq!(field.text(), "untitled.txt"),
            _ => panic!("no Save As dialog"),
        }
        app.cancel_dialog();
        // F2 has no file to rename: it names it, as Save As.
        app.editor_key(KeyCode::F(2), KeyModifiers::NONE).unwrap();
        assert!(matches!(
            app.dialog,
            Dialog::TextInput {
                kind: TextInputKind::SaveAs,
                ..
            }
        ));
        app.cancel_dialog();
        // Ctrl+S: a name, relative to the active pane's directory.
        app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        while matches!(&app.dialog, Dialog::TextInput { field, .. } if !field.text().is_empty()) {
            app.dialog_field_backspace();
        }
        type_text(&mut app, "notes.md");
        app.confirm_dialog().unwrap();
        assert_eq!(std::fs::read(dir.join("notes.md")).unwrap(), b"ala\n");
        let editor = app.editor.as_ref().unwrap();
        assert!(!editor.untitled && !editor.is_modified());
        assert_eq!(editor.path, dir.join("notes.md"));
        // Now a file like any other: Ctrl+S just saves.
        keys(&mut app, "!");
        app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        assert!(matches!(app.dialog, Dialog::None));
        assert_eq!(std::fs::read(dir.join("notes.md")).unwrap(), b"ala!\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn open_editor_over_unsaved_changes_asks_first_and_a_taken_name_too() {
        let (mut app, dir) = app_with_files("open_editor_over", &[("a.txt", b"x")]);
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "y");
        app.run_action(Action::OpenEditor).unwrap();
        assert!(app.dialog_is_unsaved_changes());
        app.cancel_dialog();
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("a.txt"));
        app.run_action(Action::OpenEditor).unwrap();
        app.step_unsaved_focus(true);
        app.activate_unsaved_button();
        assert!(app.editor.as_ref().unwrap().untitled);
        keys(&mut app, "new");
        app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        while matches!(&app.dialog, Dialog::TextInput { field, .. } if !field.text().is_empty()) {
            app.dialog_field_backspace();
        }
        type_text(&mut app, "a.txt");
        app.confirm_dialog().unwrap();
        assert!(matches!(app.dialog, Dialog::ConfirmSaveAs { .. }));
        app.cancel_dialog();
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An untitled editor with text typed in, and the unsaved-changes
    /// prompt's Save pressed for `then`: Save As is up, `name` typed in it.
    fn untitled_saved_as_for(app: &mut App, then: impl FnOnce(&mut App), name: &str) {
        app.run_action(Action::OpenEditor).unwrap();
        keys(app, "draft");
        then(app);
        assert!(app.dialog_is_unsaved_changes(), "asked first");
        app.activate_unsaved_button();
        assert!(matches!(
            app.dialog,
            Dialog::TextInput {
                kind: TextInputKind::SaveAs,
                ..
            }
        ));
        while matches!(&app.dialog, Dialog::TextInput { field, .. } if !field.text().is_empty()) {
            app.dialog_field_backspace();
        }
        type_text(app, name);
        app.confirm_dialog().unwrap();
    }

    #[test]
    fn saved_through_save_as_the_prompts_action_carries_on() {
        // Close.
        let (mut app, dir) = app_with_files("carry_on_close", &[("a.txt", b"x"), ("b.txt", b"b")]);
        untitled_saved_as_for(
            &mut app,
            |app| app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap(),
            "one.txt",
        );
        assert_eq!(std::fs::read(dir.join("one.txt")).unwrap(), b"draft\n");
        assert!(app.editor.is_none(), "closed once saved");
        // Quit.
        untitled_saved_as_for(
            &mut app,
            |app| app.run_action(Action::Quit).unwrap(),
            "two.txt",
        );
        assert!(dir.join("two.txt").exists() && app.should_quit);
        app.should_quit = false;
        // Open Editor again: a fresh untitled one.
        untitled_saved_as_for(
            &mut app,
            |app| app.run_action(Action::OpenEditor).unwrap(),
            "three.txt",
        );
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.untitled && !editor.is_modified());
        app.editor = None;
        // F4 on another file, from the list.
        untitled_saved_as_for(
            &mut app,
            |app| {
                app.toggle_editor_focus();
                select(app, "b.txt");
                app.run_action(Action::Edit).unwrap();
            },
            "four.txt",
        );
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("b.txt"));
        // F3 from the list: the quick view.
        app.editor = None;
        untitled_saved_as_for(
            &mut app,
            |app| {
                app.toggle_editor_focus();
                app.run_action(Action::View).unwrap();
            },
            "five.txt",
        );
        assert!(app.editor.is_none() && app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelling_the_save_as_the_prompt_asked_for_stops_it_all() {
        let (mut app, dir) = app_with_files("carry_on_cancel", &[("a.txt", b"x")]);
        app.run_action(Action::OpenEditor).unwrap();
        keys(&mut app, "draft");
        app.run_action(Action::Quit).unwrap();
        app.activate_unsaved_button();
        app.cancel_dialog();
        assert!(!app.should_quit);
        assert!(app.editor.as_ref().unwrap().is_modified());
        // Nothing left over to carry on with a later, unrelated Save As.
        app.run_action(Action::SaveAs).unwrap();
        type_text(&mut app, "x");
        app.confirm_dialog().unwrap();
        assert!(!app.should_quit && app.editor.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f4_on_a_file_the_editor_wont_take_leaves_the_edited_one_alone() {
        let (mut app, dir) =
            app_with_files("edit_refused", &[("a.txt", b"x"), ("bin.dat", b"\0\x01")]);
        app.run_action(Action::View).unwrap();
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "y");
        app.toggle_editor_focus();
        select(&mut app, "bin.dat");
        app.run_action(Action::Edit).unwrap();
        assert!(matches!(app.dialog, Dialog::None), "nothing asked");
        assert!(app.error_dialog.take().is_some());
        let editor = app.editor.as_ref().unwrap();
        assert_eq!(editor.lines(), ["yx"]);
        assert!(editor.is_modified());
        // Closing still goes back to the quick view F4 replaced.
        app.editor.as_mut().unwrap().mark_saved();
        app.escape();
        assert!(app.editor.is_none() && app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn f4_on_the_list_with_the_external_editor_setting_runs_it() {
        let (mut app, dir) =
            app_with_files("edit_external_beside", &[("a.txt", b"x"), ("b.txt", b"b")]);
        app.run_action(Action::OpenEditor).unwrap();
        keys(&mut app, "draft");
        app.builtin_editor = false;
        app.toggle_editor_focus();
        select(&mut app, "b.txt");
        app.run_action(Action::Edit).unwrap();
        assert!(matches!(
            app.external_request,
            Some(ExternalRequest::Edit(_))
        ));
        assert!(matches!(app.dialog, Dialog::None));
        let editor = app.editor.as_ref().unwrap();
        assert!(editor.untitled && editor.is_modified());
        assert!(app.list_focus);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_renamed_directory_with_the_edited_file_in_it_is_noted_with_the_new_path() {
        let (mut app, dir) = app_with_files("rename_edited_dir", &[("a.txt", b"x")]);
        std::fs::write(dir.join("sub/in.txt"), "in").unwrap();
        app.left.change_dir(&dir.join("sub")).unwrap();
        select(&mut app, "in.txt");
        app.run_action(Action::Edit).unwrap();
        app.left.change_dir(&dir).unwrap();
        app.toggle_editor_focus();
        select(&mut app, "sub");
        app.run_action(Action::Rename).unwrap();
        while matches!(&app.dialog, Dialog::Rename { field, .. } if !field.text().is_empty()) {
            app.dialog_field_backspace();
        }
        type_text(&mut app, "moved");
        app.confirm_dialog().unwrap();
        let note = app.error_dialog.take().unwrap();
        assert!(
            note.contains(&dir.join("moved/in.txt").display().to_string()),
            "{note}"
        );
        assert_eq!(app.editor.as_ref().unwrap().path, dir.join("sub/in.txt"));
        assert!(
            !app.editor.as_ref().unwrap().is_modified(),
            "its text is still on disk"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_move_job_taking_the_edited_file_is_noted() {
        let (mut app, dir) = app_with_files("move_job_edited", &[("a.txt", b"x")]);
        app.right = Pane::new(dir.join("sub"), false).unwrap();
        app.run_action(Action::Move).unwrap();
        // Opened while the Move was being set up (it runs on, in the
        // background).
        app.editor = Some(Editor::open(&dir.join("a.txt")).unwrap());
        app.confirm_dialog().unwrap();
        wait_for_job(&mut app);
        assert!(dir.join("sub/a.txt").exists());
        let note = app.error_dialog.take().unwrap();
        assert!(note.contains("renamed to"), "{note}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn save_as_keeps_a_private_file_private_and_asks_about_a_dangling_link() {
        use std::os::unix::fs::PermissionsExt;
        let (mut app, dir) = app_with_files("save_as_private", &[(".env", b"KEY=1")]);
        std::fs::set_permissions(dir.join(".env"), std::fs::Permissions::from_mode(0o600)).unwrap();
        app.left.reload().unwrap();
        select(&mut app, ".env");
        app.run_action(Action::Edit).unwrap();
        save_as(&mut app, "copy.env");
        let mode = std::fs::metadata(dir.join("copy.env"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
        std::os::unix::fs::symlink(dir.join("nowhere/x"), dir.join("link")).unwrap();
        save_as(&mut app, "link");
        assert!(matches!(app.dialog, Dialog::ConfirmSaveAs { .. }));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_app_editor_saves_through_a_temp_file_and_pce_in_place() {
        use std::os::unix::fs::MetadataExt;
        let (mut app, dir) = app_with_files("save_kinds", &[("a.txt", b"x")]);
        let inode = || std::fs::metadata(dir.join("a.txt")).unwrap().ino();
        let before = inode();
        app.run_action(Action::Edit).unwrap();
        keys(&mut app, "y");
        app.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        assert_ne!(inode(), before, "the safe save: a new file renamed over");
        let mut pce = App::editor_only(Editor::open(&dir.join("a.txt")).unwrap()).unwrap();
        let before = inode();
        pce.editor_key(KeyCode::Char('z'), KeyModifiers::NONE)
            .unwrap();
        pce.editor_key(KeyCode::Char('s'), KeyModifiers::CONTROL)
            .unwrap();
        assert_eq!(inode(), before, "pce: the same file, written over");
        assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"zyx");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pce_without_a_current_directory_needs_a_full_path() {
        let mut app = App::editor_only(Editor::untitled()).unwrap();
        let gone = std::env::temp_dir().join("pc_test_no_such_dir_at_all");
        app.left = Pane::unlisted(gone.clone());
        app.right = Pane::unlisted(gone);
        assert_eq!(app.save_as_target("notes.txt"), None);
        let full = std::env::temp_dir().join("notes.txt");
        assert_eq!(app.save_as_target(&full.display().to_string()), Some(full));
    }

    #[test]
    fn pce_saves_an_untitled_text_where_it_was_started() {
        let mut app = App::editor_only(Editor::untitled()).unwrap();
        let cwd = env::current_dir().unwrap();
        assert_eq!(app.save_as_target("notes.txt"), Some(cwd.join("notes.txt")));
        // Untouched, closing it just ends the program.
        app.editor_key(KeyCode::Esc, KeyModifiers::NONE).unwrap();
        assert!(app.should_quit && matches!(app.dialog, Dialog::None));
    }

    #[test]
    fn esc_closes_the_quick_view_once_nothing_else_is_left_to_clear() {
        let (mut app, dir) = app_with_files("esc_quick_view", &[("a.txt", b"kot\n")]);
        app.run_action(Action::View).unwrap();
        app.preview_focus = true;
        app.open_preview_find();
        app.preview_find_key(KeyCode::Char('k'), KeyModifiers::NONE);
        app.preview_find_key(KeyCode::Enter, KeyModifiers::NONE);
        app.command_line = "ls".to_string();
        app.escape();
        assert!(
            app.preview_find
                .as_ref()
                .unwrap()
                .text
                .selection()
                .is_none()
        );
        assert_eq!(app.command_line, "ls");
        app.escape();
        assert_eq!(app.command_line, "");
        assert!(app.quick_view);
        app.escape();
        assert!(!app.quick_view && !app.preview_focus);
        // From the listing too.
        app.run_action(Action::View).unwrap();
        app.escape();
        assert!(!app.quick_view);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_finished_job_whose_pane_has_gone_does_not_end_the_program() {
        let dir =
            std::env::temp_dir().join(format!("pc_test_job_pane_gone_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::fs::write(dir.join("src/a"), "x").unwrap();
        let mut app = app_with_panes(&dir.join("src"), &dir.join("dest"));
        select(&mut app, "a");
        app.request_copy();
        app.confirm_dialog().unwrap();
        while !app.job.as_ref().unwrap().is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_dir_all(dir.join("dest")).unwrap();
        app.poll_job().unwrap();
        assert!(!app.should_quit);
        assert!(app.error_dialog.take().unwrap().contains("right pane"));
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
