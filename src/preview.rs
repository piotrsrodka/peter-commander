use std::cell::RefCell;
use std::fs;
use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use crate::editor::{self, LineLayout};
use crate::pane::Pane;

/// How much of a file's start is read for the quick-view preview and to
/// sniff whether it's text or binary. Loaded once per selection and kept in
/// memory; scrolling moves within this buffer rather than reading more.
pub const PREVIEW_READ_LIMIT: usize = 1024 * 1024;

/// Formats PC can decode itself (the `image` crate features enabled in
/// Cargo.toml).
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp"];

pub fn is_image(name: &str) -> bool {
    crate::app::has_extension(name, IMAGE_EXTENSIONS)
}

/// A directory entry as shown in the quick-view listing: just enough to
/// render a name and color it, no size/modified columns.
pub struct DirEntryPreview {
    pub name: String,
    pub is_dir: bool,
}

/// What the quick-view preview pane has to show for the currently selected
/// entry in the source pane.
pub enum PreviewContent {
    /// Nothing selected (e.g. an empty directory).
    Empty,
    Directory(Vec<DirEntryPreview>),
    /// Sniffed as binary (contains a NUL byte in the sampled prefix) — shown
    /// as just a size, never dumped as text.
    Binary(u64),
    /// Shared with the cache (see `read_file`), so showing it again costs
    /// nothing.
    Text(Rc<Vec<String>>),
    Error(String),
}

impl PreviewContent {
    /// The same content (text shared, not copied), for what the cache keeps.
    fn shallow_clone_if_cacheable(&self) -> Option<Self> {
        match self {
            PreviewContent::Text(lines) => Some(PreviewContent::Text(Rc::clone(lines))),
            PreviewContent::Binary(size) => Some(PreviewContent::Binary(*size)),
            _ => None,
        }
    }

    fn shallow_clone(&self) -> Self {
        self.shallow_clone_if_cacheable()
            .unwrap_or(PreviewContent::Empty)
    }

    pub fn line_count(&self) -> usize {
        match self {
            PreviewContent::Text(lines) => lines.len(),
            PreviewContent::Directory(entries) => entries.len(),
            _ => 0,
        }
    }

    /// How many screen rows the content takes in a preview `width` cells
    /// wide: for `Text` with `wrap` (the editor's Wrap Lines, shared), a
    /// long line spans several, broken exactly where the editor breaks it.
    /// Scroll clamping (so the last line lands at the bottom instead of
    /// getting cut off) needs this, not the count of lines.
    pub fn screen_row_count(&self, width: usize, wrap: bool) -> usize {
        match self {
            PreviewContent::Text(lines) if wrap => {
                lines.iter().map(|line| rows_of(line, width).len()).sum()
            }
            _ => self.line_count(),
        }
    }

    /// The screen row at which text line `line` starts, in the same terms.
    pub fn first_row_of(&self, line: usize, width: usize, wrap: bool) -> usize {
        match self {
            PreviewContent::Text(lines) if wrap => lines
                .iter()
                .take(line)
                .map(|line| rows_of(line, width).len())
                .sum(),
            _ => line,
        }
    }

    /// The text line screen row `row` belongs to (the inverse of
    /// `first_row_of`), for keeping the same place when wrapping is
    /// switched.
    pub fn line_at_row(&self, row: usize, width: usize, wrap: bool) -> usize {
        match self {
            PreviewContent::Text(lines) if wrap => {
                let mut seen = 0;
                for (idx, line) in lines.iter().enumerate() {
                    seen += rows_of(line, width).len();
                    if seen > row {
                        return idx;
                    }
                }
                lines.len()
            }
            _ => row,
        }
    }

    /// Cells the widest text line takes unwrapped: how far the preview can
    /// scroll sideways.
    pub fn widest_line(&self) -> usize {
        match self {
            PreviewContent::Text(lines) => lines
                .iter()
                .map(|line| {
                    let layout = LineLayout::new(line, None);
                    layout.x_of(0, layout.chars.len())
                })
                .max()
                .unwrap_or(0),
            _ => 0,
        }
    }
}

/// Where the rows of `line` start, wrapped for a preview `width` cells wide.
pub fn rows_of(line: &str, width: usize) -> Vec<usize> {
    LineLayout::new(line, Some(editor::wrap_width_for(width))).starts
}

