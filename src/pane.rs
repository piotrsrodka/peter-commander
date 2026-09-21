use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

/// Whether `path` counts as "executable" — the same thing Enter checks to
/// decide whether to run it. On Unix this is the permission bit; Windows
/// has no such bit, so it goes by extension instead (the same signal the
/// shell itself uses, via `PATHEXT`).
#[cfg(unix)]
fn is_executable_file(_path: &Path, metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable_file(path: &Path, _metadata: &fs::Metadata) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_lowercase().as_str(),
                "exe" | "bat" | "cmd" | "com"
            )
        })
}

/// Whether "hide hidden files" should skip this entry. Unix hides by
/// dotfile-naming convention only. Windows keeps that (dotfiles like
/// `.gitignore` are common even there) but additionally hides via the
/// Hidden file attribute — this is also how Windows marks the legacy
/// localized-name compatibility junctions in the user profile (e.g. "Moje
/// Dokumenty" alongside the real "Documents"), which additionally deny
/// direct access (`ERROR_ACCESS_DENIED`) if entered, so hiding them by
/// convention avoids a dead end in the listing rather than just a cosmetic
/// duplicate.
#[cfg(windows)]
fn is_hidden_entry(name: &str, metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    name.starts_with('.') || metadata.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0
}

#[cfg(unix)]
fn is_hidden_entry(name: &str, _metadata: &fs::Metadata) -> bool {
    name.starts_with('.')
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Whether this is a file with an executable permission bit set — the
    /// same thing Enter checks to decide whether to run it.
    pub is_executable: bool,
}

#[derive(Debug)]
pub struct Pane {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
    /// Whether dotfiles (names starting with `.`) are filtered out of the
    /// listing. The ".." pseudo-entry is always shown regardless.
    pub hide_hidden: bool,
    /// Names tagged with Insert, independent of the cursor position — F5/
    /// F6/F8 act on this whole set when it's non-empty, instead of just the
    /// entry under the cursor. Never contains "..".
    pub marked: HashSet<String>,
}

impl Pane {
    pub fn new(cwd: PathBuf, hide_hidden: bool) -> Result<Self> {
        let mut pane = Pane {
            cwd,
            entries: Vec::new(),
            selected: 0,
            hide_hidden,
            marked: HashSet::new(),
        };
        pane.reload()?;
        Ok(pane)
    }

