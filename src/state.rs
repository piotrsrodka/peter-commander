use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

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

fn settings_file() -> Option<PathBuf> {
    Some(config_dir()?.join("settings"))
}

pub fn load() -> Option<LastDirs> {
    let path = state_file()?;
    let contents = fs::read_to_string(path).ok()?;
    let mut lines = contents.lines();
    let left = PathBuf::from(lines.next()?);
    let right = PathBuf::from(lines.next()?);

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
