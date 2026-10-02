use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub fn copy_recursive(src: &Path, dest: &Path) -> Result<()> {
    ensure_not_into_itself(src, dest)?;
    copy_tree(src, dest)
}

/// Refuses copies that would destroy or endlessly re-copy their own source:
/// `fs::copy` of a file onto itself truncates it to zero bytes (and reports
/// success), and copying a directory into itself or one of its
/// subdirectories keeps descending into the copy being created until the
/// path gets too long.
fn ensure_not_into_itself(src: &Path, dest: &Path) -> Result<()> {
    let (Some(src_path), Some(dest_path)) = (resolve_parent(src), resolve_parent(dest)) else {
        return Ok(());
    };
    if src_path == dest_path {
        bail!("Source and destination are the same");
    }
    let src_is_real_dir = fs::symlink_metadata(src).is_ok_and(|meta| meta.is_dir());
    if src_is_real_dir && dest_path.starts_with(&src_path) {
        bail!("Cannot copy a directory into itself");
    }
    Ok(())
}

/// Whether two paths name the same entry (comparing a symlink as the link
/// itself), so callers can tell "copy onto itself" apart from "a different
/// file is already there".
pub fn is_same_path(a: &Path, b: &Path) -> bool {
    matches!((resolve_parent(a), resolve_parent(b)), (Some(a), Some(b)) if a == b)
}

/// Canonicalizes everything but the last component, so a symlink is
/// compared as the link itself rather than its target, and a destination
/// that doesn't exist yet can still be compared.
fn resolve_parent(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    Some(fs::canonicalize(parent).ok()?.join(name))
}

/// Symlinks are recreated as symlinks instead of followed (as `cp -r` does),
/// so a link to a big directory isn't silently copied in full and a link
/// pointing at one of its own parents can't loop.
fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_dest = dest.join(entry.file_name());
            copy_tree(&entry.path(), &child_dest)?;
        }
        // After the children, since creating them bumps the directory's
        // own modified time.
        preserve_modified(&meta, dest);
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if meta.file_type().is_symlink() {
            copy_symlink(src, dest)?;
        } else {
            fs::copy(src, dest)?;
            preserve_modified(&meta, dest);
        }
    }
    Ok(())
}

/// `fs::copy` keeps permissions but stamps the copy with the current time;
/// carry the source's modified time over instead, like Norton Commander and
/// `cp -p`, so "which one is newer" still means something after a copy.
/// Best effort: a filesystem that refuses it shouldn't fail the copy.
fn preserve_modified(src_meta: &fs::Metadata, dest: &Path) {
    if let Ok(modified) = src_meta.modified() {
        let _ = open_for_times(dest).and_then(|file| file.set_modified(modified));
    }
}

/// Unix sets times through any handle the owner holds, so a read-only open
/// works even on a read-only copy. Windows needs explicit write-attributes
/// access instead (which a read-only file still grants), plus backup
/// semantics to open a directory at all.
#[cfg(not(windows))]
fn open_for_times(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(windows)]
fn open_for_times(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(unix)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    std::os::unix::fs::symlink(fs::read_link(src)?, dest)?;
    Ok(())
}