    pub fn reload(&mut self) -> Result<()> {
        let mut entries = Vec::new();

        if self.cwd.parent().is_some() {
            entries.push(Entry {
                name: "..".to_string(),
                is_dir: true,
                size: 0,
                modified: None,
                is_executable: false,
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.cwd)? {
            // Skip entries we can't stat (e.g. broken symlinks, permission
            // issues on a single item) instead of failing the whole listing.
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().to_string();
            // The hidden check always uses the entry's own (non-following)
            // metadata: a reparse point like a Windows compatibility
            // junction transparently forwards a followed stat to its
            // target's attributes, losing the junction's own Hidden flag.
            let Ok(link_metadata) = entry.metadata() else {
                continue;
            };
            if self.hide_hidden && is_hidden_entry(&name, &link_metadata) {
                continue;
            }
            // Follow symlinks for classification (so a symlink to a
            // directory is treated as a directory, matching `ls -L`);
            // fall back to the link's own metadata for broken symlinks.
            let metadata = fs::metadata(entry.path()).unwrap_or(link_metadata);
            let is_dir = metadata.is_dir();
            let item = Entry {
                name,
                is_dir,
                size: metadata.len(),
                modified: metadata.modified().ok(),
                is_executable: !is_dir && is_executable_file(&entry.path(), &metadata),
            };
            if item.is_dir {
                dirs.push(item);
            } else {
                files.push(item);
            }
        }
        dirs.sort_by_key(|a| a.name.to_lowercase());
        files.sort_by_key(|a| a.name.to_lowercase());

        entries.extend(dirs);
        entries.extend(files);

        self.entries = entries;
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len().saturating_sub(1);
        }
        if !self.marked.is_empty() {
            let names: HashSet<&str> = self.entries.iter().map(|e| e.name.as_str()).collect();
            self.marked.retain(|m| names.contains(m.as_str()));
        }
        Ok(())
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    /// Insert: tags/untags the entry under the cursor (a no-op on "..",
    /// which can never be marked, but — matching the original NC — still
    /// steps the cursor down like any other entry) and moves the cursor
    /// down, so repeated presses sweep down through a run of files.
    pub fn toggle_mark_selected(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.name != ".." {
            let name = entry.name.clone();
            if !self.marked.remove(&name) {
                self.marked.insert(name);
            }
        }
        self.move_down();
    }

    /// The marked entries as `(name, full path)` pairs, in listing order.
    /// Empty when nothing is tagged — callers fall back to the entry under
    /// the cursor in that case.
    pub fn marked_items(&self) -> Vec<(String, PathBuf)> {
        self.entries
            .iter()
            .filter(|e| self.marked.contains(&e.name))
            .map(|e| (e.name.clone(), self.cwd.join(&e.name)))
            .collect()
    }

    pub fn clear_marks(&mut self) {
        self.marked.clear();
    }

    /// `(count, total bytes)` of the marked entries, for the pane's status
    /// line — `None` when nothing is tagged, so the caller falls back to
    /// showing the plain file/dir totals instead. Directory sizes aren't
    /// summed in (a directory's raw metadata length isn't its content
    /// size), matching how the listing itself never shows a byte size for
    /// directories either.
    pub fn marked_summary(&self) -> Option<(usize, u64)> {
        if self.marked.is_empty() {
            return None;
        }
        let mut count = 0usize;
        let mut bytes = 0u64;
        for entry in &self.entries {
            if self.marked.contains(&entry.name) {
                count += 1;
                if !entry.is_dir {
                    bytes += entry.size;
                }
            }
        }
        Some((count, bytes))
    }

    /// `(files, dirs)` in the listing, excluding the ".." pseudo-entry —
    /// the pane's status line default when nothing is marked.
    pub fn totals(&self) -> (usize, usize) {
        let mut files = 0usize;
        let mut dirs = 0usize;
        for entry in &self.entries {
            if entry.name == ".." {
                continue;
            }
            if entry.is_dir {
                dirs += 1;
            } else {
                files += 1;
            }
        }
        (files, dirs)
    }

    /// The filesystem path of the selected entry, or `None` for the ".." pseudo-entry.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let entry = self.selected_entry()?;
        if entry.name == ".." {
            None
        } else {
            Some(self.cwd.join(&entry.name))
        }
    }

    /// The filesystem path the quick-view preview should show for the
    /// current selection: the parent directory for "..", otherwise the
    /// selected entry itself. Unlike `selected_path`, this resolves ".."
    /// instead of returning `None`, since the preview always has something
    /// to point at.
    pub fn preview_target(&self) -> Option<PathBuf> {
        let entry = self.selected_entry()?;
        if entry.name == ".." {
            Some(self.cwd.parent().unwrap_or(&self.cwd).to_path_buf())
        } else {
            Some(self.cwd.join(&entry.name))
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    pub fn move_to_top(&mut self) {
        self.selected = 0;
    }

    pub fn move_to_bottom(&mut self) {
        self.selected = self.entries.len().saturating_sub(1);
    }

    /// PgUp: moves up roughly one screenful (`page` rows).
    pub fn move_page_up(&mut self, page: usize) {
        self.selected = self.selected.saturating_sub(page);
    }

    /// PgDown: moves down roughly one screenful (`page` rows).
    pub fn move_page_down(&mut self, page: usize) {
        let last = self.entries.len().saturating_sub(1);
        self.selected = (self.selected + page).min(last);
    }

    /// Switches to `new_path`. Returns `Ok(Some(message))` and leaves the
    /// current directory unchanged if it couldn't be entered (e.g.
    /// permission denied), instead of propagating a fatal error.
    fn set_cwd(&mut self, new_path: PathBuf) -> Result<Option<String>> {
        self.set_cwd_selecting(new_path, None)
    }

    /// Like `set_cwd`, but if `select_name` is given and found among the
    /// reloaded entries, selects it instead of defaulting to the top. Used
    /// when navigating up so the cursor lands back on the folder just left,
    /// instead of resetting to the top of the parent listing.
    fn set_cwd_selecting(&mut self, new_path: PathBuf, select_name: Option<&str>) -> Result<Option<String>> {
        let previous_cwd = self.cwd.clone();
        let previous_selected = self.selected;
        // Marks belong to the listing they were made in — carrying them by
        // name into a different directory would tag an unrelated file that
        // happens to share a name (e.g. every directory's own ".gitignore"),
        // and silently drop the rest. So a real directory change starts
        // from a clean slate, same as the fresh `Pane` the app boots with.
        let previous_marked = std::mem::take(&mut self.marked);
        self.cwd = new_path;
        self.selected = 0;

        if let Err(err) = self.reload() {
            self.cwd = previous_cwd;
            self.selected = previous_selected;
            self.marked = previous_marked;
            return Ok(Some(format!("Cannot open directory: {err}")));
        }

        if let Some(name) = select_name
            && let Some(index) = self.entries.iter().position(|e| e.name == name)
        {
            self.selected = index;
        }
        Ok(None)
    }

    /// Enters the selected directory (see `set_cwd` for failure handling).
    pub fn enter_selected(&mut self) -> Result<Option<String>> {
        let Some(entry) = self.selected_entry() else {
            return Ok(None);
        };
        if !entry.is_dir {
            return Ok(None);
        }

        if entry.name == ".." {
            let left_name = self
                .cwd
                .file_name()
                .map(|n| n.to_string_lossy().to_string());
            let new_path = self
                .cwd
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.cwd.clone());
            return self.set_cwd_selecting(new_path, left_name.as_deref());
        }

        let new_path = self.cwd.join(&entry.name);
        self.set_cwd(new_path)
    }

    /// Changes directory to `target` (absolute, or relative to the current
    /// directory), resolving `.`/`..`/symlinks where possible. Used by the
    /// command line's built-in `cd` handling (see `set_cwd` for failure
    /// handling).
    pub fn change_dir(&mut self, target: &Path) -> Result<Option<String>> {
        let candidate = if target.is_absolute() {
            target.to_path_buf()
        } else {
            self.cwd.join(target)
        };
        let new_path = fs::canonicalize(&candidate)
            .map(strip_verbatim_prefix)
            .unwrap_or(candidate);
        self.set_cwd(new_path)
    }
}

/// `fs::canonicalize` on Windows always returns a `\\?\`-prefixed "verbatim"
/// path (e.g. `\\?\C:\Users\Piotr`) — harmless to Win32 APIs, but ugly in
/// the prompt and not understood by every external program `cd` might hand
/// the path to. This strips that prefix back to the ordinary form, same as
/// what a plain (non-canonicalized) path would look like. A no-op on other
/// platforms, where `canonicalize` doesn't add such a prefix.
#[cfg(windows)]
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(windows))]
pub fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    // Permission bits and symlink creation without elevated privileges are
    // Unix-specific; these three tests don't have a Windows equivalent.
    #[cfg(unix)]
    #[test]
    fn entering_unreadable_directory_shows_message_without_moving() {
        let base = std::env::temp_dir().join("pc_test_pane_noaccess");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let locked = base.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        pane.selected = pane
            .entries
            .iter()
            .position(|e| e.name == "locked")
            .unwrap();

        let result = pane.enter_selected().unwrap();

        assert!(result.is_some(), "expected a friendly error message");
        assert_eq!(pane.cwd, base, "cwd must not change on failed entry");

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_directory_is_classified_as_a_directory() {
        let base = std::env::temp_dir().join("pc_test_pane_symlink");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("real_dir")).unwrap();
        std::os::unix::fs::symlink(base.join("real_dir"), base.join("link_to_dir")).unwrap();

        let pane = Pane::new(base.clone(), false).unwrap();
        let entry = pane
            .entries
            .iter()
            .find(|e| e.name == "link_to_dir")
            .unwrap();

        assert!(
            entry.is_dir,
            "a symlink to a directory must be classified as a directory"
        );

        fs::remove_dir_all(&base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_is_still_listed() {
        let base = std::env::temp_dir().join("pc_test_pane_broken_symlink");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        std::os::unix::fs::symlink(base.join("does_not_exist"), base.join("dangling")).unwrap();

        let pane = Pane::new(base.clone(), false).unwrap();
        assert!(
            pane.entries.iter().any(|e| e.name == "dangling"),
            "a broken symlink should still show up in the listing"
        );

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn change_dir_moves_to_relative_and_absolute_targets() {
        let base = std::env::temp_dir().join("pc_test_pane_change_dir");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub")).unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();

        let result = pane.change_dir(Path::new("sub")).unwrap();
        assert!(result.is_none());
        assert_eq!(
            pane.cwd,
            strip_verbatim_prefix(base.join("sub").canonicalize().unwrap())
        );

        let result = pane.change_dir(&base).unwrap();
        assert!(result.is_none());
        assert_eq!(pane.cwd, strip_verbatim_prefix(base.canonicalize().unwrap()));

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn change_dir_to_missing_path_reports_error_without_moving() {
        let base = std::env::temp_dir().join("pc_test_pane_change_dir_missing");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        let previous_cwd = pane.cwd.clone();

        let result = pane.change_dir(Path::new("does_not_exist")).unwrap();

        assert!(result.is_some());
        assert_eq!(pane.cwd, previous_cwd);

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn hide_hidden_filters_dotfiles_but_keeps_dotdot() {
        let base = std::env::temp_dir().join("pc_test_pane_hidden");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join(".hidden_file"), "").unwrap();
        fs::write(base.join("visible_file"), "").unwrap();

        let pane = Pane::new(base.clone(), true).unwrap();
        assert!(!pane.entries.iter().any(|e| e.name == ".hidden_file"));
        assert!(pane.entries.iter().any(|e| e.name == "visible_file"));
        assert!(pane.entries.iter().any(|e| e.name == ".."));

        let pane = Pane::new(base.clone(), false).unwrap();
        assert!(pane.entries.iter().any(|e| e.name == ".hidden_file"));

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn toggle_mark_selected_tags_and_steps_down_but_skips_dotdot() {
        let base = std::env::temp_dir().join("pc_test_pane_mark");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("a.txt"), "").unwrap();
        fs::write(base.join("b.txt"), "").unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        // Entries are ".." (dirs come first, and ".." is always first), then
        // the two files sorted by name.
        pane.selected = 0;
        pane.toggle_mark_selected();
        assert!(
            pane.marked.is_empty(),
            "\"..\" must never be markable"
        );
        assert_eq!(
            pane.selected, 1,
            "but the cursor still steps down over it, like any other entry"
        );

        pane.toggle_mark_selected();
        assert!(pane.marked.contains("a.txt"));
        assert_eq!(pane.selected, 2, "marking steps the cursor down");

        pane.toggle_mark_selected();
        assert!(pane.marked.contains("a.txt"));
        assert!(pane.marked.contains("b.txt"));

        let items = pane.marked_items();
        assert_eq!(
            items,
            vec![
                ("a.txt".to_string(), base.join("a.txt")),
                ("b.txt".to_string(), base.join("b.txt")),
            ]
        );

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn reload_drops_marks_for_entries_that_no_longer_exist() {
        let base = std::env::temp_dir().join("pc_test_pane_mark_reload");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("a.txt"), "").unwrap();
        fs::write(base.join("b.txt"), "").unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        pane.marked.insert("a.txt".to_string());
        pane.marked.insert("b.txt".to_string());

        fs::remove_file(base.join("a.txt")).unwrap();
        pane.reload().unwrap();

        assert!(!pane.marked.contains("a.txt"));
        assert!(pane.marked.contains("b.txt"));

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn marked_summary_sums_bytes_of_marked_files_only() {
        let base = std::env::temp_dir().join("pc_test_pane_marked_summary");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("subdir")).unwrap();
        fs::write(base.join("a.txt"), "12345").unwrap();
        fs::write(base.join("b.txt"), "1234567890").unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        assert_eq!(pane.marked_summary(), None, "nothing marked yet");

        pane.marked.insert("a.txt".to_string());
        pane.marked.insert("subdir".to_string());
        let (count, bytes) = pane.marked_summary().unwrap();
        assert_eq!(count, 2, "counts marked dirs too");
        assert_eq!(bytes, 5, "but only sums bytes of marked files");

        let (files, dirs) = pane.totals();
        assert_eq!((files, dirs), (2, 1), "totals() excludes \"..\"");

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn changing_directory_clears_marks_even_when_the_other_dir_has_a_same_named_file() {
        let base = std::env::temp_dir().join("pc_test_pane_mark_dir_change");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(base.join(".gitignore"), "top").unwrap();
        fs::write(base.join("only_here.txt"), "").unwrap();
        // A same-named file in the other directory used to survive the
        // round trip and look "still marked" even though it's a completely
        // different file.
        fs::write(base.join("sub").join(".gitignore"), "nested").unwrap();

        let mut pane = Pane::new(base.clone(), false).unwrap();
        pane.marked.insert(".gitignore".to_string());
        pane.marked.insert("only_here.txt".to_string());

        pane.change_dir(Path::new("sub")).unwrap();
        assert!(
            pane.marked.is_empty(),
            "entering a different directory must not carry marks over by name"
        );

        pane.marked.insert(".gitignore".to_string());
        pane.change_dir(&base).unwrap();
        assert!(
            pane.marked.is_empty(),
            "leaving a directory must not leave stale marks behind either"
        );

        fs::remove_dir_all(&base).unwrap();
    }
}
