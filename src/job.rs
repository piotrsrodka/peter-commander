//! A Copy/Move — or a Trash that has to copy (`DialogKind::TrashByCopy`) —
//! running on a worker thread, so the UI keeps redrawing (and can show a
//! progress bar, be sent to the background, or cancel it) while big files
//! are copied.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::app::DialogKind;
use crate::fs_ops::{self, Cancelled, Monitor};

/// What the worker has got through so far, shared with the UI thread.
#[derive(Debug, Default, Clone)]
pub struct Progress {
    /// Still adding up sizes before the first byte is copied.
    pub scanning: bool,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub files_done: u64,
    pub files_total: u64,
    /// Size of an item being worked on that reports no progress of its own
    /// (a Trash that copies) — the bar counts it as half done, so it shows
    /// movement instead of sitting at 0% for the whole of one big item.
    pub opaque_bytes: u64,
    /// Index (1-based) of the item being worked on.
    pub item: usize,
    /// The file currently being copied.
    pub current: String,
}

pub struct Outcome {
    pub done: usize,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

pub struct Job {
    pub kind: DialogKind,
    pub items: Vec<(String, PathBuf)>,
    pub src_dir: PathBuf,
    pub dest_dir: PathBuf,
    pub started: Instant,
    progress: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<Outcome>>,
}

impl Job {
    /// Starts copying/moving `items` into `dest_dir` right away. `kind`
    /// must be Copy, Move or TrashByCopy (for which `dest_dir` is only
    /// shown, the crate picks the trash itself).
    pub fn start(kind: DialogKind, items: Vec<(String, PathBuf)>, src_dir: PathBuf, dest_dir: PathBuf) -> Job {
        let progress = Arc::new(Mutex::new(Progress {
            scanning: true,
            ..Progress::default()
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = {
            let progress = Arc::clone(&progress);
            let cancel = Arc::clone(&cancel);
            let items = items.clone();
            let dest_dir = dest_dir.clone();
            thread::spawn(move || run(kind, &items, &dest_dir, &progress, &cancel))
        };
        Job {
            kind,
            items,
            src_dir,
            dest_dir,
            started: Instant::now(),
            progress,
            cancel,
            handle: Some(handle),
        }
    }

    pub fn progress(&self) -> Progress {
        self.progress.lock().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelling(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the worker (immediately, once `is_finished`) and returns
    /// how it went.
    pub fn join(mut self) -> Outcome {
        match self.handle.take().map(JoinHandle::join) {
            Some(Ok(outcome)) => outcome,
            _ => Outcome {
                done: 0,
                errors: vec!["background operation crashed".to_string()],
                cancelled: false,
            },
        }
    }
}

fn run(
    kind: DialogKind,
    items: &[(String, PathBuf)],
    dest_dir: &Path,
    progress: &Mutex<Progress>,
    cancel: &AtomicBool,
) -> Outcome {
    let mut outcome = Outcome {
        done: 0,
        errors: Vec::new(),
        cancelled: false,
    };

    let mut sizes = Vec::with_capacity(items.len());
    for (_, src) in items {
        match fs_ops::tree_size(src, cancel) {
            Ok(size) => sizes.push(size),
            Err(_) => {
                outcome.cancelled = true;
                return outcome;
            }
        }
    }
    if let Ok(mut p) = progress.lock() {
        p.scanning = false;
        p.bytes_total = sizes.iter().map(|s| s.bytes).sum();
        p.files_total = sizes.iter().map(|s| s.files).sum();
    }

    let on_progress = |path: &Path, bytes: u64| {
        if let Ok(mut p) = progress.lock() {
            p.bytes_done += bytes;
            if let Some(name) = path.file_name() {
                p.current = name.to_string_lossy().into_owned();
            }
        }
    };
    let on_file_done = || {
        if let Ok(mut p) = progress.lock() {
            p.files_done += 1;
        }
    };
    let monitor = Monitor {
        cancel: Some(cancel),
        on_progress: Some(&on_progress),
        on_file_done: Some(&on_file_done),
    };

    let mut bytes_before = 0;
    let mut files_before = 0;
    for (idx, ((name, src), size)) in items.iter().zip(&sizes).enumerate() {
        // Copy/Move also notice this inside an item; a Trash item can't
        // be interrupted, so between items is the only place it stops.
        if cancel.load(Ordering::Relaxed) {
            outcome.cancelled = true;
            return outcome;
        }
        if let Ok(mut p) = progress.lock() {
            p.item = idx + 1;
            p.current = name.clone();
            if kind == DialogKind::TrashByCopy {
                p.opaque_bytes = size.bytes;
            }
        }
        let dest = dest_dir.join(name);
        let result = match kind {
            DialogKind::Move => fs_ops::move_path_with(src, &dest, monitor).map(|_| ()),
            // Opaque to us: no progress inside an item, and no way to stop
            // one part-way — the crate copies first and deletes the
            // original last, so cancel waits for the item to finish
            // rather than leave a half copy in the trash.
            DialogKind::TrashByCopy => trash::delete(src).map_err(anyhow::Error::from),
            _ => fs_ops::copy_recursive_with(src, &dest, monitor),
        };
        match result {
            Ok(()) => outcome.done += 1,
            Err(err) if err.is::<Cancelled>() => {
                outcome.cancelled = true;
                return outcome;
            }
            Err(err) => outcome.errors.push(format!("{name}: {err}")),
        }
        // A plain rename (or a failed item) reports no bytes of its own;
        // snap to where this item should end so the bar stays honest.
        bytes_before += size.bytes;
        files_before += size.files;
        if let Ok(mut p) = progress.lock() {
            p.bytes_done = bytes_before;
            p.files_done = files_before;
            p.opaque_bytes = 0;
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{Duration, Instant};

    fn wait(job: Job) -> Outcome {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !job.is_finished() {
            assert!(Instant::now() < deadline, "job did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        job.join()
    }

    #[test]
    fn copies_and_moves_in_the_background() {
        let dir = std::env::temp_dir().join("pc_test_job");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/sub")).unwrap();
        fs::create_dir_all(dir.join("dest")).unwrap();
        fs::write(dir.join("src/a.txt"), "aaaa").unwrap();
        fs::write(dir.join("src/sub/b.txt"), "bb").unwrap();
        let items = vec![
            ("a.txt".to_string(), dir.join("src/a.txt")),
            ("sub".to_string(), dir.join("src/sub")),
        ];

        let job = Job::start(DialogKind::Copy, items.clone(), dir.join("src"), dir.join("dest"));
        let outcome = wait(job);
        assert_eq!((outcome.done, outcome.errors.len(), outcome.cancelled), (2, 0, false));
        assert_eq!(fs::read_to_string(dir.join("dest/sub/b.txt")).unwrap(), "bb");
        assert!(dir.join("src/a.txt").exists());

        fs::remove_dir_all(dir.join("dest")).unwrap();
        fs::create_dir_all(dir.join("dest")).unwrap();
        let job = Job::start(DialogKind::Move, items, dir.join("src"), dir.join("dest"));
        assert_eq!(wait(job).done, 2);
        assert!(!dir.join("src/a.txt").exists());
        assert_eq!(fs::read_to_string(dir.join("dest/a.txt")).unwrap(), "aaaa");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_are_collected_per_item() {
        let dir = std::env::temp_dir().join("pc_test_job_errors");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        let items = vec![
            ("missing".to_string(), dir.join("missing")),
            ("a.txt".to_string(), dir.join("a.txt")),
        ];
        let job = Job::start(DialogKind::Copy, items, dir.clone(), dir.join("out"));
        let outcome = wait(job);
        assert_eq!(outcome.done, 1);
        assert_eq!(outcome.errors.len(), 1);
        assert!(dir.join("out/a.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn cancelling_stops_before_the_next_item() {
        let dir = std::env::temp_dir().join("pc_test_job_cancel_between");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        let items = vec![("a.txt".to_string(), dir.join("a.txt"))];
        let job = Job::start(DialogKind::Copy, items, dir.clone(), dir.join("out"));
        // Usually lands before the worker reaches the item; either way the
        // outcome must be consistent with what's on disk.
        job.cancel();
        let outcome = wait(job);
        assert_eq!(outcome.cancelled, !dir.join("out/a.txt").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
