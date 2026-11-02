//! The Copy/Move dialog's editable target: the text field itself and how
//! what was typed turns into the destination the job runs with.

use std::path::{Path, PathBuf, is_separator};

use crate::app::DialogKind;

/// Single-line text with a cursor, counted in characters so multi-byte names
/// can't split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextField {
    text: String,
    cursor: usize,
}

impl TextField {
    /// Starts with the cursor after the last character.
    pub fn new(text: String) -> Self {
        let cursor = text.chars().count();
        TextField { text, cursor }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    fn byte_index(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map_or(self.text.len(), |(index, _)| index)
    }

    pub fn insert(&mut self, c: char) {
        let index = self.byte_index(self.cursor);
        self.text.insert(index, c);
        self.cursor += 1;
    }

    /// Returns whether anything was removed.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.text.remove(self.byte_index(self.cursor));
        true
    }

    /// Returns whether anything was removed.
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.text.chars().count() {
            return false;
        }
        self.text.remove(self.byte_index(self.cursor));
        true
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.chars().count();
    }
}

/// A resolved target with items already in the way, waiting for the user to
/// accept replacing them. Editing the field drops it, so what gets overwritten
/// is always what the dialog is showing.
pub struct Overwrite {
    pub items: Vec<(String, PathBuf)>,
    pub dest_dir: PathBuf,
    /// Names from `items` whose destination already exists.
    pub conflicts: Vec<String>,
}

pub struct Transfer {
    pub kind: DialogKind,
    /// `(display name, full source path)`, as the pane listed them.
    pub items: Vec<(String, PathBuf)>,
    /// The other pane's directory, which the dialog starts out pointing at.
    pub dest_dir: PathBuf,
    /// The field's starting text, to recognise a target left as it was.
    initial: String,
    pub field: TextField,
    /// Up put the focus on the field, so Left/Right move the cursor there
    /// instead of switching buttons. Typing works either way, and moves the
    /// focus here.
    pub editing: bool,
    pub overwrite: Option<Overwrite>,
}

impl Transfer {
    /// A single item's target is shown as its full path, so its name can be
    /// changed too; a batch keeps its names and only picks a directory.
    pub fn new(kind: DialogKind, items: Vec<(String, PathBuf)>, dest_dir: PathBuf) -> Self {
        let initial = match items.as_slice() {
            [(name, _)] => dest_dir.join(name),
            _ => dest_dir.clone(),
        }
        .display()
        .to_string();
        Transfer {
            kind,
            items,
            dest_dir,
            field: TextField::new(initial.clone()),
            initial,
            editing: false,
            overwrite: None,
        }
    }

    /// Turns the field's text into the items and directory the job takes:
    /// each item lands at `dest_dir.join(name)`, so a new name for a single
    /// item simply replaces its name in the list. Relative paths start from
    /// `base`, the active pane's directory.
    ///
    /// Text left as it was means exactly the starting target, never "into a
    /// directory of that name": copying `foo` next to an existing `foo` must
    /// stay a merge prompt, not become `foo/foo`.
    pub fn resolve(&self, base: &Path) -> Result<(Vec<(String, PathBuf)>, PathBuf), String> {
        let text = self.field.text();
        if text == self.initial {
            return Ok((self.items.clone(), self.dest_dir.clone()));
        }
        if text.trim().is_empty() {
            return Err("The destination path cannot be empty".to_string());
        }
        let path = expand_home(text)
            .ok_or_else(|| "Cannot find the home directory".to_string())
            .map(|path| base.join(path))?;

        let [(name, src)] = self.items.as_slice() else {
            return Ok((self.items.clone(), path));
        };
        // Like `cp`: a trailing slash or an existing directory means "into
        // it, under the same name".
        if text.ends_with(is_separator) || path.is_dir() {
            return Ok((vec![(name.clone(), src.clone())], path));
        }
        match (path.parent(), path.file_name()) {
            (Some(parent), Some(new_name)) => Ok((
                vec![(new_name.to_string_lossy().into_owned(), src.clone())],
                parent.to_path_buf(),
            )),
            _ => Err(format!("Cannot use \"{text}\" as a destination")),
        }
    }
}