pub fn build_preview(pane: &Pane) -> PreviewContent {
    let Some(entry) = pane.selected_entry() else {
        return PreviewContent::Empty;
    };

    let Some(path) = pane.preview_target() else {
        return PreviewContent::Empty;
    };
    if entry.is_dir {
        return match list_dir(&path, pane.hide_hidden) {
            Ok(entries) => PreviewContent::Directory(entries),
            Err(err) => PreviewContent::Error(err.to_string()),
        };
    }

    // Opening a named pipe blocks until something writes to it (and reading
    // a device can block or never end), which froze the whole UI; only
    // regular files get read.
    let meta = match fs::metadata(&path) {
        Ok(meta) if !meta.is_file() => {
            return PreviewContent::Error(
                "Special file (pipe, socket or device): not previewed".to_string(),
            );
        }
        Ok(meta) => meta,
        Err(err) => return PreviewContent::Error(err.to_string()),
    };
    read_file(&path, &meta)
}

/// The file last read for the preview: which one, and as it was then.
type CachedFile = ((PathBuf, Option<SystemTime>, u64), PreviewContent);

thread_local! {
    /// The preview of the last file read, while it stays as it was: it's
    /// asked for on every redraw (and by find, scrolling and the F-key bar),
    /// and reading up to a megabyte each time for the same answer was a
    /// waste. A file that changes gets a new modification time or size,
    /// and is read again — which is how the quick view follows it.
    static LAST_FILE: RefCell<Option<CachedFile>> = const { RefCell::new(None) };
}

fn read_file(path: &Path, meta: &fs::Metadata) -> PreviewContent {
    let key = (path.to_path_buf(), meta.modified().ok(), meta.len());
    let cached = LAST_FILE.with(|cell| {
        cell.borrow()
            .as_ref()
            .filter(|(at, _)| *at == key)
            .map(|(_, content)| content.shallow_clone())
    });
    if let Some(content) = cached {
        return content;
    }
    let content = read_file_now(path, meta.len());
    if let Some(copy) = content.shallow_clone_if_cacheable() {
        LAST_FILE.with(|cell| *cell.borrow_mut() = Some((key, copy)));
    }
    content
}

fn read_file_now(path: &Path, size: u64) -> PreviewContent {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) => return PreviewContent::Error(err.to_string()),
    };
    let mut buf = vec![0u8; PREVIEW_READ_LIMIT];
    let read = match file.read(&mut buf) {
        Ok(n) => n,
        Err(err) => return PreviewContent::Error(err.to_string()),
    };
    buf.truncate(read);

    if buf.contains(&0u8) {
        PreviewContent::Binary(size)
    } else {
        let text = String::from_utf8_lossy(&buf).into_owned();
        // An invisible byte order mark, dropped as the editor drops it, so
        // the text (and find's columns in it) are the same in both.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        PreviewContent::Text(Rc::new(text.lines().map(str::to_string).collect()))
    }
}

/// Lists `path`'s contents as names only (directories first, then files,
/// each alphabetical), mirroring `Pane::reload`'s ordering but without the
/// size/modified columns a full listing needs.
fn list_dir(path: &Path, hide_hidden: bool) -> std::io::Result<Vec<DirEntryPreview>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().to_string();
        if hide_hidden && name.starts_with('.') {
            continue;
        }
        let is_dir = fs::metadata(entry.path())
            .or_else(|_| entry.metadata())
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            dirs.push(name);
        } else {
            files.push(name);
        }
    }
    dirs.sort_by_key(|name| name.to_lowercase());
    files.sort_by_key(|name| name.to_lowercase());

    let mut entries: Vec<DirEntryPreview> = dirs
        .into_iter()
        .map(|name| DirEntryPreview { name, is_dir: true })
        .collect();
    entries.extend(files.into_iter().map(|name| DirEntryPreview {
        name,
        is_dir: false,
    }));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions() {
        assert!(is_image("a.JPG"));
        assert!(is_image("b.webp"));
        assert!(!is_image("c.svg"));
        assert!(!is_image("jpg"));
    }

    #[cfg(unix)]
    #[test]
    fn named_pipe_is_not_read_for_preview() {
        let base = std::env::temp_dir().join("pc_test_preview_fifo");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(base.join("pipe"))
            .status()
            .unwrap();
        assert!(made.success());

        let mut pane = Pane::new(base.clone(), false).unwrap();
        pane.selected = pane.entries.iter().position(|e| e.name == "pipe").unwrap();

        // Without the guard File::open blocks forever waiting for a writer.
        assert!(matches!(build_preview(&pane), PreviewContent::Error(_)));
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn regular_file_is_still_previewed_as_text() {
        let base = std::env::temp_dir().join("pc_test_preview_text");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("a.txt"), "hello\nworld\n").unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        pane.selected = pane.entries.iter().position(|e| e.name == "a.txt").unwrap();

        assert!(matches!(build_preview(&pane), PreviewContent::Text(lines) if lines[0] == "hello"));
        fs::remove_dir_all(&base).unwrap();
    }
}
