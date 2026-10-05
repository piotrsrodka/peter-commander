use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pane::SortKey;

#[derive(Debug, Clone)]
pub struct LastDirs {
    pub left: PathBuf,
    pub right: PathBuf,
}

fn config_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("peter-commander"))
}

fn state_file() -> Option<PathBuf> {
    Some(config_dir()?.join("last_dirs"))
}

fn sort_file() -> Option<PathBuf> {
    Some(config_dir()?.join("sort"))
}

/// Each pane's sort, as `(key, reversed)`.
pub type PaneSort = (SortKey, bool);

/// The left and right panes' sorts from last session, one `key reversed`
/// line each (e.g. `size false`). `None` if missing or unreadable.
pub fn load_sorts() -> Option<(PaneSort, PaneSort)> {
    parse_sorts(&fs::read_to_string(sort_file()?).ok()?)
}

fn parse_sorts(contents: &str) -> Option<(PaneSort, PaneSort)> {
    let parse = |line: &str| {
        let (key, reversed) = line.split_once(' ')?;
        Some((
            SortKey::from_key(key.trim())?,
            reversed.trim().parse().ok()?,
        ))
    };
    let mut lines = contents.lines();
    Some((parse(lines.next()?)?, parse(lines.next()?)?))
}

pub fn save_sorts(left: PaneSort, right: PaneSort) {
    let Some(path) = sort_file() else { return };
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let _ = fs::write(path, format_sorts(left, right));
}

fn format_sorts(left: PaneSort, right: PaneSort) -> String {
    format!(
        "{} {}\n{} {}\n",
        left.0.key(),
        left.1,
        right.0.key(),
        right.1
    )
}

fn settings_file() -> Option<PathBuf> {
    Some(config_dir()?.join("settings"))
}

pub fn load() -> Option<LastDirs> {
    let path = state_file()?;
    let contents = fs::read_to_string(path).ok()?;
    let mut lines = contents.lines();
    // Older versions of this file may carry a `\\?\`-prefixed verbatim path
    // (from `fs::canonicalize` on Windows, before that got cleaned up) —
    // strip it here too so a stale saved path doesn't outlive the fix.
    let left = crate::pane::strip_verbatim_prefix(PathBuf::from(lines.next()?));
    let right = crate::pane::strip_verbatim_prefix(PathBuf::from(lines.next()?));

    if left.is_dir() && right.is_dir() {
        Some(LastDirs { left, right })
    } else {
        None
    }
}

pub fn save(left: &Path, right: &Path) {
    let Some(path) = state_file() else { return };
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let contents = format!("{}\n{}\n", left.display(), right.display());
    let _ = fs::write(path, contents);
}

/// Settings are stored as simple `key=true`/`key=false` lines, keyed by a
/// stable string (independent of the setting's display label) so future
/// settings can be added without touching this format.
pub fn load_settings() -> HashMap<String, bool> {
    match settings_file() {
        Some(path) => load_settings_from(&path),
        None => HashMap::new(),
    }
}

fn load_settings_from(path: &Path) -> HashMap<String, bool> {
    let mut map = HashMap::new();
    let Ok(contents) = fs::read_to_string(path) else {
        return map;
    };
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once('=')
            && let Ok(parsed) = value.trim().parse::<bool>()
        {
            map.insert(key.trim().to_string(), parsed);
        }
    }
    map
}

pub fn save_settings(pairs: &[(&str, bool)]) {
    if let Some(path) = settings_file() {
        save_settings_to(&path, pairs);
    }
}

fn save_settings_to(path: &Path, pairs: &[(&str, bool)]) {
    let Some(parent) = path.parent() else { return };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut contents = String::new();
    for (key, value) in pairs {
        contents.push_str(&format!("{key}={value}\n"));
    }
    let _ = fs::write(path, contents);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_sorts_and_rejects_garbage() {
        let text = format_sorts((SortKey::Size, false), (SortKey::Name, true));
        assert_eq!(
            parse_sorts(&text),
            Some(((SortKey::Size, false), (SortKey::Name, true)))
        );
        assert_eq!(parse_sorts("size false\n"), None);
        assert_eq!(parse_sorts("bogus false\nname true\n"), None);
    }

    #[test]
    fn round_trips_last_dirs() {
        let tmp = std::env::temp_dir().join("pc_state_test");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("left")).unwrap();
        fs::create_dir_all(tmp.join("right")).unwrap();

        let path = tmp.join("last_dirs");
        let contents = format!(
            "{}\n{}\n",
            tmp.join("left").display(),
            tmp.join("right").display()
        );
        fs::write(&path, contents).unwrap();

        let loaded = fs::read_to_string(&path).unwrap();
        let mut lines = loaded.lines();
        let left = PathBuf::from(lines.next().unwrap());
        let right = PathBuf::from(lines.next().unwrap());
        assert!(left.is_dir());
        assert!(right.is_dir());

        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn round_trips_settings() {
        let path = std::env::temp_dir().join("pc_state_test_settings");
        let _ = fs::remove_file(&path);

        save_settings_to(
            &path,
            &[("wait_after_shell_command", false), ("other", true)],
        );
        let loaded = load_settings_from(&path);

        assert_eq!(loaded.get("wait_after_shell_command"), Some(&false));
        assert_eq!(loaded.get("other"), Some(&true));

        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn missing_settings_file_loads_empty() {
        let path = std::env::temp_dir().join("pc_state_test_settings_missing");
        let _ = fs::remove_file(&path);

        let loaded = load_settings_from(&path);

        assert!(loaded.is_empty());
    }
}
