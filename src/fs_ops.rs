use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, bail};

/// Returned (wrapped in `anyhow::Error`) when a `Monitor`'s cancel flag
/// stops an operation part-way, so callers can tell "the user cancelled"
/// apart from a real failure via `err.is::<Cancelled>()`.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Progress callback: the file being copied and how many more bytes of it
/// were just written.
pub type ProgressFn<'a> = dyn Fn(&Path, u64) + Sync + 'a;

/// Optional hooks a background Copy/Move threads through the copy so it can
/// show progress and be cancelled. The default (no hooks) copies each file
/// with a single `fs::copy`, exactly as before.
#[derive(Default, Clone, Copy)]
pub struct Monitor<'a> {
    pub cancel: Option<&'a AtomicBool>,
    /// Called with the file being copied and how many more bytes of it
    /// were just written.
    pub on_progress: Option<&'a ProgressFn<'a>>,
    /// Called once each file (or symlink) has been copied in full.
    pub on_file_done: Option<&'a (dyn Fn() + Sync)>,
}

impl Monitor<'_> {
    fn is_active(&self) -> bool {
        self.cancel.is_some() || self.on_progress.is_some() || self.on_file_done.is_some()
    }

    fn file_done(&self) {
        if let Some(on_file_done) = self.on_file_done {
            on_file_done();
        }
    }

    fn check_cancelled(&self) -> Result<()> {
        if self.cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return Err(Cancelled.into());
        }
        Ok(())
    }

    fn report(&self, path: &Path, bytes: u64) {
        if let Some(on_progress) = self.on_progress {
            on_progress(path, bytes);
        }
    }
}

#[cfg(test)]
pub fn copy_recursive(src: &Path, dest: &Path) -> Result<()> {
    copy_recursive_with(src, dest, Monitor::default())
}

pub fn copy_recursive_with(src: &Path, dest: &Path, monitor: Monitor) -> Result<()> {
    ensure_not_into_itself(src, dest)?;
    copy_tree(src, dest, monitor)
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

/// Whether renaming `src` to `dest` would replace some *other* entry.
/// `fs::rename` silently overwrites an existing file, so Rename has to ask
/// first. On case-insensitive filesystems (macOS, Windows) `readme` →
/// `README` finds `dest` "existing" because it's `src` itself; comparing
/// file identity keeps that case-only rename allowed.
pub fn rename_target_taken(src: &Path, dest: &Path) -> bool {
    fs::symlink_metadata(dest).is_ok() && !is_same_file(src, dest)
}

#[cfg(unix)]
fn is_same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(windows)]
fn is_same_file(a: &Path, b: &Path) -> bool {
    // No stable file-id API on Windows; canonicalize returns the on-disk
    // spelling, so two spellings of one file compare equal.
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
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
fn copy_tree(src: &Path, dest: &Path, monitor: Monitor) -> Result<()> {
    monitor.check_cancelled()?;
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let child_dest = dest.join(entry.file_name());
            copy_tree(&entry.path(), &child_dest, monitor)?;
        }
        // After the children, since creating them bumps the directory's
        // own modified time.
        preserve_modified(&meta, dest);
    } else {
        // fs::copy on a named pipe or device reads until a writer shows up
        // or the device runs dry, which can be never; refuse instead of
        // freezing the UI.
        if !meta.is_file() && !meta.file_type().is_symlink() {
            bail!("Cannot copy a special file (pipe, socket or device)");
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if meta.file_type().is_symlink() {
            copy_symlink(src, dest)?;
            monitor.file_done();
        } else if monitor.is_active() {
            copy_file_monitored(src, dest, &meta, monitor)?;
            preserve_modified(&meta, dest);
            monitor.file_done();
        } else {
            fs::copy(src, dest)?;
            preserve_modified(&meta, dest);
        }
    }
    Ok(())
}

/// How much of a file is copied between progress reports/cancel checks.
const COPY_CHUNK: u64 = 8 * 1024 * 1024;