#[cfg(windows)]
fn copy_symlink(src: &Path, dest: &Path) -> Result<()> {
    // Windows has separate file and directory symlinks; match the target's
    // kind, falling back to a file link when the target is missing.
    let target = fs::read_link(src)?;
    if fs::metadata(src).is_ok_and(|meta| meta.is_dir()) {
        std::os::windows::fs::symlink_dir(target, dest)?;
    } else {
        std::os::windows::fs::symlink_file(target, dest)?;
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

    #[test]
    fn refuses_to_copy_file_onto_itself() {
        let dir = std::env::temp_dir().join("pc_test_copy_onto_itself");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.txt");
        fs::write(&file, "important").unwrap();

        assert!(copy_recursive(&file, &file).is_err());

        assert_eq!(fs::read_to_string(&file).unwrap(), "important");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_to_copy_directory_onto_itself() {
        let dir = std::env::temp_dir().join("pc_test_copy_dir_onto_itself");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "keep").unwrap();

        assert!(copy_recursive(&src, &src).is_err());

        assert_eq!(fs::read_to_string(src.join("f.txt")).unwrap(), "keep");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_to_copy_directory_into_its_subdirectory() {
        let dir = std::env::temp_dir().join("pc_test_copy_into_subdir");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("a");
        fs::create_dir_all(src.join("inner")).unwrap();
        let dest = src.join("inner").join("a");

        assert!(copy_recursive(&src, &dest).is_err());

        assert!(!dest.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn refuses_to_move_directory_into_its_subdirectory() {
        let dir = std::env::temp_dir().join("pc_test_move_into_subdir");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("a");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::write(src.join("f.txt"), "keep").unwrap();
        let dest = src.join("inner").join("a");

        assert!(move_path(&src, &dest).is_err());

        assert_eq!(fs::read_to_string(src.join("f.txt")).unwrap(), "keep");
        assert!(!dest.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copies_into_sibling_with_shared_name_prefix() {
        // "ab" starts with "a" as a string but is not inside "a".
        let dir = std::env::temp_dir().join("pc_test_copy_prefix_sibling");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("a");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(dir.join("ab")).unwrap();
        fs::write(src.join("f.txt"), "x").unwrap();

        copy_recursive(&src, &dir.join("ab").join("a")).unwrap();

        assert_eq!(fs::read_to_string(dir.join("ab/a/f.txt")).unwrap(), "x");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copies_symlink_to_parent_as_symlink() {
        let dir = std::env::temp_dir().join("pc_test_copy_symlink_loop");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("s").join("b");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(dir.join("out")).unwrap();
        std::os::unix::fs::symlink("..", src.join("up")).unwrap();
        let dest = dir.join("out").join("b");

        copy_recursive(&src, &dest).unwrap();

        let copied = dest.join("up");
        assert!(fs::symlink_metadata(&copied).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(&copied).unwrap(), Path::new(".."));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copies_dangling_symlink() {
        let dir = std::env::temp_dir().join("pc_test_copy_dangling_symlink");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let link = dir.join("broken");
        std::os::unix::fs::symlink("does-not-exist", &link).unwrap();
        let dest = dir.join("broken-copy");

        copy_recursive(&link, &dest).unwrap();

        assert_eq!(fs::read_link(&dest).unwrap(), Path::new("does-not-exist"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copies_symlink_into_its_target_directory() {
        // The link is copied, not followed, so this is not a copy into itself.
        let dir = std::env::temp_dir().join("pc_test_copy_symlink_into_target");
        let _ = fs::remove_dir_all(&dir);
        let target = dir.join("target");
        fs::create_dir_all(&target).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        copy_recursive(&link, &target.join("link")).unwrap();

        assert!(fs::symlink_metadata(target.join("link")).unwrap().file_type().is_symlink());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn set_mtime(path: &Path, secs_ago: u64) -> std::time::SystemTime {
        let time = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        // Whole seconds, so filesystems with coarse timestamps compare equal.
        let secs = time.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        fs::File::open(path).unwrap().set_modified(time).unwrap();
        time
    }

    fn mtime(path: &Path) -> std::time::SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn copy_keeps_file_modified_time() {
        let dir = std::env::temp_dir().join("pc_test_copy_keeps_mtime");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("old.txt");
        fs::write(&src, "x").unwrap();
        let original = set_mtime(&src, 30 * 24 * 3600);
        let dest = dir.join("copy.txt");

        copy_recursive(&src, &dest).unwrap();

        assert_eq!(mtime(&dest), original);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn copy_keeps_directory_modified_time() {
        let dir = std::env::temp_dir().join("pc_test_copy_keeps_dir_mtime");
        let _ = fs::remove_dir_all(&dir);
        let src = dir.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("f.txt"), "x").unwrap();
        let original = set_mtime(&src, 7 * 24 * 3600);
        let dest = dir.join("dest");

        copy_recursive(&src, &dest).unwrap();

        assert_eq!(mtime(&dest), original);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copy_keeps_modified_time_of_read_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("pc_test_copy_keeps_readonly_mtime");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ro.txt");
        fs::write(&src, "x").unwrap();
        let original = set_mtime(&src, 3600);
        fs::set_permissions(&src, fs::Permissions::from_mode(0o444)).unwrap();
        let dest = dir.join("ro-copy.txt");

        copy_recursive(&src, &dest).unwrap();

        assert_eq!(mtime(&dest), original);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn same_path_detects_identical_and_distinct_entries() {
        let dir = std::env::temp_dir().join("pc_test_is_same_path");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "x").unwrap();

        assert!(is_same_path(&dir.join("a.txt"), &dir.join("sub/../a.txt")));
        assert!(!is_same_path(&dir.join("a.txt"), &dir.join("sub/a.txt")));
        fs::remove_dir_all(&dir).unwrap();
    }
}
