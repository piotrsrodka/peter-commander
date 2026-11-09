//! The built-in text editor behind F4: a plain Notepad-style buffer —
//! arrows, Shift to select, Ctrl to jump by word, Ctrl+S/Z/C/X/V/F — with
//! nothing modal or exotic to learn. Everything that doesn't need the rest
//! of the app (the text, cursor, selection, undo, wrapping, search, and which
//! key does what) lives here so it can be tested on its own; `App` only adds
//! the file system, the clipboard, the menu and the dialogs around it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyModifiers};
use unicode_width::UnicodeWidthChar;

use crate::transfer::{TextField, is_text_input};

/// Columns between tab stops, both for showing tab characters already in a
/// file and for what the Tab key inserts (that many spaces).
pub const TAB_WIDTH: usize = 4;

/// Files bigger than this go to the external editor instead: the whole text
/// is held (and undone) in memory, and nobody edits a log that size by hand.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// How many undo steps are kept before the oldest are dropped.
const UNDO_LIMIT: usize = 1000;

/// Matches the find bar keeps (and highlights) at most, so searching a big
/// file for a single letter can't eat the memory; the count then reads
/// "N+".
const MATCH_LIMIT: usize = 100_000;

/// A place in the text: a line, and a *character* (not byte) index within
/// it, so multibyte letters like `ą` are one step like any other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Pos { line, col }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    Home,
    End,
    DocStart,
    DocEnd,
    PageUp,
    PageDown,
}

/// What a key press in the editor asks for. The editing ones are carried out
/// by `Editor::apply`; the rest (clipboard, saving, closing, the menu, ...)
/// need the app around the editor and are handled there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Insert(char),
    /// Tab: `TAB_WIDTH` spaces, never a tab character.
    Tab,
    /// Shift+Tab: removes up to `TAB_WIDTH` spaces from the start of the line.
    Unindent,
    NewLine,
    Backspace,
    Delete,
    DeleteWordBack,
    DeleteWordForward,
    Move {
        motion: Motion,
        select: bool,
    },
    SelectAll,
    Undo,
    Redo,
    Find,
    FindNext,
    FindPrevious,
    Copy,
    Cut,
    Paste,
    Save,
    Close,
    ToggleFullScreen,
    Help,
    /// F2: rename the file being edited (the app's own Rename dialog).
    Rename,
    Quit,
    /// F9 (`None`), or Alt+letter for the menu category starting with it.
    Menu(Option<char>),
}

/// Keys while the find bar is open: typing goes into it, F3/Shift+F3 step
/// through the matches, Enter keeps the one found (selected) and Esc forgets
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindKey {
    Insert(char),
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Next,
    Previous,
    Accept,
    Close,
}

/// The Notepad keys. Anything not listed here (other F-keys, Ctrl+other
/// letters, ...) does nothing while the editor has the focus.
pub fn command_for_key(code: KeyCode, modifiers: KeyModifiers) -> Option<Command> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let select = modifiers.contains(KeyModifiers::SHIFT);
    let go = |motion| Some(Command::Move { motion, select });
    match code {
        KeyCode::Char(c) if is_text_input(c, modifiers) => Some(Command::Insert(c)),
        KeyCode::Char(c) if alt && !ctrl && c.is_ascii_alphabetic() => Some(Command::Menu(Some(c))),
        KeyCode::Char(c) if ctrl && !alt => match c.to_ascii_lowercase() {
            's' => Some(Command::Save),
            'a' => Some(Command::SelectAll),
            'c' => Some(Command::Copy),
            'x' => Some(Command::Cut),
            'v' => Some(Command::Paste),
            'z' => Some(Command::Undo),
            'y' => Some(Command::Redo),
            'f' => Some(Command::Find),
            'q' => Some(Command::Quit),
            // Not Ctrl+H for Ctrl+Backspace, although some terminals send
            // it that way: others send ^H for a plain Backspace, which would
            // then delete whole words.
            _ => None,
        },
        KeyCode::Left if ctrl => go(Motion::WordLeft),
        KeyCode::Right if ctrl => go(Motion::WordRight),
        KeyCode::Home if ctrl => go(Motion::DocStart),
        KeyCode::End if ctrl => go(Motion::DocEnd),
        KeyCode::Left => go(Motion::Left),
        KeyCode::Right => go(Motion::Right),
        KeyCode::Up => go(Motion::Up),
        KeyCode::Down => go(Motion::Down),
        KeyCode::Home => go(Motion::Home),
        KeyCode::End => go(Motion::End),
        KeyCode::PageUp => go(Motion::PageUp),
        KeyCode::PageDown => go(Motion::PageDown),
        KeyCode::Tab if !ctrl && !alt => Some(Command::Tab),
        KeyCode::BackTab => Some(Command::Unindent),
        KeyCode::Enter => Some(Command::NewLine),
        KeyCode::Backspace if ctrl => Some(Command::DeleteWordBack),
        KeyCode::Backspace => Some(Command::Backspace),
        KeyCode::Delete if ctrl => Some(Command::DeleteWordForward),
        KeyCode::Delete => Some(Command::Delete),
        KeyCode::Esc => Some(Command::Close),
        KeyCode::F(1) => Some(Command::Help),
        KeyCode::F(2) => Some(Command::Rename),
        KeyCode::F(3) if select => Some(Command::FindPrevious),
        KeyCode::F(3) => Some(Command::FindNext),
        KeyCode::F(4) if !select => Some(Command::ToggleFullScreen),
        KeyCode::F(9) => Some(Command::Menu(None)),
        KeyCode::F(10) => Some(Command::Quit),
        _ => None,
    }
}

/// The find bar's own keys; anything else falls back to `command_for_key`,
/// of which only what makes sense with the bar open is used (see `App`).
pub fn find_bar_key(code: KeyCode, modifiers: KeyModifiers) -> Option<FindKey> {
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    match code {
        KeyCode::Char(c) if is_text_input(c, modifiers) => Some(FindKey::Insert(c)),
        KeyCode::Backspace => Some(FindKey::Backspace),
        KeyCode::Delete => Some(FindKey::Delete),
        KeyCode::Left => Some(FindKey::Left),
        KeyCode::Right => Some(FindKey::Right),
        KeyCode::Home => Some(FindKey::Home),
        KeyCode::End => Some(FindKey::End),
        KeyCode::F(3) if shift => Some(FindKey::Previous),
        KeyCode::F(3) => Some(FindKey::Next),
        KeyCode::Enter => Some(FindKey::Accept),
        KeyCode::Esc => Some(FindKey::Close),
        _ => None,
    }
}

/// Cells `c` takes on screen when it starts at column `x`: a tab reaches the
/// next tab stop, other control characters (and direction marks) show as one
/// placeholder cell (see `display_glyph`), wide CJK letters take two and
/// combining marks none.
pub fn char_width(c: char, x: usize) -> usize {
    if c == '\t' {
        TAB_WIDTH - x % TAB_WIDTH
    } else if c.is_control() || is_bidi_control(c) {
        1
    } else {
        c.width().unwrap_or(1)
    }
}

/// What to draw for `c`: control characters (a stray `\r`, an escape) get
/// their visible Unicode "control picture" (`␍`, `␛`) so they can neither
/// disturb the terminal nor vanish. Tabs are drawn as spaces by the caller.
/// Marks that turn the direction of the text around show as U+FFFD (`�`):
/// a terminal that honors them would show the line in a different order
/// than it's in the file ("Trojan Source").
pub fn display_glyph(c: char) -> char {
    match c as u32 {
        code @ 0..=0x1f => char::from_u32(0x2400 + code).unwrap_or('?'),
        0x7f => '\u{2421}',
        code if c.is_control() && code < 0xa0 => '?',
        _ if is_bidi_control(c) => '\u{fffd}',
        _ => c,
    }
}

/// The invisible marks that set or override the direction of text: LRM/RLM,
/// the embeddings and overrides (U+202A–U+202E) and the isolates
/// (U+2066–U+2069), plus the Arabic letter mark.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// Cells `chars[from..to]` take when laid out from the start of a screen row.
fn width_of(chars: &[char], from: usize, to: usize) -> usize {
    chars[from..to].iter().fold(0, |x, &c| x + char_width(c, x))
}

/// The screen column (from the start of the line) where character `col` of
/// `line` begins, when it isn't wrapped.
#[cfg(test)]
fn x_of(line: &str, col: usize) -> usize {
    let chars: Vec<char> = line.chars().collect();
    width_of(&chars, 0, col.min(chars.len()))
}

/// The character in the screen row `chars[start..end]` nearest to column `x`
/// of that row: a click on the right half of a wide letter or tab lands after
/// it. Past the row's end, its end — except on a row that wraps on to the
/// next, whose end *is* the next row's start, so the last character instead.
/// The search begins at `from`, a character of the row and the column it
/// starts at (see `LineLayout::seek`), rather than at the row's start.
fn col_in_row(
    chars: &[char],
    (start, end): (usize, usize),
    from: (usize, usize),
    x: usize,
    last_row: bool,
) -> usize {
    let (from, mut cell) = from;
    for (col, &c) in chars.iter().enumerate().take(end).skip(from) {
        let width = char_width(c, cell);
        if x < cell + width {
            return if width > 1 && x - cell >= width.div_ceil(2) {
                col + 1
            } else {
                col
            };
        }
        cell += width;
    }
    if last_row || end == start {
        end
    } else {
        end - 1
    }
}

/// Where each screen row of `line` starts, as character indices.
#[cfg(test)]
fn row_starts(line: &str, wrap: Option<usize>) -> Vec<usize> {
    row_starts_in(&line.chars().collect::<Vec<_>>(), wrap)
}

/// Where each screen row of the line `chars` starts, as character indices
/// (the first is always 0). Without `wrap` a line is one row however long;
/// with it, rows are at most that many cells, broken after a space where
/// there is one, as Notepad's word wrap does. A single space may hang one
/// cell past the edge instead of starting the next row.
fn row_starts_in(chars: &[char], wrap: Option<usize>) -> Vec<usize> {
    let Some(width) = wrap else {
        return vec![0];
    };
    let mut starts = vec![0];
    wrap_rows_from(chars, 0, width, |start| {
        starts.push(start);
        true
    });
    starts
}

/// The wrapping behind `row_starts_in`, from the row starting at character
/// `first` on: hands each further row's start to `next`, until that returns
/// false. Where a row starts depends only on the text from the row before
/// it, so this can pick up from any row start of a line that changed after
/// it.
fn wrap_rows_from(chars: &[char], first: usize, width: usize, mut next: impl FnMut(usize) -> bool) {
    let mut row_start = first;
    let mut x = 0;
    let mut after_space: Option<usize> = None;
    let mut col = first;
    while col < chars.len() {
        let c = chars[col];
        let w = char_width(c, x);
        let limit = if c == ' ' { width + 1 } else { width };
        // `col > row_start`: a row always takes at least one character, even
        // one wider than the whole row.
        if x + w > limit && col > row_start {
            let at = after_space.filter(|&b| b > row_start).unwrap_or(col);
            if !next(at) {
                return;
            }
            row_start = at;
            after_space = None;
            x = width_of(chars, row_start, col);
            continue;
        }
        x += w;
        col += 1;
        if c.is_whitespace() {
            after_space = Some(col);
        }
    }
}

/// The width lines wrap at in a text area `cols` cells wide: one cell less,
/// so the cursor after a full row (or a space hanging past it) still has a
/// cell. The quick view wraps at the same width, so a file breaks into the
/// same rows there as here.
pub fn wrap_width_for(cols: usize) -> usize {
    cols.saturating_sub(1).max(1)
}

/// Characters between the columns `LineLayout` notes down.
const CHECKPOINT: usize = 1024;

/// Lines whose layout is kept at most; more than a screenful or two of them
/// is only ever needed for a moment (a long PgDn), so the cache then simply
/// starts over.
const LAYOUT_CACHE_LINES: usize = 4096;