/// `fs::copy` in chunks, reporting progress and checking for cancellation
/// between them. Each chunk still goes through `io::copy` between two
/// `File`s, which std hands to the kernel (`copy_file_range` on Linux, so
/// btrfs/XFS can still share extents) rather than bouncing every byte
/// through a userspace buffer. A copy cancelled or failing part-way removes
/// the half-written file instead of leaving a truncated one behind — but
/// only once `dest` has actually been opened for writing: if the source
/// can't be read or an existing `dest` can't be replaced, `dest` is left
/// untouched, exactly like `fs::copy`.
fn copy_file_monitored(
    src: &Path,
    dest: &Path,
    meta: &fs::Metadata,
    monitor: Monitor,
) -> Result<()> {
    monitor.check_cancelled()?;
    let mut reader = fs::File::open(src)?;
    let mut writer = fs::File::create(dest)?;
    let result = (|| -> Result<()> {
        loop {
            monitor.check_cancelled()?;
            let copied = io::copy(&mut (&mut reader).take(COPY_CHUNK), &mut writer)?;
            if copied == 0 {
                break;
            }
            monitor.report(src, copied);
        }
        Ok(())
    })();
    if let Err(err) = result {
        drop(writer);
        let _ = fs::remove_file(dest);
        return Err(err);
    }
    drop(writer);
    fs::set_permissions(dest, meta.permissions())?;
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
    move_path_with(src, dest, Monitor::default()).map(|_| ())
}

/// Returns whether the move had to fall back to copy+delete (`false` means
/// a plain rename did it).
pub fn move_path_with(src: &Path, dest: &Path, monitor: Monitor) -> Result<bool> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    // fs::rename fails across filesystems/mount points, so fall back to
    // copy+delete. A cancelled copy returns Err here, before the source is
    // touched.
    if fs::rename(src, dest).is_err() {
        copy_recursive_with(src, dest, monitor)?;
        delete_recursive(src)?;
        return Ok(true);
    }
    Ok(false)
}

/// What `tree_size` adds up.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TreeSize {
    pub bytes: u64,
    /// Regular files and symlinks — the things `copy_tree` copies one by
    /// one.
    pub files: u64,
}

/// Bytes and file count under `path` (symlinks counted as links, not
/// followed, matching what `copy_tree` actually copies), so a background
/// Copy/Move can show how far along it is. Unreadable entries are skipped
/// rather than failing — the copy itself will report them.
pub fn tree_size(path: &Path, cancel: &AtomicBool) -> Result<TreeSize> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Cancelled.into());
    }
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(TreeSize::default());
    };
    if meta.is_dir() {
        let mut total = TreeSize::default();
        if let Ok(entries) = fs::read_dir(path) {
            for entry in entries.flatten() {
                let child = tree_size(&entry.path(), cancel)?;
                total.bytes += child.bytes;
                total.files += child.files;
            }
        }
        Ok(total)
    } else if meta.is_file() {
        Ok(TreeSize {
            bytes: meta.len(),
            files: 1,
        })
    } else if meta.file_type().is_symlink() {
        Ok(TreeSize { bytes: 0, files: 1 })
    } else {
        Ok(TreeSize::default())
    }
}

