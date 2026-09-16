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

    /// Enters the selected directory. Returns `Ok(Some(message))` if the
    /// directory couldn't be entered (e.g. permission denied) without
    /// changing the current directory, instead of propagating a fatal error.
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
}