/// How one line lies on screen, worked out once for each state of the text
/// and width of the view instead of for every screen row and every question
/// in a frame: with a 10 MB file on a single line, measuring it a dozen
/// times a frame took more than half a second.
pub struct LineLayout {
    pub chars: Vec<char>,
    /// Where each screen row starts (see `row_starts_in`).
    pub starts: Vec<usize>,
    /// Only for a long line that isn't wrapped: the screen column at every
    /// `CHECKPOINT`-th character, so a place far along it can be found
    /// without measuring everything before it.
    checkpoints: Vec<usize>,
}

impl LineLayout {
    /// `line` laid out unwrapped, or wrapped at `wrap` cells.
    pub fn new(line: &str, wrap: Option<usize>) -> Self {
        let chars: Vec<char> = line.chars().collect();
        let starts = row_starts_in(&chars, wrap);
        let mut layout = LineLayout {
            chars,
            starts,
            checkpoints: Vec::new(),
        };
        if wrap.is_none() {
            layout.redo_checkpoints(0);
        }
        layout
    }

    /// Characters `from..to` of the line were replaced by `text` (no line
    /// break in it): brings the layout up to date without starting over,
    /// which for a huge line would cost every key typed into it a pass over
    /// the whole of it.
    fn edit(&mut self, from: usize, to: usize, text: &str, wrap: Option<usize>) {
        let inserted: Vec<char> = text.chars().collect();
        let edited_end = from + inserted.len();
        let delta = inserted.len() as isize - (to - from) as isize;
        self.chars.splice(from..to, inserted);
        let Some(width) = wrap else {
            self.redo_checkpoints(from / CHECKPOINT);
            return;
        };
        // The rows up to two before the edit are as they were: each row's
        // start depends only on the text from the row before it on.
        let resume = self
            .starts
            .partition_point(|&start| start < from)
            .saturating_sub(2);
        let old = self.starts.split_off(resume + 1);
        let mut next_old = 0;
        let mut synced = None;
        let starts = &mut self.starts;
        wrap_rows_from(&self.chars, starts[resume], width, |start| {
            // Past the edit, a row starting where one started before (moved
            // by the edit) is followed by the same rows as before.
            if start >= edited_end {
                let was = (start as isize - delta) as usize;
                while next_old < old.len() && old[next_old] < was {
                    next_old += 1;
                }
                if old.get(next_old) == Some(&was) {
                    synced = Some(next_old);
                    return false;
                }
            }
            starts.push(start);
            true
        });
        if let Some(rest) = synced {
            starts.extend(
                old[rest..]
                    .iter()
                    .map(|&start| (start as isize + delta) as usize),
            );
        }
    }

    /// Works out `checkpoints` again from the `keep`-th on (those before it
    /// still being right), or drops them for a line short enough to do
    /// without.
    fn redo_checkpoints(&mut self, keep: usize) {
        if self.chars.len() <= CHECKPOINT {
            self.checkpoints.clear();
            return;
        }
        self.checkpoints.truncate(keep);
        let first = self.checkpoints.len() * CHECKPOINT;
        let mut x = self.checkpoints.last().map_or(0, |&at| {
            // The column after the last kept checkpoint's characters.
            self.chars[first - CHECKPOINT..first]
                .iter()
                .fold(at, |x, &c| x + char_width(c, x))
        });
        for (col, &c) in self.chars.iter().enumerate().skip(first) {
            if col % CHECKPOINT == 0 {
                self.checkpoints.push(x);
            }
            x += char_width(c, x);
        }
    }

    /// Cells from the start of the screen row beginning at character
    /// `start` to character `col`.
    pub fn x_of(&self, start: usize, col: usize) -> usize {
        if start == 0 && !self.checkpoints.is_empty() {
            let k = (col / CHECKPOINT).min(self.checkpoints.len() - 1);
            return self.chars[k * CHECKPOINT..col]
                .iter()
                .fold(self.checkpoints[k], |x, &c| x + char_width(c, x));
        }
        width_of(&self.chars, start, col)
    }

    /// Where to start looking for column `x` of the screen row beginning at
    /// character `start`: a character at or before the one there, and the
    /// column it begins at.
    pub fn seek(&self, start: usize, x: usize) -> (usize, usize) {
        if start == 0 && !self.checkpoints.is_empty() {
            let k = self
                .checkpoints
                .partition_point(|&at| at <= x)
                .saturating_sub(1);
            return (k * CHECKPOINT, self.checkpoints[k]);
        }
        (start, 0)
    }
}

/// The layouts worked out so far, all for one state of the text (see
/// `Editor::text_gen`) and one wrap width.
#[derive(Default)]
struct LayoutCache {
    key: (u64, Option<usize>),
    lines: HashMap<usize, Rc<LineLayout>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punct,
}

/// Word motion stops where one of these runs ends, as in Windows: letters
/// and digits (Polish ones included) with `_`, other symbols, and spaces.
fn class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// A hash of the whole text, to tell it from the saved one.
fn text_hash(lines: &[String]) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    lines.hash(&mut hasher);
    hasher.finish()
}

/// Case folding for search, one character for one so match positions line up
/// with the text: `Ż` finds `ż`. The rare letter whose lowercase is longer
/// (Turkish `İ`) is left as it is.
fn fold(c: char) -> char {
    let mut lower = c.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(l), None) => l,
        _ => c,
    }
}

/// Where the text `text` ends if it's put at `start` (`\n`-separated).
fn end_of(start: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        None => Pos::new(start.line, start.col + text.chars().count()),
        Some(last) => Pos::new(
            start.line + text.matches('\n').count(),
            text[last + 1..].chars().count(),
        ),
    }
}

fn byte_index(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map_or(line.len(), |(index, _)| index)
}

/// Which kind of edit an undo step was, so that typing a word or holding
/// Backspace is undone in one go rather than a character at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditKind {
    Typing,
    Backspace,
    Delete,
    Other,
}

/// One undo step: at `start`, `removed` was replaced by `inserted`. Storing
/// just the difference (not a copy of the text) keeps a long undo history
/// cheap even for big files.
#[derive(Debug, Clone)]
struct Change {
    start: Pos,
    removed: String,
    inserted: String,
    cursor_before: Pos,
    anchor_before: Option<Pos>,
    version_before: u64,
    version_after: u64,
}

/// One screen row of text to draw: characters `start..end` of `line`, and
/// whether it's that line's last row (where a selected line break shows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub last: bool,
}

/// The Ctrl+F bar. The text can't change while it's open (typing goes into
/// the bar), so the matches are found once per change of the search text.
pub struct FindBar {
    pub field: TextField,
    /// The text was put there on opening (the selection, or the last
    /// search): like a browser's preselected find text, the first letter
    /// typed replaces it.
    pub fresh: bool,
    /// Every match, in order. Matches never span lines.
    matches: Vec<(Pos, Pos)>,
    capped: bool,
    current: Option<usize>,
    /// Where the search started: the first match at or after it is the one
    /// shown while typing, and with no match the cursor goes back here.
    origin: Pos,
}

impl FindBar {
    /// "3/12", "No matches", or nothing before anything has been typed.
    pub fn status(&self) -> String {
        if self.field.text().is_empty() {
            return String::new();
        }
        let plus = if self.capped { "+" } else { "" };
        match self.current {
            Some(current) => format!("{}/{}{plus}", current + 1, self.matches.len()),
            None => "No matches".to_string(),
        }
    }

    /// The matches on `line`, for highlighting.
    pub fn matches_on(&self, line: usize) -> &[(Pos, Pos)] {
        let from = self.matches.partition_point(|(start, _)| start.line < line);
        let to = self
            .matches
            .partition_point(|(start, _)| start.line <= line);
        &self.matches[from..to]
    }
}

pub struct Editor {
    pub path: PathBuf,
    /// Never empty: an empty file is one empty line.
    lines: Vec<String>,
    /// The line ending written back on save, as found in the file.
    crlf: bool,
    /// Whether the file ended with a line ending, kept so saving an
    /// untouched last line doesn't add or drop one.
    trailing_newline: bool,
    /// A UTF-8 byte order mark at the start of the file, written back too.
    bom: bool,
    cursor: Pos,
    /// The other end of the selection, if any — where Shift+movement began.
    anchor: Option<Pos>,
    /// The screen column Up/Down try to keep, so passing through a short
    /// line doesn't pull the cursor to the left for good.
    goal_x: Option<usize>,
    /// Whether F4 has made the editor fill the whole terminal.
    pub full_screen: bool,
    /// Edit > Wrap Lines: long lines continue on the next screen row instead
    /// of running off to the right.
    wrap: bool,
    /// The top of the view: a line, and which of its screen rows (always 0
    /// without wrapping).
    scroll_row: usize,
    scroll_sub: usize,
    /// First screen column shown (always 0 when wrapping).
    scroll_x: usize,
    /// Off after the mouse wheel scrolled the view, so the next frame doesn't
    /// jump straight back to the cursor; any key turns it on again.
    follow_cursor: bool,
    /// Size of the text area in the last drawn frame: the size of a PgUp/PgDn
    /// and the width lines wrap at.
    view_rows: usize,
    view_cols: usize,
    undo: Vec<Change>,
    redo: Vec<Change>,
    /// The kind of the last edit while the next one may still join its undo
    /// step; cleared by anything else (moving the cursor, undo, ...).
    merge: Option<EditKind>,
    /// Identifies the text's state: every edit gets a fresh number and undo
    /// brings back the old one, so undoing back to what's on disk counts as
    /// unmodified again.
    version: u64,
    next_version: u64,
    saved_version: u64,
    /// The saved text's line count and hash, so a text that came back to
    /// it by any road (typing a letter and deleting it again) counts as
    /// unmodified too, not only undoing back to it.
    saved_lines: usize,
    saved_hash: u64,
    /// The text's length in bytes (lines joined by `\n`), kept up to date
    /// edit by edit, and the saved text's: while either count differs from
    /// the saved one, the text is modified without hashing all of it — the
    /// usual case while typing.
    bytes: usize,
    saved_bytes: usize,
    /// `is_modified`'s answer for one `version`, so the text is hashed once
    /// per edit rather than on every draw.
    modified: Cell<Option<(u64, bool)>>,
    pub find: Option<FindBar>,
    /// What was searched for last, offered again when Ctrl+F reopens the
    /// bar.
    last_search: String,
    /// A short note for the status line ("Saved"), until the next key.
    pub message: Option<String>,
    /// No file at all yet (Command > Open Editor): "Untitled", and the
    /// first save asks for a name.
    pub untitled: bool,
    /// Named, but not on disk yet (`pce new.txt`): the first save makes it.
    pub new_file: bool,
    /// Which file on disk this is (device and inode), as opened or last
    /// saved — to notice it renamed, deleted or replaced under the editor,
    /// so a save can't silently land somewhere other than the user thinks.
    pub disk_id: Option<crate::fs_ops::FileId>,
    /// The file's permission bits as opened, for a copy made of it by Save
    /// As (a private file stays private); `None` for Untitled.
    pub mode: Option<u32>,
    /// Counts every change to `lines`, so what's worked out from them can be
    /// kept until they change: unlike `version`, it never goes back.
    text_gen: u64,
    layout: RefCell<LayoutCache>,
    /// The text case-folded for search, as of `text_gen`.
    folded: RefCell<Option<(u64, Rc<Vec<String>>)>>,
}

impl Editor {
    /// Reads `path` for editing, or says why the built-in editor won't —
    /// it only takes files it can save back exactly as they were.
    pub fn open(path: &Path) -> Result<Editor, String> {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        Editor::read(path).map_err(|why| {
            format!(
                "Cannot edit \"{name}\" here: {why} (Settings: External editor opens it in $EDITOR)"
            )
        })
    }

