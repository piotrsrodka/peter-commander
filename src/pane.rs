use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
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
            });
        }

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.cwd)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let item = Entry {
                name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
            };
            if item.is_dir {
                dirs.push(item);
            } else {
                files.push(item);
            }
        }
        dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

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

    pub fn enter_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.selected_entry() {
            if entry.is_dir {
                let new_path = if entry.name == ".." {
                    self.cwd
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| self.cwd.clone())
                } else {
                    self.cwd.join(&entry.name)
                };
                self.cwd = new_path;
                self.selected = 0;
                self.reload()?;
            }
        }
        Ok(())
    }
}