/// Whether moving entries of `dir` to the trash would make the `trash`
/// crate *copy* them (then delete the originals) rather than just rename
/// them — slow for anything big, and done without a word by the crate
/// itself. Mirrors its freedesktop logic:
/// - `dir` under the same mount point as the home trash → home trash;
/// - otherwise the per-volume trash (`$topdir/.Trash/$uid`, or
///   `$topdir/.Trash-$uid`, created if missing) → that, falling back to the
///   home trash when it can't be created (permission denied).
///
/// And whichever trash it lands on, a rename onto another filesystem (a
/// different `st_dev` — which includes another btrfs subvolume under the
/// same mount) fails, so the crate copies instead. Best effort: `false`
/// whenever it can't tell. Only Linux is checked; macOS and Windows hand
/// trashing to the OS.
#[cfg(target_os = "linux")]
pub fn trash_needs_copy(dir: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let (Ok(dir), Some(home_trash)) = (fs::canonicalize(dir), home_trash_dir()) else {
        return false;
    };
    let Ok(dir_dev) = fs::metadata(&dir).map(|meta| meta.dev()) else {
        return false;
    };
    // The home trash may not exist yet: judge it by its nearest existing
    // ancestor, which is where it would be created.
    let Some((home_trash_existing, home_trash_dev)) = home_trash
        .ancestors()
        .find_map(|path| Some((fs::canonicalize(path).ok()?, fs::metadata(path).ok()?.dev())))
    else {
        return false;
    };
    let mounts = mount_points();
    let topdir = mount_topdir(&dir, &mounts);
    if topdir == mount_topdir(&home_trash_existing, &mounts) {
        return dir_dev != home_trash_dev;
    }

    let uid = unsafe { libc::getuid() };
    let shared_trash = topdir.join(".Trash");
    let shared_trash_ok = fs::symlink_metadata(&shared_trash)
        .is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & 0o1000 != 0)
        && shared_trash.join(uid.to_string()).is_dir();
    let volume_trash_usable =
        shared_trash_ok || topdir.join(format!(".Trash-{uid}")).is_dir() || is_writable(topdir);
    let trash_dev = if volume_trash_usable {
        fs::metadata(topdir).map(|meta| meta.dev()).ok()
    } else {
        Some(home_trash_dev)
    };
    trash_dev.is_some_and(|dev| dev != dir_dev)
}

#[cfg(not(target_os = "linux"))]
pub fn trash_needs_copy(_dir: &Path) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
pub fn home_trash_dir() -> Option<PathBuf> {
    None
}

/// `$XDG_DATA_HOME/Trash`, or `~/.local/share/Trash` — where the `trash`
/// crate keeps the home trash.
#[cfg(target_os = "linux")]
pub fn home_trash_dir() -> Option<PathBuf> {
    match std::env::var_os("XDG_DATA_HOME") {
        Some(data_home) if !data_home.is_empty() => Some(PathBuf::from(data_home).join("Trash")),
        _ => Some(dirs::home_dir()?.join(".local/share/Trash")),
    }
}

/// Mount points from `/proc/mounts` (as the `trash` crate reads them),
/// with its octal escapes (`\040` for a space, ...) decoded.
#[cfg(target_os = "linux")]
fn mount_points() -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let Ok(text) = fs::read("/proc/mounts").or_else(|_| fs::read("/etc/mtab")) else {
        return Vec::new();
    };
    text.split(|&b| b == b'\n')
        .filter_map(|line| line.split(|&b| b == b' ').nth(1))
        .filter(|field| !field.is_empty())
        .map(|field| PathBuf::from(std::ffi::OsString::from_vec(unescape_mount_field(field))))
        .collect()
}

#[cfg(target_os = "linux")]
fn unescape_mount_field(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let octal = field
            .get(i + 1..i + 4)
            .filter(|digits| field[i] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)));
        match octal {
            Some(digits) => {
                out.push(
                    digits
                        .iter()
                        .fold(0u8, |acc, d| acc.wrapping_mul(8) + (d - b'0')),
                );
                i += 4;
            }
            None => {
                out.push(field[i]);
                i += 1;
            }
        }
    }
    out
}

/// The longest mount point `path` is under — the crate's "topdir".
#[cfg(target_os = "linux")]
fn mount_topdir<'a>(path: &Path, mounts: &'a [PathBuf]) -> &'a Path {
    mounts
        .iter()
        .filter(|mount| path.starts_with(mount))
        .max_by_key(|mount| mount.as_os_str().len())
        .map_or(Path::new("/"), PathBuf::as_path)
}

