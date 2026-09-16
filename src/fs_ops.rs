use std::fs;
use std::path::Path;

use anyhow::Result;

pub fn copy_recursive(src: &Path, dest: &Path) -> Result<()> {
    if src.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_dest = dest.join(entry.file_name());
            copy_recursive(&entry.path(), &child_dest)?;
        }
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dest)?;
    }
    Ok(())
}

pub fn move_path(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // fs::rename fails across filesystems/mount points, so fall back to copy+delete.
    if fs::rename(src, dest).is_err() {
        copy_recursive(src, dest)?;
        delete_recursive(src)?;
    }
    Ok(())
}

pub fn delete_recursive(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_file() {
        let dir = std::env::temp_dir().join("pc_test_copy_file");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.txt");
        fs::write(&src, "hello").unwrap();
        let dest = dir.join("b.txt");

        copy_recursive(&src, &dest).unwrap();

        assert_eq!(fs::read_to_string(&dest).unwrap(), "hello");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_directory_recursively() {
        let dir = std::env::temp_dir().join("pc_test_copy_dir");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("file.txt"), "top").unwrap();
        fs::write(src.join("nested/n.txt"), "deep").unwrap();
        let dest = dir.join("dest");

        copy_recursive(&src, &dest).unwrap();

        assert_eq!(fs::read_to_string(dest.join("file.txt")).unwrap(), "top");
        assert_eq!(
            fs::read_to_string(dest.join("nested/n.txt")).unwrap(),
            "deep"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn deletes_file_and_directory() {
        let dir = std::env::temp_dir().join("pc_test_delete");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        let file = dir.join("f.txt");
        fs::write(&file, "x").unwrap();

        delete_recursive(&file).unwrap();
        assert!(!file.exists());

        delete_recursive(&dir.join("sub")).unwrap();
        assert!(!dir.join("sub").exists());

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn moves_file() {
        let dir = std::env::temp_dir().join("pc_test_move_file");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("a.txt");
        fs::write(&src, "hello").unwrap();
        let dest = dir.join("b.txt");

        move_path(&src, &dest).unwrap();

        assert!(!src.exists());
        assert_eq!(fs::read_to_string(&dest).unwrap(), "hello");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn moves_directory() {
        let dir = std::env::temp_dir().join("pc_test_move_dir");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src");
        fs::create_dir_all(src.join("nested")).unwrap();
        fs::write(src.join("nested/n.txt"), "deep").unwrap();
        let dest = dir.join("dest");

        move_path(&src, &dest).unwrap();

        assert!(!src.exists());
        assert_eq!(
            fs::read_to_string(dest.join("nested/n.txt")).unwrap(),
            "deep"
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}
