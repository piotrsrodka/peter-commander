use std::fs::OpenOptions;
use std::io::Write;

use chrono::Local;

const LOG_PATH: &str = "/tmp/peter-commander.log";

/// Appends a timestamped line to /tmp/peter-commander.log. Best-effort: if the
/// log file can't be opened/written, the error is silently dropped rather than
/// crashing the app over a logging failure.
pub fn log_error(message: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(LOG_PATH) {
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
        let _ = std::fs::remove_file(LOG_PATH);
        log_error("test message");
        let contents = std::fs::read_to_string(LOG_PATH).unwrap();
        assert!(contents.contains("test message"));
        let _ = std::fs::remove_file(LOG_PATH);
    }
}