    /// `open`, its refusal just the reason ("not a regular file").
    pub fn read(path: &Path) -> Result<Editor, String> {
        let meta = std::fs::metadata(path).map_err(|err| err.to_string())?;
        // A FIFO or device would block the read forever, or never end.
        if !meta.is_file() {
            return Err("not a regular file".to_string());
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err("larger than 10 MB".to_string());
        }
        let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
        let mut editor = Editor::from_bytes(path.to_path_buf(), &bytes)?;
        editor.disk_id = crate::fs_ops::file_identity(path);
        editor.mode = crate::fs_ops::permission_bits(&meta);
        Ok(editor)
    }

    /// An empty buffer for `path`, which isn't there yet (`pce new.txt`):
    /// nothing is made on disk until the first save, which makes it with LF
    /// line endings and a final newline.
    pub fn new_file(path: PathBuf) -> Editor {
        let mut editor = Editor::from_bytes(path, b"").expect("empty text is fine");
        editor.trailing_newline = true;
        editor.new_file = true;
        editor
    }

    /// `pce +N`: the cursor to the start of line `line` (counted from 1),
    /// or the last line if there are fewer.
    pub fn go_to_line(&mut self, line: usize) {
        self.cursor = Pos::new(line.saturating_sub(1).min(self.lines.len() - 1), 0);
        self.anchor = None;
        self.goal_x = None;
        self.follow_cursor = true;
    }

    /// An empty buffer that's no file yet: LF line endings, a final newline
    /// and no byte order mark once it's saved as one.
    pub fn untitled() -> Editor {
        let mut editor = Editor::from_bytes(PathBuf::new(), b"").expect("empty text is fine");
        editor.trailing_newline = true;
        editor.untitled = true;
        editor
    }