#[cfg(target_os = "linux")]
fn is_writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
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
        assert!(
            fs::symlink_metadata(&copied)
                .unwrap()
                .file_type()
                .is_symlink()
        );
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

        assert!(
            fs::symlink_metadata(target.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    fn set_mtime(path: &Path, secs_ago: u64) -> std::time::SystemTime {
        let time = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        // Whole seconds, so filesystems with coarse timestamps compare equal.
        let secs = time
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
        // Same opener as the code under test: a plain File::open can't set
        // times on Windows, or open a directory there at all.
        open_for_times(path).unwrap().set_modified(time).unwrap();
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

    #[test]
    fn rename_target_taken_only_for_another_existing_entry() {
        let dir = std::env::temp_dir().join("pc_test_rename_target_taken");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "draft").unwrap();
        fs::write(dir.join("b.txt"), "final").unwrap();

        assert!(rename_target_taken(&dir.join("a.txt"), &dir.join("b.txt")));
        assert!(!rename_target_taken(&dir.join("a.txt"), &dir.join("c.txt")));
        // Renaming to its own name is not a conflict.
        assert!(!rename_target_taken(&dir.join("a.txt"), &dir.join("a.txt")));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn case_only_rename_is_not_a_conflict() {
        // On a case-insensitive filesystem (macOS, Windows CI) "README.TXT"
        // resolves to "readme.txt" itself; on Linux it doesn't exist. Either
        // way the rename must stay allowed.
        let dir = std::env::temp_dir().join("pc_test_case_only_rename");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("readme.txt"), "x").unwrap();

        assert!(!rename_target_taken(
            &dir.join("readme.txt"),
            &dir.join("README.TXT")
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_copy_a_named_pipe() {
        let dir = std::env::temp_dir().join("pc_test_copy_fifo");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());

        // Without the guard this blocks forever waiting for a writer.
        assert!(copy_recursive(&fifo, &dir.join("pipe-copy")).is_err());

        assert!(!dir.join("pipe-copy").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_copy_a_socket() {
        let dir = std::env::temp_dir().join("pc_test_copy_socket");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        assert!(copy_recursive(&socket, &dir.join("sock-copy")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn noop_progress(_: &Path, _: u64) {}

    #[test]
    fn monitored_copy_reports_bytes_and_keeps_mtime() {
        use std::sync::atomic::AtomicU64;
        let dir = std::env::temp_dir().join("pc_test_monitored_copy");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/sub")).unwrap();
        fs::write(dir.join("src/a.txt"), vec![b'x'; 1000]).unwrap();
        fs::write(dir.join("src/sub/b.txt"), vec![b'y'; 234]).unwrap();
        let expected = set_mtime(&dir.join("src/a.txt"), 3600);

        let copied = AtomicU64::new(0);
        let on_progress = |_: &Path, bytes: u64| {
            copied.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
        };
        let cancel = AtomicBool::new(false);
        let files = AtomicU64::new(0);
        let on_file_done = || {
            files.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        };
        let monitor = Monitor {
            cancel: Some(&cancel),
            on_progress: Some(&on_progress),
            on_file_done: Some(&on_file_done),
        };
        copy_recursive_with(&dir.join("src"), &dir.join("dest"), monitor).unwrap();
        assert_eq!(files.load(std::sync::atomic::Ordering::Relaxed), 2);

        assert_eq!(copied.load(std::sync::atomic::Ordering::Relaxed), 1234);
        assert_eq!(
            fs::read(dir.join("dest/sub/b.txt")).unwrap(),
            vec![b'y'; 234]
        );
        assert_eq!(mtime(&dir.join("dest/a.txt")), expected);
        assert_eq!(
            tree_size(&dir.join("src"), &cancel).unwrap(),
            TreeSize {
                bytes: 1234,
                files: 2
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelled_copy_fails_with_cancelled_and_leaves_no_partial_file() {
        let dir = std::env::temp_dir().join("pc_test_cancelled_copy");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "data").unwrap();

        let cancel = AtomicBool::new(true);
        let monitor = Monitor {
            cancel: Some(&cancel),
            on_progress: Some(&noop_progress),
            on_file_done: None,
        };
        let err = copy_recursive_with(&dir.join("a.txt"), &dir.join("b.txt"), monitor).unwrap_err();
        assert!(err.is::<Cancelled>());
        assert!(!dir.join("b.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelled_cross_device_style_move_keeps_the_source() {
        // Calls the copy+delete fallback directly with a pre-set cancel
        // flag: the copy must fail before the source is deleted.
        let dir = std::env::temp_dir().join("pc_test_cancelled_move");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.txt"), "data").unwrap();

        let cancel = AtomicBool::new(true);
        let monitor = Monitor {
            cancel: Some(&cancel),
            on_progress: None,
            on_file_done: None,
        };
        let result = copy_recursive_with(&dir.join("src"), &dir.join("dest"), monitor)
            .and_then(|()| delete_recursive(&dir.join("src")));
        assert!(result.unwrap_err().is::<Cancelled>());
        assert_eq!(fs::read_to_string(dir.join("src/a.txt")).unwrap(), "data");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn monitored_copy_keeps_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("pc_test_monitored_perms");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("run.sh"), "#!/bin/sh").unwrap();
        fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();

        let monitor = Monitor {
            cancel: None,
            on_progress: Some(&noop_progress),
            on_file_done: None,
        };
        copy_recursive_with(&dir.join("run.sh"), &dir.join("copy.sh"), monitor).unwrap();
        let mode = fs::metadata(dir.join("copy.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn failed_monitored_copy_leaves_an_existing_destination_alone() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join("pc_test_monitored_keep_dest");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("src.txt"), "new").unwrap();
        fs::write(dir.join("dest.txt"), "old").unwrap();
        fs::set_permissions(dir.join("dest.txt"), fs::Permissions::from_mode(0o444)).unwrap();
        // Root ignores file modes, so this only means something as a user.
        let read_only_enforced = fs::OpenOptions::new()
            .write(true)
            .open(dir.join("dest.txt"))
            .is_err();

        let monitor = Monitor {
            cancel: None,
            on_progress: Some(&noop_progress),
            on_file_done: None,
        };
        let missing = copy_recursive_with(&dir.join("missing.txt"), &dir.join("dest.txt"), monitor);
        assert!(missing.is_err());
        assert_eq!(fs::read_to_string(dir.join("dest.txt")).unwrap(), "old");

        if read_only_enforced {
            assert!(
                copy_recursive_with(&dir.join("src.txt"), &dir.join("dest.txt"), monitor).is_err()
            );
            assert_eq!(fs::read_to_string(dir.join("dest.txt")).unwrap(), "old");
        }
        fs::set_permissions(dir.join("dest.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mount_fields_are_unescaped_and_longest_mount_wins() {
        assert_eq!(unescape_mount_field(b"/mnt/my\\040disk"), b"/mnt/my disk");
        assert_eq!(unescape_mount_field(b"/plain\\x"), b"/plain\\x");
        let mounts = vec![
            PathBuf::from("/"),
            PathBuf::from("/home"),
            PathBuf::from("/home/me/usb"),
        ];
        assert_eq!(
            mount_topdir(Path::new("/home/me/usb/a"), &mounts),
            Path::new("/home/me/usb")
        );
        assert_eq!(
            mount_topdir(Path::new("/home/me/x"), &mounts),
            Path::new("/home")
        );
        assert_eq!(mount_topdir(Path::new("/opt"), &mounts), Path::new("/"));
        assert_eq!(
            mount_topdir(Path::new("/homework"), &mounts),
            Path::new("/")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn trash_needs_copy_only_off_the_home_filesystem_without_a_usable_volume_trash() {
        // The home directory is on the home trash's own filesystem in any
        // normal setup, so trashing there is always a plain rename.
        if let Some(home) = dirs::home_dir() {
            assert!(!trash_needs_copy(&home));
        }
        // A missing directory can't be judged, so it isn't flagged.
        assert!(!trash_needs_copy(Path::new("/definitely/not/here")));
        // "/" is only flagged when it's a different filesystem from the
        // home trash *and* there's no writable way to a volume trash —
        // which is exactly what the crate itself would decide.
        let root_flagged = trash_needs_copy(Path::new("/"));
        if root_flagged {
            assert!(!is_writable(Path::new("/")));
        }
    }
}
