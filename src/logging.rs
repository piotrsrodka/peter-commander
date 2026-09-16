use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use chrono::Local;

const LOG_PATH: &str = "/tmp/peter-commander.log";

/// Appends a timestamped line to /tmp/peter-commander.log. Best-effort: if the
/// log file can't be opened/written, the error is silently dropped rather than
/// crashing the app over a logging failure.
pub fn log_error(message: &str) {
    log_to(Path::new(LOG_PATH), message);
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
