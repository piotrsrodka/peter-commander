use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug)]
pub struct Pane {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub selected: usize,
}

impl Pane {
    pub fn new(cwd: PathBuf) -> Result<Self> {
        let mut pane = Pane {
            cwd,
            entries: Vec::new(),
            selected: 0,
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
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.cwd)? {
            // Skip entries we can't stat (e.g. broken symlinks, permission
            // issues on a single item) instead of failing the whole listing.
            let Ok(entry) = entry else { continue };
            // Follow symlinks for classification (so a symlink to a
            // directory is treated as a directory, matching `ls -L`);
            // fall back to the link's own metadata for broken symlinks.
            let Ok(metadata) = fs::metadata(entry.path()).or_else(|_| entry.metadata()) else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let item = Entry {
                name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
                modified: metadata.modified().ok(),
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
        Ok(())
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
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

    /// Switches to `new_path`. Returns `Ok(Some(message))` and leaves the
    /// current directory unchanged if it couldn't be entered (e.g.
    /// permission denied), instead of propagating a fatal error.
    fn set_cwd(&mut self, new_path: PathBuf) -> Result<Option<String>> {
        let previous_cwd = self.cwd.clone();
        let previous_selected = self.selected;
        self.cwd = new_path;
        self.selected = 0;

        if let Err(err) = self.reload() {
            self.cwd = previous_cwd;
            self.selected = previous_selected;
            return Ok(Some(format!("Cannot open directory: {err}")));
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

        let new_path = if entry.name == ".." {
            self.cwd
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.cwd.clone())
        } else {
            self.cwd.join(&entry.name)
        };

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
        let new_path = fs::canonicalize(&candidate).unwrap_or(candidate);
        self.set_cwd(new_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn entering_unreadable_directory_shows_message_without_moving() {
        let base = std::env::temp_dir().join("pc_test_pane_noaccess");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let locked = base.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let mut pane = Pane::new(base.clone()).unwrap();
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

    #[test]
    fn symlink_to_directory_is_classified_as_a_directory() {
        let base = std::env::temp_dir().join("pc_test_pane_symlink");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("real_dir")).unwrap();
        std::os::unix::fs::symlink(base.join("real_dir"), base.join("link_to_dir")).unwrap();

        let pane = Pane::new(base.clone()).unwrap();
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

    #[test]
    fn broken_symlink_is_still_listed() {
        let base = std::env::temp_dir().join("pc_test_pane_broken_symlink");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        std::os::unix::fs::symlink(base.join("does_not_exist"), base.join("dangling")).unwrap();

        let pane = Pane::new(base.clone()).unwrap();
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

        let mut pane = Pane::new(base.clone()).unwrap();

        let result = pane.change_dir(Path::new("sub")).unwrap();
        assert!(result.is_none());
        assert_eq!(pane.cwd, base.join("sub").canonicalize().unwrap());

        let result = pane.change_dir(&base).unwrap();
        assert!(result.is_none());
        assert_eq!(pane.cwd, base.canonicalize().unwrap());

        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn change_dir_to_missing_path_reports_error_without_moving() {
        let base = std::env::temp_dir().join("pc_test_pane_change_dir_missing");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        let mut pane = Pane::new(base.clone()).unwrap();
        let previous_cwd = pane.cwd.clone();

        let result = pane.change_dir(Path::new("does_not_exist")).unwrap();

        assert!(result.is_some());
        assert_eq!(pane.cwd, previous_cwd);

        fs::remove_dir_all(&base).unwrap();
    }
}
