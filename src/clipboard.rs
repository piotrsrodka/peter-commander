//! Copy and paste for the built-in editor: the desktop's own clipboard on
//! Wayland (through `wl-copy`/`wl-paste`, so text goes to and from other
//! apps), and a clipboard of PC's own everywhere else — or whenever those
//! commands are missing or fail, so Ctrl+C/Ctrl+V within the editor always
//! work.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// How long Ctrl+V waits for `wl-paste` before using PC's own clipboard —
/// normally it answers in a few milliseconds.
const PASTE_TIMEOUT: Duration = Duration::from_millis(1000);

pub struct Clipboard {
    /// The last text copied, used whenever the desktop's clipboard can't be.
    text: String,
    /// Whether `wl-copy`/`wl-paste` are there to use (a Wayland session
    /// with both on the PATH), checked once.
    wayland: bool,
}

impl Clipboard {
    pub fn new() -> Self {
        let wayland = cfg!(unix)
            && std::env::var_os("WAYLAND_DISPLAY").is_some()
            && on_path("wl-copy")
            && on_path("wl-paste");
        Clipboard {
            text: String::new(),
            wayland,
        }
    }

    /// Only PC's own clipboard, never the desktop's — for tests.
    #[cfg(test)]
    pub fn internal() -> Self {
        Clipboard {
            text: String::new(),
            wayland: false,
        }
    }

    /// Keeps `text`, and hands it to the desktop on a separate thread: the
    /// editor shouldn't wait on `wl-copy`, which has nothing to report back.
    pub fn copy(&mut self, text: String) {
        if self.wayland {
            let for_desktop = text.clone();
            std::thread::spawn(move || {
                let child = Command::new("wl-copy")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                if let Ok(mut child) = child {
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(for_desktop.as_bytes());
                    }
                    let _ = child.wait();
                }
            });
        }
        self.text = text;
    }

    /// The desktop's clipboard text if there is any, else what was last
    /// copied here.
    pub fn paste(&self) -> String {
        if self.wayland
            && let Some(text) = wl_paste()
        {
            return text;
        }
        self.text.clone()
    }
}

/// `wl-paste`'s text, or `None` if it fails, has no text to give, or takes
/// too long (it's then killed, so it can't hang around).
fn wl_paste() -> Option<String> {
    let mut child = Command::new("wl-paste")
        .args(["--no-newline", "--type", "text"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(PASTE_TIMEOUT) {
        Ok(Ok(bytes)) => {
            let status = child.wait().ok()?;
            if !status.success() {
                return None;
            }
            String::from_utf8(bytes).ok()
        }
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            None
        }
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_clipboard_keeps_the_last_copy() {
        let mut clipboard = Clipboard::internal();
        assert_eq!(clipboard.paste(), "");
        clipboard.copy("zażółć".to_string());
        clipboard.copy("gęślą".to_string());
        assert_eq!(clipboard.paste(), "gęślą");
    }
}