    /// The text of a file as it is on disk; `Err` says why it isn't plain
    /// text this editor can round-trip.
    pub fn from_bytes(path: PathBuf, bytes: &[u8]) -> Result<Editor, &'static str> {
        if bytes.contains(&0) {
            return Err("it looks like a binary file");
        }
        let text = std::str::from_utf8(bytes).map_err(|_| "it isn't UTF-8 text")?;
        let (bom, text) = match text.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        // CRLF only when *every* line ends that way: a file mixing both is
        // kept as LF lines with the odd `\r` left in the text, so saving it
        // changes nothing that wasn't edited.
        let newlines = text.matches('\n').count();
        let crlf = newlines > 0 && text.matches("\r\n").count() == newlines;
        let trailing_newline = text.ends_with('\n');
        let body = if crlf {
            text.replace("\r\n", "\n")
        } else {
            text.to_string()
        };
        let body = body.strip_suffix('\n').unwrap_or(&body);
        let lines: Vec<String> = body.split('\n').map(String::from).collect();
        let saved_lines = lines.len();
        let saved_hash = text_hash(&lines);
        let bytes = body.len();
        Ok(Editor {
            path,
            lines,
            crlf,
            trailing_newline,
            bom,
            cursor: Pos::default(),
            anchor: None,
            goal_x: None,
            full_screen: false,
            wrap: false,
            scroll_row: 0,
            scroll_sub: 0,
            scroll_x: 0,
            follow_cursor: true,
            view_rows: 1,
            view_cols: 80,
            undo: Vec::new(),
            redo: Vec::new(),
            merge: None,
            version: 0,
            next_version: 1,
            saved_version: 0,
            saved_lines,
            saved_hash,
            bytes,
            saved_bytes: bytes,
            modified: Cell::new(None),
            find: None,
            last_search: String::new(),
            message: None,
            text_gen: 0,
            layout: RefCell::default(),
            folded: RefCell::default(),
            untitled: false,
            new_file: false,
            disk_id: None,
            mode: None,
        })
    }

    /// The file's new contents, with its own line endings, final newline
    /// and byte order mark.
    pub fn to_bytes(&self) -> Vec<u8> {
        let eol = if self.crlf { "\r\n" } else { "\n" };
        let mut text = String::new();
        if self.bom {
            text.push('\u{feff}');
        }
        text.push_str(&self.lines.join(eol));
        if self.trailing_newline {
            text.push_str(eol);
        }
        text.into_bytes()
    }

    /// Whether the text differs from what's on disk: not after undoing back
    /// to it, nor after an edit that put back what was there.
    pub fn is_modified(&self) -> bool {
        if self.version == self.saved_version {
            return false;
        }
        if let Some((version, modified)) = self.modified.get()
            && version == self.version
        {
            return modified;
        }
        let modified = self.lines.len() != self.saved_lines
            || self.bytes != self.saved_bytes
            || text_hash(&self.lines) != self.saved_hash;
        self.modified.set(Some((self.version, modified)));
        modified
    }

    /// Everything about the editor's state a key could change, for tests
    /// that prove a key went elsewhere.
    #[cfg(test)]
    pub fn snapshot(&self) -> String {
        format!(
            "{:?}",
            (
                self.to_bytes(),
                self.cursor,
                self.anchor,
                self.undo.len(),
                self.redo.len(),
                self.version,
                self.is_modified(),
                (self.scroll_row, self.scroll_sub, self.scroll_x),
                self.full_screen,
                self.find
                    .as_ref()
                    .map(|bar| (bar.field.text(), bar.status())),
                (&self.path, &self.message, &self.last_search),
            )
        )
    }

    /// The file is gone from disk: whatever the text, it isn't saved —
    /// until Ctrl+S writes it again.
    pub fn mark_unsaved(&mut self) {
        self.saved_version = u64::MAX;
        self.saved_lines = usize::MAX;
        self.saved_bytes = usize::MAX;
        self.modified.set(None);
    }

    pub fn mark_saved(&mut self) {
        self.saved_version = self.version;
        self.saved_lines = self.lines.len();
        self.saved_hash = text_hash(&self.lines);
        self.saved_bytes = self.bytes;
        self.modified.set(None);
        // Typing on after a save starts a new undo step, so undo can go back
        // to exactly what's on disk.
        self.merge = None;
    }

    pub fn file_name(&self) -> String {
        if self.untitled {
            return "Untitled".to_string();
        }
        self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// The line ending after `line` as the file has it: CR LF or LF, or
    /// none after the last line of a file that doesn't end with one.
    pub fn line_ending(&self, line: usize) -> &'static [char] {
        if line + 1 == self.lines.len() && !self.trailing_newline {
            &[]
        } else if self.crlf {
            &['\r', '\n']
        } else {
            &['\n']
        }
    }

    #[cfg(test)]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn cursor(&self) -> Pos {
        self.cursor
    }

    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
        self.scroll_sub = 0;
        self.scroll_x = 0;
        self.goal_x = None;
        self.follow_cursor = true;
    }

    /// The first screen column shown (0 while wrapping).
    pub fn scroll_x(&self) -> usize {
        self.scroll_x
    }

    /// The selected range, start first; `None` when nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        match anchor.cmp(&self.cursor) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Less => Some((anchor, self.cursor)),
            std::cmp::Ordering::Greater => Some((self.cursor, anchor)),
        }
    }

    /// Esc with something selected: the selection goes, the cursor stays
    /// where it is.
    pub fn deselect(&mut self) {
        self.anchor = None;
        self.merge = None;
        self.message = None;
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection()?;
        Some(self.text_between(start, end))
    }

    fn line_len(&self, line: usize) -> usize {
        self.line_layout(line).chars.len()
    }

    fn doc_end(&self) -> Pos {
        let last = self.lines.len() - 1;
        Pos::new(last, self.line_len(last))
    }

    fn text_between(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            return self.lines[start.line]
                .chars()
                .skip(start.col)
                .take(end.col - start.col)
                .collect();
        }
        let mut text: String = self.lines[start.line].chars().skip(start.col).collect();
        for line in &self.lines[start.line + 1..end.line] {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        text.extend(self.lines[end.line].chars().take(end.col));
        text
    }

    // --- Layout: screen rows, for wrapping and scrolling -----------------

    /// The width lines wrap at (see `wrap_width_for`).
    fn wrap_width(&self) -> Option<usize> {
        self.wrap.then(|| wrap_width_for(self.view_cols))
    }

    /// How `line` lies on screen now, from the cache when the text and the
    /// view's width haven't changed since it was worked out.
    pub fn line_layout(&self, line: usize) -> Rc<LineLayout> {
        let key = (self.text_gen, self.wrap_width());
        let mut cache = self.layout.borrow_mut();
        if cache.key != key || cache.lines.len() >= LAYOUT_CACHE_LINES {
            cache.key = key;
            cache.lines.clear();
        }
        Rc::clone(
            cache
                .lines
                .entry(line)
                .or_insert_with(|| Rc::new(LineLayout::new(&self.lines[line], key.1))),
        )
    }

    /// How many screen rows `line` takes.
    fn rows_in(&self, line: usize) -> usize {
        self.line_layout(line).starts.len()
    }

    /// Which screen row of its line `pos` is on, and its column in that row.
    /// A position where a wrapped row ends shows at the start of the next.
    fn visual(&self, pos: Pos) -> (usize, usize) {
        let layout = self.line_layout(pos.line);
        let row = layout.starts.partition_point(|&start| start <= pos.col) - 1;
        (row, layout.x_of(layout.starts[row], pos.col))
    }

    /// The cursor's screen row (as line and row within it) and column, from
    /// the text's own left edge.
    pub fn cursor_screen(&self) -> (usize, usize, usize) {
        let (row, x) = self.visual(self.cursor);
        (self.cursor.line, row, x)
    }

    /// Where the cursor is drawn, as a screen row and column within the
    /// text area, if it's in view. While wrapping, a cursor after a space
    /// hanging past the edge stays in the last cell.
    pub fn cursor_in_view(&self) -> Option<(usize, usize)> {
        let (line, row, x) = self.cursor_screen();
        let start = self.line_layout(line).starts[row];
        let screen_row = self
            .screen_rows(self.view_rows)
            .iter()
            .position(|shown| shown.line == line && shown.start == start)?;
        let x = x.checked_sub(self.scroll_x)?;
        if self.wrap {
            Some((screen_row, x.min(self.view_cols - 1)))
        } else {
            (x < self.view_cols).then_some((screen_row, x))
        }
    }

    /// `delta` screen rows on from `at` (line, row), stopping at the first
    /// and last row of the text.
    fn step_rows(&self, at: (usize, usize), delta: isize) -> (usize, usize) {
        // Held to the text, in case it shrank under a remembered position.
        let mut line = at.0.min(self.lines.len() - 1);
        let mut rows_here = self.rows_in(line);
        let mut row = at.1.min(rows_here - 1);
        for _ in 0..delta.unsigned_abs() {
            if delta > 0 {
                if row + 1 < rows_here {
                    row += 1;
                } else if line + 1 < self.lines.len() {
                    line += 1;
                    row = 0;
                    rows_here = self.rows_in(line);
                } else {
                    break;
                }
            } else if row > 0 {
                row -= 1;
            } else if line > 0 {
                line -= 1;
                rows_here = self.rows_in(line);
                row = rows_here - 1;
            } else {
                break;
            }
        }
        (line, row)
    }

    /// The highest the view's top may scroll to: where the text's last row
    /// sits at the bottom.
    fn max_top(&self) -> (usize, usize) {
        let last = self.lines.len() - 1;
        let last_row = self.rows_in(last) - 1;
        self.step_rows((last, last_row), -(self.view_rows.max(1) as isize - 1))
    }

    fn set_top(&mut self, top: (usize, usize)) {
        (self.scroll_row, self.scroll_sub) = top;
    }

    /// The position on screen row `row` of `line` nearest column `x`.
    fn pos_in_row(&self, line: usize, row: usize, x: usize) -> Pos {
        let layout = self.line_layout(line);
        let start = layout.starts[row];
        let end = layout
            .starts
            .get(row + 1)
            .copied()
            .unwrap_or(layout.chars.len());
        let last_row = row + 1 == layout.starts.len();
        let from = layout.seek(start, x);
        Pos::new(
            line,
            col_in_row(&layout.chars, (start, end), from, x, last_row),
        )
    }

    /// Called with the text area's size each frame: remembers it for paging
    /// and wrapping, and scrolls so the cursor is in view (unless the wheel
    /// moved away).
    pub fn scroll_into_view(&mut self, rows: usize, cols: usize) {
        self.view_rows = rows.max(1);
        self.view_cols = cols.max(1);
        if self.wrap {
            self.scroll_x = 0;
        }
        // A resize can change how many rows the top line wraps to, and
        // where the end of the text is.
        self.scroll_row = self.scroll_row.min(self.lines.len() - 1);
        self.scroll_sub = self.scroll_sub.min(self.rows_in(self.scroll_row) - 1);
        let max_top = self.max_top();
        if (self.scroll_row, self.scroll_sub) > max_top {
            self.set_top(max_top);
        }
        if !self.follow_cursor || rows == 0 || cols == 0 {
            return;
        }
        let (row, x) = self.visual(self.cursor);
        let cursor_row = (self.cursor.line, row);
        if cursor_row < (self.scroll_row, self.scroll_sub) {
            self.set_top(cursor_row);
        } else {
            let lowest_top = self.step_rows(cursor_row, -(rows as isize - 1));
            if lowest_top > (self.scroll_row, self.scroll_sub) {
                self.set_top(lowest_top);
            }
        }
        if !self.wrap {
            if x < self.scroll_x {
                self.scroll_x = x;
            } else if x >= self.scroll_x + cols {
                self.scroll_x = x + 1 - cols;
            }
        }
    }

    /// The screen rows to draw, from the top of the view: at most `count`.
    pub fn screen_rows(&self, count: usize) -> Vec<ScreenRow> {
        let mut rows = Vec::with_capacity(count);
        let mut line = self.scroll_row;
        let mut sub = self.scroll_sub;
        while rows.len() < count && line < self.lines.len() {
            let layout = self.line_layout(line);
            let starts = &layout.starts;
            let len = layout.chars.len();
            for (idx, &start) in starts.iter().enumerate().skip(sub) {
                if rows.len() == count {
                    break;
                }
                let last = idx + 1 == starts.len();
                rows.push(ScreenRow {
                    line,
                    start,
                    end: starts.get(idx + 1).copied().unwrap_or(len),
                    last,
                });
            }
            line += 1;
            sub = 0;
        }
        rows
    }

    /// Mouse wheel: moves the view only, leaving the cursor where it is.
    pub fn scroll_view(&mut self, rows: isize) {
        self.follow_cursor = false;
        let top = self.step_rows((self.scroll_row, self.scroll_sub), rows);
        self.set_top(top.min(self.max_top()));
    }

    /// A click (or drag, with `extend`) at screen row `row`, column `x` of
    /// the text area.
    pub fn click(&mut self, row: usize, x: usize, extend: bool) {
        self.merge = None;
        self.message = None;
        self.follow_cursor = true;
        let (line, sub) = self.step_rows((self.scroll_row, self.scroll_sub), row as isize);
        let pos = self.pos_in_row(line, sub, self.scroll_x + x);
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = pos;
        self.goal_x = None;
    }

    // --- Editing ----------------------------------------------------------

    /// The one primitive every edit goes through: replaces `start..end` with
    /// `text` (`\n`-separated) and returns what was there.
    fn splice(&mut self, start: Pos, end: Pos, text: &str) -> String {
        let removed = self.splice_text(start, end, text);
        self.bytes = self.bytes + text.len() - removed.len();
        removed
    }

    fn splice_text(&mut self, start: Pos, end: Pos, text: &str) -> String {
        if start.line == end.line && !text.contains('\n') {
            return self.splice_in_line(start.line, start.col, end.col, text);
        }
        let removed = self.text_between(start, end);
        let head = &self.lines[start.line][..byte_index(&self.lines[start.line], start.col)];
        let tail = &self.lines[end.line][byte_index(&self.lines[end.line], end.col)..];
        let mut new_lines: Vec<String> = text.split('\n').map(String::from).collect();
        new_lines[0].insert_str(0, head);
        if let Some(last) = new_lines.last_mut() {
            last.push_str(tail);
        }
        self.lines.splice(start.line..=end.line, new_lines);
        self.text_gen += 1;
        removed
    }

    /// `splice` within one line — most edits, like every letter typed. The
    /// line is changed where it is, and its layout updated rather than
    /// thrown away: the other lines' layouts stay right as they are.
    fn splice_in_line(&mut self, line: usize, from: usize, to: usize, text: &str) -> String {
        let string = &mut self.lines[line];
        let start = byte_index(string, from);
        let end = start + byte_index(&string[start..], to - from);
        let removed = string[start..end].to_string();
        string.replace_range(start..end, text);

        let wrap = self.wrap_width();
        let cache = self.layout.get_mut();
        let current = cache.key == (self.text_gen, wrap);
        self.text_gen += 1;
        if current {
            cache.key = (self.text_gen, wrap);
            let layout = cache
                .lines
                .remove(&line)
                .and_then(|rc| Rc::try_unwrap(rc).ok());
            if let Some(mut layout) = layout {
                layout.edit(from, to, text, wrap);
                cache.lines.insert(line, Rc::new(layout));
            }
        }
        removed
    }

    /// Replaces `start..end` with `text`, records it for undo (joined to
    /// the last step where `kind` allows) and leaves the cursor after it.
    fn edit(&mut self, kind: EditKind, start: Pos, end: Pos, text: &str) {
        let cursor_before = self.cursor;
        let anchor_before = self.anchor;
        let version_before = self.version;
        let removed = self.splice(start, end, text);
        self.cursor = end_of(start, text);
        self.anchor = None;
        self.goal_x = None;
        self.redo.clear();
        self.version = self.next_version;
        self.next_version += 1;

        if self.merge == Some(kind)
            && let Some(last) = self.undo.last_mut()
            && merge_into(last, kind, start, end, &removed, text)
        {
            last.version_after = self.version;
            return;
        }
        self.undo.push(Change {
            start,
            removed,
            inserted: text.to_string(),
            cursor_before,
            anchor_before,
            version_before,
            version_after: self.version,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.merge = (kind != EditKind::Other).then_some(kind);
    }

    /// Typing, a paste, Enter: `text` replaces the selection, if any, or
    /// goes in at the cursor.
    fn replace_selection(&mut self, kind: EditKind, text: &str) {
        let (start, end) = self.selection().unwrap_or((self.cursor, self.cursor));
        self.edit(kind, start, end, text);
    }

    /// Pasted text, with any line endings it brought (`\r\n` from Windows,
    /// a lone `\r`) turned into this editor's own. With the find bar open it
    /// goes there instead, its first line only.
    pub fn insert_text(&mut self, text: &str) {
        if let Some(bar) = &mut self.find {
            let first_line = text.lines().next().unwrap_or("");
            if bar.fresh {
                bar.field = TextField::new(String::new());
                bar.fresh = false;
            }
            for c in first_line.chars().filter(|c| !c.is_control()) {
                bar.field.insert(c);
            }
            self.update_find();
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.is_empty() {
            return;
        }
        self.merge = None;
        self.replace_selection(EditKind::Other, &text);
        self.message = None;
        self.follow_cursor = true;
    }

    /// Ctrl+X: the selected text, now removed. `None` (and nothing changes)
    /// without a selection, as in Notepad.
    pub fn cut(&mut self) -> Option<String> {
        let (start, end) = self.selection()?;
        let text = self.text_between(start, end);
        self.merge = None;
        self.edit(EditKind::Other, start, end, "");
        self.follow_cursor = true;
        Some(text)
    }

    /// Carries out an editing command; the ones that need the app (see
    /// `Command`) are ignored here.
    pub fn apply(&mut self, command: Command) {
        self.message = None;
        self.follow_cursor = true;
        match command {
            Command::Insert(c) => {
                // Undo goes word by word: a letter after a space starts a
                // new step instead of joining the one before.
                let after_space = self
                    .undo
                    .last()
                    .and_then(|last| last.inserted.chars().last())
                    .is_some_and(char::is_whitespace);
                if after_space && !c.is_whitespace() {
                    self.merge = None;
                }
                self.replace_selection(EditKind::Typing, &c.to_string());
            }
            Command::Tab => {
                self.merge = None;
                self.replace_selection(EditKind::Other, &" ".repeat(TAB_WIDTH));
            }
            Command::Unindent => self.unindent(),
            Command::NewLine => {
                self.merge = None;
                self.replace_selection(EditKind::Other, "\n");
            }
            Command::Backspace => match self.selection() {
                Some((start, end)) => self.delete_range(start, end),
                None if self.cursor != Pos::default() => {
                    let start = self.step_left(self.cursor, false);
                    self.edit(EditKind::Backspace, start, self.cursor, "");
                }
                None => {}
            },
            Command::Delete => match self.selection() {
                Some((start, end)) => self.delete_range(start, end),
                None if self.cursor != self.doc_end() => {
                    let end = self.step_right(self.cursor);
                    self.edit(EditKind::Delete, self.cursor, end, "");
                }
                None => {}
            },
            Command::DeleteWordBack => match self.selection() {
                Some((start, end)) => self.delete_range(start, end),
                None => self.delete_range(self.word_left(self.cursor), self.cursor),
            },
            Command::DeleteWordForward => match self.selection() {
                Some((start, end)) => self.delete_range(start, end),
                None => self.delete_range(self.cursor, self.word_right(self.cursor)),
            },
            Command::Move { motion, select } => self.move_cursor(motion, select),
            Command::SelectAll => {
                self.merge = None;
                self.anchor = Some(Pos::default());
                self.cursor = self.doc_end();
                self.goal_x = None;
            }
            Command::Undo => self.undo(),
            Command::Redo => self.redo(),
            Command::Find => self.open_find(),
            Command::FindNext => self.find_next(true),
            Command::FindPrevious => self.find_next(false),
            Command::Copy
            | Command::Cut
            | Command::Paste
            | Command::Save
            | Command::Close
            | Command::ToggleFullScreen
            | Command::Help
            | Command::Rename
            | Command::Quit
            | Command::Menu(_) => {}
        }
    }

    fn delete_range(&mut self, start: Pos, end: Pos) {
        if start == end {
            return;
        }
        self.merge = None;
        self.edit(EditKind::Other, start, end, "");
    }

    /// Shift+Tab: takes up to `TAB_WIDTH` spaces off the start of the
    /// cursor's line, the cursor staying on the same text.
    fn unindent(&mut self) {
        self.merge = None;
        let line = self.cursor.line;
        let spaces = self.lines[line]
            .chars()
            .take(TAB_WIDTH)
            .take_while(|&c| c == ' ')
            .count();
        self.anchor = None;
        if spaces == 0 {
            return;
        }
        let col = self.cursor.col.saturating_sub(spaces);
        self.edit(
            EditKind::Other,
            Pos::new(line, 0),
            Pos::new(line, spaces),
            "",
        );
        self.cursor = Pos::new(line, col);
    }

    fn undo(&mut self) {
        self.merge = None;
        let Some(change) = self.undo.pop() else {
            return;
        };
        self.splice(
            change.start,
            end_of(change.start, &change.inserted),
            &change.removed,
        );
        self.cursor = change.cursor_before;
        self.anchor = change.anchor_before;
        self.goal_x = None;
        self.version = change.version_before;
        self.redo.push(change);
    }

    fn redo(&mut self) {
        self.merge = None;
        let Some(change) = self.redo.pop() else {
            return;
        };
        self.splice(
            change.start,
            end_of(change.start, &change.removed),
            &change.inserted,
        );
        self.cursor = end_of(change.start, &change.inserted);
        self.anchor = None;
        self.goal_x = None;
        self.version = change.version_after;
        self.undo.push(change);
    }

    // --- Movement ---------------------------------------------------------

    /// One character left, or to the end of the line above. With
    /// `whole_letter`, steps over combining marks too, so the cursor never
    /// lands between a letter and its accent.
    fn step_left(&self, pos: Pos, whole_letter: bool) -> Pos {
        if pos.col == 0 {
            if pos.line == 0 {
                return pos;
            }
            return Pos::new(pos.line - 1, self.line_len(pos.line - 1));
        }
        let layout = self.line_layout(pos.line);
        let chars = &layout.chars;
        let mut col = pos.col - 1;
        while whole_letter && col > 0 && is_combining(chars[col]) {
            col -= 1;
        }
        Pos::new(pos.line, col)
    }

    /// One character right (with any combining marks after it), or to the
    /// start of the next line.
    fn step_right(&self, pos: Pos) -> Pos {
        let layout = self.line_layout(pos.line);
        let chars = &layout.chars;
        if pos.col >= chars.len() {
            if pos.line + 1 >= self.lines.len() {
                return pos;
            }
            return Pos::new(pos.line + 1, 0);
        }
        let mut col = pos.col + 1;
        while col < chars.len() && is_combining(chars[col]) {
            col += 1;
        }
        Pos::new(pos.line, col)
    }

    /// Ctrl+Right, as in Windows: past the rest of this word (or run of
    /// symbols) and the spaces after it, to the start of the next one; at
    /// the end of a line, to the start of the next line.
    fn word_right(&self, pos: Pos) -> Pos {
        let layout = self.line_layout(pos.line);
        let chars = &layout.chars;
        if pos.col >= chars.len() {
            return self.step_right(pos);
        }
        let mut col = pos.col;
        let start_class = class(chars[col]);
        if start_class != CharClass::Space {
            while col < chars.len() && class(chars[col]) == start_class {
                col += 1;
            }
        }
        while col < chars.len() && class(chars[col]) == CharClass::Space {
            col += 1;
        }
        Pos::new(pos.line, col)
    }

    /// Ctrl+Left: back over spaces, then to the start of the word (or run
    /// of symbols) before them; at the start of a line, to the end of the
    /// line above.
    fn word_left(&self, pos: Pos) -> Pos {
        if pos.col == 0 {
            return self.step_left(pos, false);
        }
        let layout = self.line_layout(pos.line);
        let chars = &layout.chars;
        let mut col = pos.col;
        while col > 0 && class(chars[col - 1]) == CharClass::Space {
            col -= 1;
        }
        if col > 0 {
            let word_class = class(chars[col - 1]);
            while col > 0 && class(chars[col - 1]) == word_class {
                col -= 1;
            }
        }
        Pos::new(pos.line, col)
    }

    /// Up/Down/PgUp/PgDn: `delta` screen rows away (lines, unless wrapping),
    /// as near the remembered column as that row allows.
    fn row_target(&mut self, delta: isize) -> Pos {
        let (row, x) = self.visual(self.cursor);
        let goal = *self.goal_x.get_or_insert(x);
        let (line, row) = self.step_rows((self.cursor.line, row), delta);
        self.pos_in_row(line, row, goal)
    }

    fn move_cursor(&mut self, motion: Motion, select: bool) {
        self.merge = None;
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else if let Some((start, end)) = self.selection()
            && matches!(motion, Motion::Left | Motion::Right)
        {
            // Like Notepad: the first arrow only collapses the selection,
            // onto the side it points to.
            self.cursor = if motion == Motion::Left { start } else { end };
            self.anchor = None;
            self.goal_x = None;
            return;
        } else {
            self.anchor = None;
        }

        let page = self.view_rows.max(1) as isize;
        let line = self.cursor.line;
        let target = match motion {
            Motion::Left => self.step_left(self.cursor, true),
            Motion::Right => self.step_right(self.cursor),
            Motion::WordLeft => self.word_left(self.cursor),
            Motion::WordRight => self.word_right(self.cursor),
            // Home/End stay on the whole line even when it's wrapped, as in
            // Notepad with Word Wrap on.
            Motion::Home => Pos::new(line, 0),
            Motion::End => Pos::new(line, self.line_len(line)),
            Motion::DocStart => Pos::default(),
            Motion::DocEnd => self.doc_end(),
            Motion::Up => self.row_target(-1),
            Motion::Down => self.row_target(1),
            Motion::PageUp => {
                // The view moves along with the cursor, as in Notepad, so the
                // cursor keeps its place on screen.
                let top = self.step_rows((self.scroll_row, self.scroll_sub), -page);
                self.set_top(top);
                self.row_target(-page)
            }
            Motion::PageDown => {
                let top = self.step_rows((self.scroll_row, self.scroll_sub), page);
                self.set_top(top.min(self.max_top()));
                self.row_target(page)
            }
        };
        if !matches!(
            motion,
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown
        ) {
            self.goal_x = None;
        }
        self.cursor = target;
    }

    // --- Find -------------------------------------------------------------

    /// Ctrl+F: opens the find bar on the selected text (if it's within one
    /// line) or the last search, preselected so typing replaces it. Pressed
    /// again with the bar open, preselects its text again.
    fn open_find(&mut self) {
        if let Some(bar) = &mut self.find {
            bar.fresh = !bar.field.text().is_empty();
            return;
        }
        let selected = self.selected_text().filter(|text| !text.contains('\n'));
        let text = selected.unwrap_or_else(|| self.last_search.clone());
        let origin = self.selection().map_or(self.cursor, |(start, _)| start);
        self.find = Some(FindBar {
            fresh: !text.is_empty(),
            field: TextField::new(text),
            matches: Vec::new(),
            capped: false,
            current: None,
            origin,
        });
        self.update_find();
    }

    /// Closes the find bar. With `keep`, Enter's way: the match found stays
    /// selected, so typing replaces it. Without, Esc's: the search leaves
    /// nothing behind — no highlights, no selection — just the cursor at the
    /// start of the match it was on (or, with none, where Ctrl+F was
    /// pressed). Either way F3 can look for it again.
    fn close_find(&mut self, keep: bool) {
        let Some(bar) = self.find.take() else {
            return;
        };
        if !bar.field.text().is_empty() {
            self.last_search = bar.field.text().to_string();
        }
        if keep {
            return;
        }
        self.cursor = bar
            .current
            .map_or(bar.origin, |current| bar.matches[current].0);
        self.anchor = None;
        self.goal_x = None;
        self.follow_cursor = true;
    }

    pub fn find_key(&mut self, key: FindKey) {
        self.message = None;
        let Some(bar) = &mut self.find else {
            return;
        };
        // The first edit of preselected text replaces it; moving the cursor
        // keeps it.
        let fresh = std::mem::take(&mut bar.fresh);
        let changed = match key {
            FindKey::Insert(c) => {
                if fresh {
                    bar.field = TextField::new(String::new());
                }
                bar.field.insert(c);
                true
            }
            FindKey::Backspace if fresh => {
                bar.field = TextField::new(String::new());
                true
            }
            FindKey::Backspace => bar.field.backspace(),
            FindKey::Delete if fresh => {
                bar.field = TextField::new(String::new());
                true
            }
            FindKey::Delete => bar.field.delete(),
            FindKey::Left => {
                bar.field.move_left();
                false
            }
            FindKey::Right => {
                bar.field.move_right();
                false
            }
            FindKey::Home => {
                bar.field.move_home();
                false
            }
            FindKey::End => {
                bar.field.move_end();
                false
            }
            FindKey::Next | FindKey::Previous => {
                self.step_match(key == FindKey::Next);
                false
            }
            FindKey::Accept | FindKey::Close => {
                self.close_find(key == FindKey::Accept);
                false
            }
        };
        if changed {
            self.update_find();
        }
    }

    /// The text case-folded, line by line. Folded once and kept until the
    /// text changes, since the find bar searches it again for every letter
    /// typed (and the text can't change while it's open).
    fn folded_lines(&self) -> Rc<Vec<String>> {
        let mut cache = self.folded.borrow_mut();
        if let Some((generation, folded)) = cache.as_ref()
            && *generation == self.text_gen
        {
            return Rc::clone(folded);
        }
        let folded: Rc<Vec<String>> = Rc::new(
            self.lines
                .iter()
                .map(|line| line.chars().map(fold).collect())
                .collect(),
        );
        *cache = Some((self.text_gen, Rc::clone(&folded)));
        folded
    }

    /// Every (case-insensitive) occurrence of `query`, at most `MATCH_LIMIT`;
    /// the flag says whether there were more.
    fn find_all(&self, query: &str) -> (Vec<(Pos, Pos)>, bool) {
        let needle: String = query.chars().map(fold).collect();
        let mut matches = Vec::new();
        if needle.is_empty() {
            return (matches, false);
        }
        let needle_len = needle.chars().count();
        for (line, hay) in self.folded_lines().iter().enumerate() {
            // `fold` maps character to character, so counting characters in
            // the folded line gives columns in the real one.
            let (mut byte, mut col) = (0, 0);
            for (at, _) in hay.match_indices(needle.as_str()) {
                if matches.len() == MATCH_LIMIT {
                    return (matches, true);
                }
                col += hay[byte..at].chars().count();
                byte = at;
                matches.push((Pos::new(line, col), Pos::new(line, col + needle_len)));
            }
        }
        (matches, false)
    }

    /// After the search text changed: finds it again and selects the first
    /// match from where the search started, wrapping around to the top.
    fn update_find(&mut self) {
        let Some(query) = self.find.as_ref().map(|bar| bar.field.text().to_string()) else {
            return;
        };
        let (matches, capped) = self.find_all(&query);
        let Some(bar) = &mut self.find else {
            return;
        };
        let origin = bar.origin;
        bar.current = (!matches.is_empty()).then(|| {
            matches
                .iter()
                .position(|(start, _)| *start >= origin)
                .unwrap_or(0)
        });
        bar.matches = matches;
        bar.capped = capped;
        if !query.is_empty() {
            self.last_search = query;
        }
        match bar.current.map(|idx| bar.matches[idx]) {
            Some(found) => self.select_range(found),
            None => {
                self.cursor = origin;
                self.anchor = None;
                self.follow_cursor = true;
            }
        }
    }

    fn select_range(&mut self, (start, end): (Pos, Pos)) {
        self.anchor = Some(start);
        self.cursor = end;
        self.goal_x = None;
        self.merge = None;
        self.follow_cursor = true;
    }

    /// F3 (or Shift+F3) in the find bar: the next (or previous) match,
    /// wrapping around.
    fn step_match(&mut self, forward: bool) {
        let Some(bar) = &mut self.find else {
            return;
        };
        let count = bar.matches.len();
        let Some(current) = bar.current else {
            return;
        };
        let next = if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        bar.current = Some(next);
        let found = bar.matches[next];
        self.select_range(found);
    }

    /// What was searched for last (what F3 looks for with the bar closed).
    pub fn last_search(&self) -> &str {
        &self.last_search
    }

    /// Carries a search over from elsewhere — the quick view's find, from
    /// one previewed file to the next.
    pub fn set_last_search(&mut self, query: &str) {
        self.last_search = query.to_string();
    }

    /// Whether F3 has anything to do: the bar is open, or there's an
    /// earlier search to look for again.
    pub fn can_find_next(&self) -> bool {
        self.find.is_some() || !self.last_search.is_empty()
    }

    /// F3/Shift+F3: with the bar open, its next/previous match; otherwise
    /// the last search again from the cursor, wrapping around the end. With
    /// nothing searched for yet it does nothing: only Ctrl+F opens the bar.
    fn find_next(&mut self, forward: bool) {
        if self.find.is_some() {
            self.step_match(forward);
            return;
        }
        if self.last_search.is_empty() {
            return;
        }
        let (matches, _) = self.find_all(&self.last_search);
        let (from, to) = self.selection().unwrap_or((self.cursor, self.cursor));
        let found = if forward {
            matches
                .iter()
                .find(|(start, _)| *start >= to)
                .or(matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|(start, _)| *start < from)
                .or(matches.last())
        };
        match found {
            Some(&found) => self.select_range(found),
            None => self.message = Some(format!("\"{}\" not found", self.last_search)),
        }
    }
}

/// Combining accents and other zero-width marks, which belong with the
/// letter before them.
fn is_combining(c: char) -> bool {
    char_width(c, 0) == 0
}

/// Folds a new edit into the previous undo step when it simply continues it:
/// more letters typed right after it, more Backspaces before it, or more
/// Deletes at the same spot.
fn merge_into(
    last: &mut Change,
    kind: EditKind,
    start: Pos,
    end: Pos,
    removed: &str,
    inserted: &str,
) -> bool {
    match kind {
        EditKind::Typing if start == end && start == end_of(last.start, &last.inserted) => {
            last.inserted.push_str(inserted);
            true
        }
        EditKind::Backspace if last.inserted.is_empty() && end == last.start => {
            last.start = start;
            last.removed.insert_str(0, removed);
            true
        }
        EditKind::Delete if last.inserted.is_empty() && start == last.start => {
            last.removed.push_str(removed);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str) -> Editor {
        Editor::from_bytes(PathBuf::from("test.txt"), text.as_bytes()).unwrap()
    }

    fn col_at_x_line(line: &str, x: usize) -> usize {
        let chars: Vec<char> = line.chars().collect();
        col_in_row(&chars, (0, chars.len()), (0, 0), x, true)
    }

    fn text(editor: &Editor) -> String {
        String::from_utf8(editor.to_bytes()).unwrap()
    }

    fn key(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) {
        if let Some(command) = command_for_key(code, modifiers) {
            editor.apply(command);
        }
    }

    fn press(editor: &mut Editor, code: KeyCode) {
        key(editor, code, KeyModifiers::NONE);
    }

    fn type_str(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            key(editor, KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    fn ctrl(editor: &mut Editor, code: KeyCode) {
        key(editor, code, KeyModifiers::CONTROL);
    }

    fn shift(editor: &mut Editor, code: KeyCode) {
        key(editor, code, KeyModifiers::SHIFT);
    }

    #[test]
    fn putting_the_text_back_by_hand_counts_as_unmodified() {
        // A letter typed and deleted again: no undo, same text as on disk.
        let mut ed = editor("abc\n");
        type_str(&mut ed, "x");
        assert!(ed.is_modified());
        press(&mut ed, KeyCode::Backspace);
        assert!(!ed.is_modified());
        // A Tab and Ctrl+Z, the accidental-Tab case.
        press(&mut ed, KeyCode::Tab);
        assert!(ed.is_modified());
        ctrl(&mut ed, KeyCode::Char('z'));
        assert!(!ed.is_modified());
        // A whole line added and removed again.
        press(&mut ed, KeyCode::Enter);
        assert!(ed.is_modified());
        press(&mut ed, KeyCode::Backspace);
        assert!(!ed.is_modified());

        // After a save, the saved text is the one compared with.
        type_str(&mut ed, "y");
        ed.mark_saved();
        assert!(!ed.is_modified());
        press(&mut ed, KeyCode::Backspace);
        assert!(ed.is_modified(), "differs from what was just saved");
        type_str(&mut ed, "y");
        assert!(!ed.is_modified());
    }

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut ed = editor("ac\n");
        press(&mut ed, KeyCode::Right);
        type_str(&mut ed, "b ą");
        assert_eq!(text(&ed), "ab ąc\n");
        assert_eq!(ed.cursor(), Pos::new(0, 4));
        assert!(ed.is_modified());
    }

    #[test]
    fn enter_splits_the_line_without_indenting() {
        let mut ed = editor("    foo bar");
        ed.cursor = Pos::new(0, 7);
        press(&mut ed, KeyCode::Enter);
        assert_eq!(ed.lines(), ["    foo", " bar"]);
        assert_eq!(ed.cursor(), Pos::new(1, 0));
    }

    #[test]
    fn backspace_and_delete_join_lines_at_the_ends() {
        let mut ed = editor("ab\ncd");
        ed.cursor = Pos::new(1, 0);
        press(&mut ed, KeyCode::Backspace);
        assert_eq!(ed.lines(), ["abcd"]);
        assert_eq!(ed.cursor(), Pos::new(0, 2));

        let mut ed = editor("ab\ncd");
        ed.cursor = Pos::new(0, 2);
        press(&mut ed, KeyCode::Delete);
        assert_eq!(ed.lines(), ["abcd"]);
        assert_eq!(ed.cursor(), Pos::new(0, 2));
    }

    #[test]
    fn backspace_and_delete_do_nothing_at_the_document_edges() {
        let mut ed = editor("ab");
        press(&mut ed, KeyCode::Backspace);
        ed.cursor = Pos::new(0, 2);
        press(&mut ed, KeyCode::Delete);
        assert_eq!(text(&ed), "ab");
        assert!(!ed.is_modified());
    }

    #[test]
    fn multibyte_letters_are_single_steps() {
        let mut ed = editor("zażółć");
        ed.cursor = Pos::new(0, 6);
        press(&mut ed, KeyCode::Backspace);
        press(&mut ed, KeyCode::Left);
        press(&mut ed, KeyCode::Delete);
        assert_eq!(text(&ed), "zażó");
    }

    #[test]
    fn tab_inserts_four_spaces_and_shift_tab_removes_them() {
        let mut ed = editor("x");
        press(&mut ed, KeyCode::Tab);
        assert_eq!(text(&ed), "    x");
        assert_eq!(ed.cursor(), Pos::new(0, 4));
        ed.cursor = Pos::new(0, 5);
        press(&mut ed, KeyCode::BackTab);
        assert_eq!(text(&ed), "x");
        assert_eq!(ed.cursor(), Pos::new(0, 1));
        press(&mut ed, KeyCode::BackTab);
        assert_eq!(text(&ed), "x");
    }

    #[test]
    fn ctrl_right_jumps_to_the_start_of_the_next_word() {
        let mut ed = editor("zażółć gęślą, jaźń_2 ok\nnext");
        let mut stops = Vec::new();
        for _ in 0..7 {
            ctrl(&mut ed, KeyCode::Right);
            stops.push(ed.cursor());
        }
        assert_eq!(
            stops,
            [
                Pos::new(0, 7),
                Pos::new(0, 12),
                Pos::new(0, 14),
                Pos::new(0, 21),
                Pos::new(0, 23),
                Pos::new(1, 0),
                Pos::new(1, 4),
            ]
        );
    }

    #[test]
    fn ctrl_left_jumps_to_the_start_of_the_previous_word() {
        let mut ed = editor("first\nzażółć gęślą, jaźń");
        ed.cursor = ed.doc_end();
        let mut stops = Vec::new();
        for _ in 0..6 {
            ctrl(&mut ed, KeyCode::Left);
            stops.push(ed.cursor());
        }
        assert_eq!(
            stops,
            [
                Pos::new(1, 14),
                Pos::new(1, 12),
                Pos::new(1, 7),
                Pos::new(1, 0),
                Pos::new(0, 5),
                Pos::new(0, 0),
            ]
        );
    }

    #[test]
    fn ctrl_backspace_and_ctrl_delete_remove_a_word() {
        let mut ed = editor("jeden dwa trzy");
        ed.cursor = Pos::new(0, 9);
        ctrl(&mut ed, KeyCode::Backspace);
        assert_eq!(text(&ed), "jeden  trzy");
        ctrl(&mut ed, KeyCode::Delete);
        assert_eq!(text(&ed), "jeden trzy");
        // ^H is no shortcut: some terminals send it for a plain Backspace.
        assert_eq!(
            command_for_key(KeyCode::Char('h'), KeyModifiers::CONTROL),
            None
        );
    }

    #[test]
    fn shift_extends_the_selection_and_typing_replaces_it() {
        let mut ed = editor("hello world");
        ed.cursor = Pos::new(0, 6);
        key(
            &mut ed,
            KeyCode::Right,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(ed.selected_text().as_deref(), Some("world"));
        type_str(&mut ed, "świecie");
        assert_eq!(text(&ed), "hello świecie");
        assert_eq!(ed.selection(), None);
    }

    #[test]
    fn selection_spans_lines_and_shift_home_end_work() {
        let mut ed = editor("one\ntwo\nthree");
        ed.cursor = Pos::new(0, 1);
        shift(&mut ed, KeyCode::Down);
        shift(&mut ed, KeyCode::End);
        assert_eq!(ed.selected_text().as_deref(), Some("ne\ntwo"));
        shift(&mut ed, KeyCode::Home);
        assert_eq!(ed.selected_text().as_deref(), Some("ne\n"));
        press(&mut ed, KeyCode::Delete);
        assert_eq!(text(&ed), "otwo\nthree");
    }

    #[test]
    fn ctrl_shift_end_selects_to_the_end_of_the_document() {
        let mut ed = editor("a\nb\nc");
        ed.cursor = Pos::new(1, 0);
        key(
            &mut ed,
            KeyCode::End,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(ed.selected_text().as_deref(), Some("b\nc"));
        key(
            &mut ed,
            KeyCode::Home,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        assert_eq!(ed.selected_text().as_deref(), Some("a\n"));
    }

    #[test]
    fn arrows_collapse_a_selection_to_its_side() {
        let mut ed = editor("abcdef");
        ed.cursor = Pos::new(0, 2);
        shift(&mut ed, KeyCode::Right);
        shift(&mut ed, KeyCode::Right);
        press(&mut ed, KeyCode::Left);
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(0, 2), None));
        shift(&mut ed, KeyCode::Right);
        shift(&mut ed, KeyCode::Right);
        press(&mut ed, KeyCode::Right);
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(0, 4), None));
        // Other movement just drops it and moves on from the cursor.
        shift(&mut ed, KeyCode::Left);
        press(&mut ed, KeyCode::End);
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(0, 6), None));
    }

    #[test]
    fn select_all_then_backspace_empties_the_file() {
        let mut ed = editor("a\nb\n");
        ctrl(&mut ed, KeyCode::Char('a'));
        assert_eq!(ed.selected_text().as_deref(), Some("a\nb"));
        press(&mut ed, KeyCode::Backspace);
        assert_eq!(ed.lines(), [""]);
        // The final newline is the file's, kept like its line endings.
        assert_eq!(text(&ed), "\n");
    }

    #[test]
    fn cut_and_paste_move_text() {
        let mut ed = editor("abc\ndef");
        ed.cursor = Pos::new(0, 1);
        shift(&mut ed, KeyCode::Down);
        let cut = ed.cut().unwrap();
        assert_eq!(cut, "bc\nd");
        assert_eq!(text(&ed), "aef");
        ed.cursor = ed.doc_end();
        ed.insert_text("X\r\nY\rZ");
        assert_eq!(ed.lines(), ["aefX", "Y", "Z"]);
        assert_eq!(ed.cursor(), Pos::new(2, 1));
        assert_eq!(ed.cut(), None);
    }

    #[test]
    fn up_and_down_keep_the_column_across_short_lines() {
        let mut ed = editor("abcdef\nab\nabcdef");
        ed.cursor = Pos::new(0, 5);
        press(&mut ed, KeyCode::Down);
        assert_eq!(ed.cursor(), Pos::new(1, 2));
        press(&mut ed, KeyCode::Down);
        assert_eq!(ed.cursor(), Pos::new(2, 5));
        press(&mut ed, KeyCode::Down);
        assert_eq!(ed.cursor(), Pos::new(2, 5));
    }

    #[test]
    fn page_down_moves_by_the_visible_height() {
        let mut ed = editor(&"x\n".repeat(50));
        ed.scroll_into_view(10, 80);
        press(&mut ed, KeyCode::PageDown);
        assert_eq!(ed.cursor().line, 10);
        assert_eq!(ed.scroll_row, 10);
        press(&mut ed, KeyCode::PageUp);
        assert_eq!(ed.cursor().line, 0);
        ctrl(&mut ed, KeyCode::End);
        ed.scroll_into_view(10, 80);
        assert_eq!(ed.scroll_row, 40);
    }

    #[test]
    fn undo_goes_back_word_by_word_and_redo_returns() {
        let mut ed = editor("");
        type_str(&mut ed, "ala ma");
        press(&mut ed, KeyCode::Enter);
        type_str(&mut ed, "kota");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "ala ma\n");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "ala ma");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "ala ");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "");
        assert!(!ed.is_modified(), "back to the text as it was opened");
        ctrl(&mut ed, KeyCode::Char('y'));
        ctrl(&mut ed, KeyCode::Char('y'));
        assert_eq!(text(&ed), "ala ma");
        assert_eq!(ed.cursor(), Pos::new(0, 6));
        assert!(ed.is_modified());
    }

    #[test]
    fn held_backspace_and_delete_undo_in_one_step() {
        let mut ed = editor("abcdef");
        ed.cursor = Pos::new(0, 4);
        for _ in 0..3 {
            press(&mut ed, KeyCode::Backspace);
        }
        for _ in 0..2 {
            press(&mut ed, KeyCode::Delete);
        }
        assert_eq!(text(&ed), "a");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "aef");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "abcdef");
        assert_eq!(ed.cursor(), Pos::new(0, 4));
    }

    #[test]
    fn undo_restores_a_replaced_selection() {
        let mut ed = editor("one two");
        ctrl(&mut ed, KeyCode::Char('a'));
        type_str(&mut ed, "x");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "one two");
        assert_eq!(ed.selected_text().as_deref(), Some("one two"));
    }

    #[test]
    fn saving_marks_the_text_unmodified() {
        let mut ed = editor("a");
        type_str(&mut ed, "b");
        ed.mark_saved();
        assert!(!ed.is_modified());
        ctrl(&mut ed, KeyCode::Char('z'));
        assert!(ed.is_modified(), "undo past the save differs from the disk");
    }

    #[test]
    fn undo_after_typing_on_past_a_save_stops_at_the_saved_text() {
        let mut ed = editor("");
        type_str(&mut ed, "abc");
        ed.mark_saved();
        type_str(&mut ed, "d");
        ctrl(&mut ed, KeyCode::Char('z'));
        assert_eq!(text(&ed), "abc");
        assert!(!ed.is_modified());
    }

    #[test]
    fn f2_renames_rather_than_saves() {
        assert_eq!(
            command_for_key(KeyCode::F(2), KeyModifiers::NONE),
            Some(Command::Rename)
        );
    }

    #[test]
    fn line_endings_and_final_newline_round_trip() {
        for original in [
            "",
            "\n",
            "one",
            "one\n",
            "one\ntwo\n",
            "one\r\ntwo\r\n",
            "one\r\ntwo",
            "mixed\r\nlines\nhere\n",
            "\u{feff}bom\r\n",
            "\n\n\n",
            "tab\there\n",
        ] {
            let ed = editor(original);
            assert_eq!(text(&ed), original, "{original:?}");
        }
    }

    #[test]
    fn edits_keep_crlf_line_endings() {
        let mut ed = editor("a\r\nb\r\n");
        assert_eq!(ed.lines(), ["a", "b"]);
        ed.cursor = Pos::new(0, 1);
        press(&mut ed, KeyCode::Enter);
        type_str(&mut ed, "x");
        assert_eq!(text(&ed), "a\r\nx\r\nb\r\n");
    }

    #[test]
    fn refuses_binary_and_non_utf8_files() {
        let path = PathBuf::from("x");
        assert!(Editor::from_bytes(path.clone(), b"ab\0cd").is_err());
        assert!(Editor::from_bytes(path.clone(), &[0x66, 0xff, 0x66]).is_err());
        assert!(Editor::from_bytes(path, "zażółć".as_bytes()).is_ok());
    }

    #[test]
    fn open_refuses_files_over_the_size_limit() {
        let dir = std::env::temp_dir().join(format!("pc_test_editor_big_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.txt");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        let err = Editor::open(&path).err().unwrap();
        assert!(err.contains("10 MB"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tabs_display_to_the_next_stop() {
        assert_eq!(x_of("\tx", 1), 4);
        assert_eq!(x_of("ab\tx", 3), 4);
        assert_eq!(x_of("abcd\tx", 5), 8);
        assert_eq!(x_of("ą\u{65e5}x", 2), 3);
        // A click in a tab's right half lands after it.
        assert_eq!(col_at_x_line("ab\tx", 2), 2);
        assert_eq!(col_at_x_line("ab\tx", 3), 3);
        assert_eq!(col_at_x_line("ab\tx", 4), 3);
        assert_eq!(col_at_x_line("ab\tx", 99), 4);
    }

    #[test]
    fn down_through_a_tab_keeps_the_screen_column() {
        let mut ed = editor("abcdefgh\n\tx");
        ed.cursor = Pos::new(0, 5);
        press(&mut ed, KeyCode::Down);
        // Column 5 on screen is just after the tab's 4 cells and on "x".
        assert_eq!(ed.cursor(), Pos::new(1, 2));
        assert_eq!(ed.cursor_screen().2, 5);
    }

    #[test]
    fn horizontal_scroll_follows_the_cursor() {
        let mut ed = editor(&"x".repeat(100));
        press(&mut ed, KeyCode::End);
        ed.scroll_into_view(5, 20);
        assert_eq!((ed.scroll_row, ed.scroll_x), (0, 81));
        press(&mut ed, KeyCode::Home);
        ed.scroll_into_view(5, 20);
        assert_eq!((ed.scroll_row, ed.scroll_x), (0, 0));
    }

    #[test]
    fn wheel_scrolls_the_view_but_a_key_brings_the_cursor_back() {
        let mut ed = editor(&"x\n".repeat(100));
        ed.scroll_into_view(10, 80);
        ed.scroll_view(30);
        ed.scroll_into_view(10, 80);
        assert_eq!(ed.scroll_row, 30);
        ed.scroll_view(1000);
        ed.scroll_into_view(10, 80);
        assert_eq!(ed.scroll_row, 90);
        press(&mut ed, KeyCode::Right);
        ed.scroll_into_view(10, 80);
        assert_eq!(ed.scroll_row, 0);
    }

    #[test]
    fn click_places_the_cursor_and_drag_selects() {
        let mut ed = editor("hello\nworld");
        ed.click(1, 2, false);
        assert_eq!(ed.cursor(), Pos::new(1, 2));
        ed.click(0, 1, true);
        assert_eq!(ed.selected_text().as_deref(), Some("ello\nwo"));
        ed.click(5, 50, false);
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(1, 5), None));
    }

    #[test]
    fn combining_marks_move_with_their_letter() {
        // "e" + combining ogonek, then "x".
        let mut ed = editor("e\u{328}x");
        press(&mut ed, KeyCode::Right);
        assert_eq!(ed.cursor(), Pos::new(0, 2));
        press(&mut ed, KeyCode::Left);
        assert_eq!(ed.cursor(), Pos::new(0, 0));
    }

    #[test]
    fn altgr_polish_letters_are_typed_in_every_disguise() {
        let altgr_reports = [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::ALT,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        ];
        for letter in "ąęćłńóśźżĄĘĆŁŃÓŚŹŻ".chars() {
            for modifiers in altgr_reports {
                assert_eq!(
                    command_for_key(KeyCode::Char(letter), modifiers),
                    Some(Command::Insert(letter)),
                    "{letter} with {modifiers:?}"
                );
            }
        }
    }

    #[test]
    fn ctrl_letters_are_shortcuts_never_text() {
        assert_eq!(
            command_for_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            Some(Command::Save)
        );
        assert_eq!(
            command_for_key(
                KeyCode::Char('Z'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ),
            Some(Command::Undo)
        );
        assert_eq!(
            command_for_key(KeyCode::Char('k'), KeyModifiers::CONTROL),
            None
        );
        // Alt+ASCII letter opens the menu, as outside the editor.
        assert_eq!(
            command_for_key(KeyCode::Char('f'), KeyModifiers::ALT),
            Some(Command::Menu(Some('f')))
        );
        assert_eq!(
            command_for_key(KeyCode::F(9), KeyModifiers::NONE),
            Some(Command::Menu(None))
        );
        // Windows AltGr: Ctrl+Alt with an ASCII symbol, like AltGr+Q = @.
        assert_eq!(
            command_for_key(
                KeyCode::Char('@'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            Some(Command::Insert('@'))
        );
        assert!(!is_text_input('ą', KeyModifiers::CONTROL));
        assert!(!is_text_input('a', KeyModifiers::SUPER));
    }

    #[test]
    fn control_characters_show_as_pictures() {
        assert_eq!(display_glyph('\r'), '\u{240d}');
        assert_eq!(display_glyph('\u{1b}'), '\u{241b}');
        assert_eq!(display_glyph('ą'), 'ą');
        for mark in ['\u{202e}', '\u{2066}', '\u{2069}', '\u{200f}'] {
            assert_eq!(display_glyph(mark), '\u{fffd}', "{mark:?}");
            assert_eq!(char_width(mark, 0), 1, "{mark:?}");
        }
    }

    fn wrapped(text: &str, cols: usize) -> Editor {
        let mut ed = editor(text);
        ed.set_wrap(true);
        ed.scroll_into_view(10, cols);
        ed
    }

    fn row_texts(ed: &Editor) -> Vec<String> {
        ed.screen_rows(100)
            .iter()
            .map(|row| {
                ed.lines()[row.line]
                    .chars()
                    .skip(row.start)
                    .take(row.end - row.start)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn wrapping_breaks_after_spaces() {
        // 11 columns: rows of up to 10 cells, a space may hang into the 11th.
        let ed = wrapped("ala ma kota i psa\nkrótka", 11);
        assert_eq!(row_texts(&ed), ["ala ma ", "kota i psa", "krótka"]);
    }

    #[test]
    fn a_word_longer_than_the_row_is_cut() {
        let ed = wrapped("abcdefghijklmnop", 6);
        assert_eq!(row_texts(&ed), ["abcde", "fghij", "klmno", "p"]);
        assert_eq!(row_starts("", Some(5)), [0]);
        // Wide letters never straddle the edge.
        assert_eq!(row_starts("日本語日本", Some(5)), [0, 2, 4]);
    }

    #[test]
    fn up_and_down_move_by_screen_row_when_wrapping() {
        let mut ed = wrapped("abcdefghijklmno\nxy", 6);
        ed.cursor = Pos::new(0, 2);
        press(&mut ed, KeyCode::Down);
        assert_eq!(ed.cursor(), Pos::new(0, 7));
        assert_eq!(ed.cursor_screen(), (0, 1, 2));
        press(&mut ed, KeyCode::Down);
        assert_eq!(ed.cursor(), Pos::new(0, 12));
        press(&mut ed, KeyCode::Down);
        assert_eq!(ed.cursor(), Pos::new(1, 2));
        press(&mut ed, KeyCode::Up);
        press(&mut ed, KeyCode::Up);
        assert_eq!(ed.cursor(), Pos::new(0, 7));
        // Home and End still take the whole line.
        press(&mut ed, KeyCode::End);
        assert_eq!(ed.cursor(), Pos::new(0, 15));
        assert_eq!(ed.cursor_screen(), (0, 2, 5));
        press(&mut ed, KeyCode::Home);
        assert_eq!(ed.cursor(), Pos::new(0, 0));
    }

    #[test]
    fn down_onto_a_shorter_wrapped_row_stays_on_it() {
        // Rows "abc " / "defgh"; from x=4 on row 1, Up lands on row 0's last
        // character, not on the start of row 1 again.
        let mut ed = wrapped("abc defgh", 6);
        ed.cursor = Pos::new(0, 8);
        press(&mut ed, KeyCode::Up);
        assert_eq!(ed.cursor(), Pos::new(0, 3));
        assert_eq!(ed.cursor_screen().1, 0);
    }

    #[test]
    fn wrapped_view_scrolls_by_screen_rows() {
        let mut ed = editor(&"abcdefghij".repeat(10));
        ed.set_wrap(true);
        ed.scroll_into_view(3, 11);
        assert_eq!(ed.screen_rows(100).len(), 10);
        ctrl(&mut ed, KeyCode::End);
        ed.scroll_into_view(3, 11);
        // Ten rows of ten; the cursor after the last one stays on it, in the
        // cell kept free for it.
        assert_eq!(ed.cursor_screen(), (0, 9, 10));
        assert_eq!((ed.scroll_row, ed.scroll_sub), (0, 7));
        ed.scroll_view(-100);
        ed.scroll_into_view(3, 11);
        assert_eq!((ed.scroll_row, ed.scroll_sub), (0, 0));
        ed.scroll_view(3);
        assert_eq!((ed.scroll_row, ed.scroll_sub), (0, 3));
        ed.click(1, 2, false);
        assert_eq!(ed.cursor(), Pos::new(0, 42));
        ed.scroll_into_view(3, 11);
        assert_eq!((ed.scroll_row, ed.scroll_sub), (0, 3));
    }

    #[test]
    fn turning_wrap_off_scrolls_sideways_again() {
        let mut ed = wrapped(&"x".repeat(50), 11);
        press(&mut ed, KeyCode::End);
        ed.scroll_into_view(5, 11);
        assert_eq!(ed.scroll_x, 0);
        ed.set_wrap(false);
        ed.scroll_into_view(5, 11);
        assert_eq!(ed.screen_rows(10).len(), 1);
        assert_eq!(ed.scroll_x, 40);
    }

    fn find_type(ed: &mut Editor, text: &str) {
        for c in text.chars() {
            ed.find_key(FindKey::Insert(c));
        }
    }

    fn status(ed: &Editor) -> String {
        ed.find.as_ref().unwrap().status()
    }

    #[test]
    fn find_is_incremental_and_ignores_case_in_polish() {
        let mut ed = editor("Żółw i ŻÓŁW\nżółwik");
        ctrl(&mut ed, KeyCode::Char('f'));
        assert_eq!(status(&ed), "");
        find_type(&mut ed, "ż");
        assert_eq!(status(&ed), "1/3");
        find_type(&mut ed, "ółw");
        assert_eq!(status(&ed), "1/3");
        assert_eq!(ed.selected_text().as_deref(), Some("Żółw"));
        find_type(&mut ed, "i");
        assert_eq!(status(&ed), "1/1");
        assert_eq!(ed.selection(), Some((Pos::new(1, 0), Pos::new(1, 5))));
        find_type(&mut ed, "x");
        assert_eq!(status(&ed), "No matches");
        assert_eq!(ed.selection(), None);
        ed.find_key(FindKey::Backspace);
        assert_eq!(status(&ed), "1/1");
    }

    #[test]
    fn find_starts_at_the_cursor_and_wraps_around() {
        let mut ed = editor("ab ab\nab");
        ed.cursor = Pos::new(0, 1);
        ctrl(&mut ed, KeyCode::Char('f'));
        find_type(&mut ed, "AB");
        assert_eq!(status(&ed), "2/3");
        ed.find_key(FindKey::Next);
        assert_eq!(status(&ed), "3/3");
        ed.find_key(FindKey::Next);
        assert_eq!(status(&ed), "1/3");
        assert_eq!(ed.cursor(), Pos::new(0, 2));
        ed.find_key(FindKey::Previous);
        assert_eq!(status(&ed), "3/3");
        let bar_key = |code, modifiers| find_bar_key(code, modifiers);
        assert_eq!(
            bar_key(KeyCode::F(3), KeyModifiers::NONE),
            Some(FindKey::Next)
        );
        assert_eq!(
            bar_key(KeyCode::F(3), KeyModifiers::SHIFT),
            Some(FindKey::Previous)
        );
        assert_eq!(
            bar_key(KeyCode::Enter, KeyModifiers::NONE),
            Some(FindKey::Accept)
        );
        assert_eq!(bar_key(KeyCode::Up, KeyModifiers::NONE), None);
        assert_eq!(bar_key(KeyCode::Down, KeyModifiers::NONE), None);
    }

    #[test]
    fn enter_keeps_the_match_selected_and_esc_forgets_it() {
        let mut ed = editor("kot pies kot");
        ed.cursor = Pos::new(0, 5);
        ctrl(&mut ed, KeyCode::Char('f'));
        find_type(&mut ed, "kot");
        ed.find_key(FindKey::Accept);
        assert!(ed.find.is_none());
        assert_eq!(ed.selection(), Some((Pos::new(0, 9), Pos::new(0, 12))));

        ctrl(&mut ed, KeyCode::Char('f'));
        ed.find_key(FindKey::Next);
        assert_eq!(ed.selection(), Some((Pos::new(0, 0), Pos::new(0, 3))));
        ed.find_key(FindKey::Close);
        assert!(ed.find.is_none());
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(0, 0), None));
        // F3 and Shift+F3 still look for it, from the cursor.
        press(&mut ed, KeyCode::F(3));
        assert_eq!(ed.selection(), Some((Pos::new(0, 0), Pos::new(0, 3))));
        press(&mut ed, KeyCode::F(3));
        assert_eq!(ed.selection(), Some((Pos::new(0, 9), Pos::new(0, 12))));
        shift(&mut ed, KeyCode::F(3));
        assert_eq!(ed.selection(), Some((Pos::new(0, 0), Pos::new(0, 3))));
        assert!(ed.find.is_none(), "F3 never opens the bar");

        // With no match, Esc puts the cursor back where Ctrl+F was pressed.
        ed.cursor = Pos::new(0, 2);
        ed.anchor = None;
        ctrl(&mut ed, KeyCode::Char('f'));
        find_type(&mut ed, "pies");
        find_type(&mut ed, "x");
        ed.find_key(FindKey::Close);
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(0, 2), None));
        // Reopened, the last search is offered again, ready to be replaced.
        ctrl(&mut ed, KeyCode::Char('f'));
        let bar = ed.find.as_ref().unwrap();
        assert_eq!((bar.field.text(), bar.fresh), ("piesx", true));
    }

    #[test]
    fn deselect_lets_go_of_the_selection_where_the_cursor_is() {
        let mut ed = editor("kot pies kot");
        ctrl(&mut ed, KeyCode::Char('f'));
        find_type(&mut ed, "pies");
        ed.find_key(FindKey::Close);
        press(&mut ed, KeyCode::F(3));
        assert_eq!(ed.selection(), Some((Pos::new(0, 4), Pos::new(0, 8))));
        ed.deselect();
        assert_eq!((ed.cursor(), ed.selection()), (Pos::new(0, 8), None));
        assert_eq!(text(&ed), "kot pies kot");
    }

    #[test]
    fn f3_with_nothing_searched_does_nothing() {
        let mut ed = editor("text");
        press(&mut ed, KeyCode::F(3));
        shift(&mut ed, KeyCode::F(3));
        assert!(ed.find.is_none());
        assert_eq!((ed.cursor(), ed.selection()), (Pos::default(), None));
    }

    #[test]
    fn reopened_find_starts_from_the_selection_and_typing_replaces_it() {
        let mut ed = editor("alpha beta");
        ed.cursor = Pos::new(0, 6);
        key(
            &mut ed,
            KeyCode::Right,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        ctrl(&mut ed, KeyCode::Char('f'));
        let bar = ed.find.as_ref().unwrap();
        assert_eq!((bar.field.text(), bar.fresh), ("beta", true));
        assert_eq!(status(&ed), "1/1");
        find_type(&mut ed, "al");
        assert_eq!(ed.find.as_ref().unwrap().field.text(), "al");
        assert_eq!(status(&ed), "1/1");
        // A paste goes into the bar too, not into the text.
        ed.insert_text("pha\nignored");
        assert_eq!(ed.find.as_ref().unwrap().field.text(), "alpha");
        assert_eq!(text(&ed), "alpha beta");
    }

    #[test]
    fn find_highlights_every_match_on_a_line() {
        let mut ed = editor("aXa\naaa");
        ctrl(&mut ed, KeyCode::Char('f'));
        find_type(&mut ed, "a");
        let bar = ed.find.as_ref().unwrap();
        assert_eq!(bar.matches_on(0).len(), 2);
        assert_eq!(bar.matches_on(1).len(), 3);
        assert_eq!(bar.matches_on(2).len(), 0);
    }

    /// A small deterministic random number generator, for the tests below.
    fn lcg(seed: &mut u64) -> usize {
        *seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (*seed >> 33) as usize
    }

    #[test]
    fn edited_layouts_match_ones_worked_out_afresh() {
        let alphabet: Vec<char> = "ab  \tż日\u{328}x".chars().collect();
        let mut seed = 7;
        for wrap in [None, Some(1), Some(3), Some(6), Some(11)] {
            let mut line: String = (0..3000)
                .map(|_| alphabet[lcg(&mut seed) % alphabet.len()])
                .collect();
            let mut layout = LineLayout::new(&line, wrap);
            for _ in 0..300 {
                let len = line.chars().count();
                let from = lcg(&mut seed) % (len + 1);
                let to = (from + lcg(&mut seed) % 4).min(len);
                let text: String = (0..lcg(&mut seed) % 4)
                    .map(|_| alphabet[lcg(&mut seed) % alphabet.len()])
                    .collect();
                let mut chars: Vec<char> = line.chars().collect();
                chars.splice(from..to, text.chars());
                line = chars.into_iter().collect();
                layout.edit(from, to, &text, wrap);
                let fresh = LineLayout::new(&line, wrap);
                assert_eq!(layout.chars, fresh.chars);
                assert_eq!(layout.starts, fresh.starts, "{wrap:?}");
                assert_eq!(layout.checkpoints, fresh.checkpoints, "{wrap:?}");
            }
        }
    }

    #[test]
    fn far_along_a_long_line_columns_are_found_from_checkpoints() {
        let line = "ab\tż日\u{328}x ".repeat(1000);
        let layout = LineLayout::new(&line, None);
        assert!(!layout.checkpoints.is_empty());
        let chars: Vec<char> = line.chars().collect();
        for col in [0, 1, 1023, 1024, 1025, 5000, chars.len()] {
            let x = width_of(&chars, 0, col);
            assert_eq!(layout.x_of(0, col), x, "{col}");
            let (from, from_x) = layout.seek(0, x);
            assert!(from <= col && from_x == width_of(&chars, 0, from), "{col}");
        }
    }

    #[test]
    fn typing_into_a_wrapped_line_keeps_its_rows_right() {
        let mut ed = wrapped(&"slowo ".repeat(50), 20);
        ed.cursor = Pos::new(0, 40);
        type_str(&mut ed, "długie_słowo_bez_spacji");
        let fresh = row_starts(&ed.lines[0], ed.wrap_width());
        assert_eq!(ed.line_layout(0).starts, fresh);
        ctrl(&mut ed, KeyCode::Char('z'));
        let fresh = row_starts(&ed.lines[0], ed.wrap_width());
        assert_eq!(ed.line_layout(0).starts, fresh);
    }

    #[test]
    fn find_matches_do_not_overlap() {
        let ed = editor("aaaa");
        assert_eq!(ed.find_all("aa").0.len(), 2);
    }
}
