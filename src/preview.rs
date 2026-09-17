use std::fs;
use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use crate::pane::Pane;

/// How much of a file's start is read for the quick-view preview and to
/// sniff whether it's text or binary. Loaded once per selection and kept in
/// memory; scrolling moves within this buffer rather than reading more.
pub const PREVIEW_READ_LIMIT: usize = 1024 * 1024;

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
    Text(Vec<String>),
    Error(String),
}

impl PreviewContent {
    pub fn line_count(&self) -> usize {
        match self {
            PreviewContent::Text(lines) => lines.len(),
            PreviewContent::Directory(entries) => entries.len(),
            _ => 0,
        }
    }
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

    let mut file = match File::open(&path) {
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
        PreviewContent::Binary(entry.size)
    } else {
        let text = String::from_utf8_lossy(&buf).into_owned();
        PreviewContent::Text(text.lines().map(str::to_string).collect())
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
    entries.extend(
        files
            .into_iter()
            .map(|name| DirEntryPreview { name, is_dir: false }),
    );
    Ok(entries)
}