/// `~` and `~/path` become paths under the home directory; anything else is
/// returned as written. `None` only when the home directory can't be found.
pub fn expand_home(path: &str) -> Option<PathBuf> {
    if path == "~" {
        return dirs::home_dir();
    }
    if let Some(suffix) = path.strip_prefix("~/") {
        return Some(dirs::home_dir()?.join(suffix));
    }
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn single(dest_dir: &Path, name: &str) -> Transfer {
        Transfer::new(
            DialogKind::Copy,
            vec![(name.to_string(), Path::new("/src").join(name))],
            dest_dir.to_path_buf(),
        )
    }

    /// Replaces the field's text, as if it had been typed.
    fn set_text(transfer: &mut Transfer, text: &str) {
        transfer.field = TextField::new(text.to_string());
    }

    #[test]
    fn field_edits_at_the_cursor_across_multibyte_characters() {
        let mut field = TextField::new("żółw".to_string());
        assert_eq!(field.cursor(), 4);
        field.move_left();
        field.insert('X');
        assert_eq!(field.text(), "żółXw");
        assert!(field.backspace());
        assert!(field.backspace());
        assert_eq!(field.text(), "żów");
        field.move_home();
        assert!(!field.backspace());
        assert!(field.delete());
        assert_eq!(field.text(), "ów");
        field.move_end();
        assert!(!field.delete());
        field.move_right();
        assert_eq!(field.cursor(), 2);
    }

    #[test]
    fn single_item_shows_the_full_target_and_a_batch_only_the_directory() {
        assert_eq!(single(Path::new("/d"), "a.txt").field.text(), "/d/a.txt");
        let batch = Transfer::new(
            DialogKind::Copy,
            vec![
                ("a".to_string(), PathBuf::from("/s/a")),
                ("b".to_string(), PathBuf::from("/s/b")),
            ],
            PathBuf::from("/d"),
        );
        assert_eq!(batch.field.text(), "/d");
    }

    #[test]
    fn untouched_directory_target_stays_a_merge_not_a_nested_copy() {
        let dir = test_dir("pc_test_transfer_untouched");
        fs::create_dir(dir.join("foo")).unwrap();
        let (items, dest_dir) = single(&dir, "foo").resolve(Path::new("/base")).unwrap();
        assert_eq!(items[0].0, "foo");
        assert_eq!(dest_dir, dir);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edited_name_becomes_the_new_name_in_the_edited_directory() {
        let mut transfer = single(Path::new("/d"), "a.txt");
        set_text(&mut transfer, "/other/b.txt");
        let (items, dest_dir) = transfer.resolve(Path::new("/base")).unwrap();
        assert_eq!(items[0].0, "b.txt");
        assert_eq!(items[0].1, Path::new("/src/a.txt"));
        assert_eq!(dest_dir, Path::new("/other"));
    }

    #[test]
    fn existing_directory_or_trailing_slash_keeps_the_name() {
        let dir = test_dir("pc_test_transfer_into_dir");
        fs::create_dir(dir.join("sub")).unwrap();
        let mut transfer = single(Path::new("/d"), "a.txt");

        set_text(&mut transfer, &dir.join("sub").display().to_string());
        let (items, dest_dir) = transfer.resolve(Path::new("/base")).unwrap();
        assert_eq!((items[0].0.as_str(), dest_dir), ("a.txt", dir.join("sub")));

        set_text(&mut transfer, "/not/there/yet/");
        let (items, dest_dir) = transfer.resolve(Path::new("/base")).unwrap();
        assert_eq!(items[0].0, "a.txt");
        assert_eq!(dest_dir, Path::new("/not/there/yet/"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn relative_paths_start_from_the_active_pane_and_home_expands() {
        let mut transfer = single(Path::new("/d"), "a.txt");
        set_text(&mut transfer, "copies/b.txt");
        let (items, dest_dir) = transfer.resolve(Path::new("/base")).unwrap();
        assert_eq!(items[0].0, "b.txt");
        assert_eq!(dest_dir, Path::new("/base/copies"));

        set_text(&mut transfer, "~/not-a-real-pc-dir/b.txt");
        let (_, dest_dir) = transfer.resolve(Path::new("/base")).unwrap();
        assert_eq!(
            dest_dir,
            dirs::home_dir().unwrap().join("not-a-real-pc-dir")
        );
    }

    #[test]
    fn batch_target_is_a_directory_and_names_stay() {
        let mut transfer = Transfer::new(
            DialogKind::Move,
            vec![
                ("a".to_string(), PathBuf::from("/s/a")),
                ("b".to_string(), PathBuf::from("/s/b")),
            ],
            PathBuf::from("/d"),
        );
        set_text(&mut transfer, "/elsewhere");
        let (items, dest_dir) = transfer.resolve(Path::new("/base")).unwrap();
        assert_eq!(items, transfer.items);
        assert_eq!(dest_dir, Path::new("/elsewhere"));
    }

    #[test]
    fn empty_text_is_refused() {
        let mut transfer = single(Path::new("/d"), "a.txt");
        set_text(&mut transfer, "  ");
        assert!(transfer.resolve(Path::new("/base")).is_err());
    }
}
