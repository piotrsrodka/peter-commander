use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::Local;

/// The OS temp directory (`$TMPDIR`/`/tmp` on Unix, `%TEMP%` on Windows),
/// resolved once and cached — `std::env::temp_dir()` is portable, unlike a
/// hardcoded `/tmp` path.
fn log_path_cell() -> &'static PathBuf {
    static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
    LOG_PATH.get_or_init(|| std::env::temp_dir().join("peter-commander.log"))
}

/// Appends a timestamped line to the log file. Best-effort: if the log file
/// can't be opened/written, the error is silently dropped rather than
/// crashing the app over a logging failure.
pub fn log_error(message: &str) {
    log_to(log_path_cell(), message);
}

/// Same as `log_error`, for routine status confirmations (e.g. "Deleted
/// x.txt") rather than actual failures — kept in the same file/format so
/// "Show Logs" (Command menu) shows one merged, chronological view.
pub fn log_info(message: &str) {
    log_to(log_path_cell(), message);
}

/// The path `log_error`/`log_info` write to, for "Show Logs" to read back.
pub fn log_path() -> &'static Path {
    log_path_cell()
}

fn log_to(path: &Path, message: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "[{}] {}",
            Local::now().format("%Y-%m-%d %H:%M:%S"),
            message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_line_to_the_log() {
        // Uses a scratch path, not the real LOG_PATH, so running the test
        // suite never touches whatever the app itself is logging live.
        let path = std::env::temp_dir().join("pc_test_logging.log");
        let _ = std::fs::remove_file(&path);
        log_to(&path, "test message");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("test message"));
        std::fs::remove_file(&path).unwrap();
    }
}
