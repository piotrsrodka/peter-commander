use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct LastDirs {
    pub left: PathBuf,
    pub right: PathBuf,
}

fn state_file() -> Option<PathBuf> {
    Some(
        dirs::config_dir()?
            .join("peter-commander")
            .join("last_dirs"),
    )
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
}
