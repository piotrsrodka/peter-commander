//! Images in the quick-view pane, drawn with whatever the terminal offers:
//! Sixel, Kitty or iTerm2 graphics, or colored half blocks as a fallback.
//! Decoding and encoding can take a while for a big photo, so both happen on
//! a worker thread and the pane says "Loading" until the result is in.

#[cfg(unix)]
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::SystemTime;
#[cfg(unix)]
use std::time::{Duration, Instant};

use crate::logging;
use image::imageops::FilterType;
use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
#[cfg(unix)]
use ratatui_image::picker::cap_parser::Parser;
use ratatui_image::protocol::Protocol;

/// One picture at one size; a modified file or a resized pane needs a new
/// encode.
#[derive(Clone, PartialEq, Eq)]
struct Key {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: Size,
}

type Encoded = Result<Protocol, String>;

pub enum ImageState<'a> {
    Loading,
    Ready(&'a Protocol),
    Failed(&'a str),
}

#[derive(Default)]
pub struct ImagePreview {
    /// How to talk to this terminal; `None` until asked (`detect`).
    picker: Option<Picker>,
    finished: Option<(Key, Encoded)>,
    /// At most one worker at a time: holding an arrow key over a folder of
    /// photos shouldn't decode each of them, only whichever is selected when
    /// the current one is done.
    working: Option<(Key, Receiver<Encoded>)>,
}

impl ImagePreview {
    pub fn is_detected(&self) -> bool {
        self.picker.is_some()
    }

    /// Asks the terminal what it can do. It answers over stdin, so this runs
    /// between frames and only once an image is first previewed: a terminal
    /// that never answers costs a moment's wait then, not at every start.
    pub fn detect(&mut self) {
        // The full query reads stdin on a thread of its own and leaves it
        // blocked if no answer comes. It would then swallow the next reply or
        // key meant for us (a missing cursor position report is fatal to
        // `Terminal::clear`), so it only runs once something is known to
        // answer. Building this picker also lets graphics through tmux.
        let fallback = Picker::halfblocks();
        let answered = terminal_answers(fallback.tmux_detected());
        let picker = if answered {
            Picker::from_query_stdio().unwrap_or(fallback)
        } else {
            fallback
        };
        // Answers "why am I getting blocks?" from Ctrl+L.
        logging::log_info(&format!(
            "Image preview: {:?}, cell {:?}, tmux {}, terminal answered {answered}",
            picker.protocol_type(),
            picker.font_size(),
            picker.tmux_detected(),
        ));
        self.picker = Some(picker);
    }

    #[cfg(test)]
    pub fn set_picker(&mut self, picker: Picker) {
        self.picker = Some(picker);
    }

    pub fn is_loading(&self) -> bool {
        self.working.is_some()
    }

    /// What to show for `path` in `size` cells right now, starting the work
    /// if it hasn't been done yet. Called on every draw.
    pub fn state_for(&mut self, path: &Path, size: Size) -> ImageState<'_> {
        self.collect_finished();
        let key = Key {
            path: path.to_path_buf(),
            modified: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
            size,
        };
        if self.finished.as_ref().is_none_or(|(done, _)| *done != key) {
            self.start(&key);
            return ImageState::Loading;
        }
        match self.finished.as_ref() {
            Some((_, Ok(protocol))) => ImageState::Ready(protocol),
            Some((_, Err(message))) => ImageState::Failed(message),
            None => ImageState::Loading,
        }
    }

    fn collect_finished(&mut self) {
        let Some((key, receiver)) = &self.working else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err("the preview worker crashed".to_string()),
        };
        self.finished = Some((key.clone(), result));
        self.working = None;
    }

    fn start(&mut self, key: &Key) {
        let Some(picker) = self.picker.clone() else {
            return;
        };
        if self.working.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let path = key.path.clone();
        let size = key.size;
        thread::spawn(move || {
            let _ = sender.send(encode(&picker, &path, size));
        });
        self.working = Some((key.clone(), receiver));
    }
}

/// Sends a status request the way the full query would, and waits briefly for
/// the reply on this thread.
#[cfg(unix)]
fn terminal_answers(is_tmux: bool) -> bool {
    let (start, escape, end) = Parser::tmux_start_escape_end(is_tmux);
    let mut stdout = std::io::stdout();
    if write!(stdout, "{start}{escape}[5n{end}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return false;
    }
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    let mut received = Vec::new();
    while !contains_status_reply(&received) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        let mut stdin = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // Safety: one valid pollfd, and a buffer of the stated length.
        if unsafe { libc::poll(&mut stdin, 1, left.as_millis() as libc::c_int) } <= 0 {
            return false;
        }
        let mut buf = [0u8; 64];
        let read = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
        if read <= 0 {
            return false;
        }
        received.extend_from_slice(&buf[..read as usize]);
    }
    true
}

/// Elsewhere the full query has a fallback of its own for silent consoles.
#[cfg(not(unix))]
fn terminal_answers(_is_tmux: bool) -> bool {
    true
}

#[cfg(unix)]
const ANSWER_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(unix)]
fn contains_status_reply(received: &[u8]) -> bool {
    received.windows(4).any(|bytes| bytes == b"\x1b[0n")
}

fn encode(picker: &Picker, path: &Path, size: Size) -> Encoded {
    let picture = image::ImageReader::open(path)
        .map_err(|err| err.to_string())?
        .with_guessed_format()
        .map_err(|err| err.to_string())?
        .decode()
        .map_err(|err| err.to_string())?;
    picker
        .new_protocol(picture, size, Resize::Fit(Some(FilterType::Triangle)))
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(not(unix))]
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    #[test]
    fn recognises_the_status_reply_among_other_input() {
        assert!(contains_status_reply(b"x\x1b[0ny"));
        assert!(!contains_status_reply(b"\x1b[0"));
        assert!(!contains_status_reply(b"\x1b[3n"));
    }

    fn wait_until_done(preview: &mut ImagePreview, path: &Path, size: Size) {
        let start = Instant::now();
        while matches!(preview.state_for(path, size), ImageState::Loading) {
            assert!(start.elapsed() < Duration::from_secs(10), "still loading");
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn write_png(path: &Path, width: u32, height: u32) {
        image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 40]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn encodes_in_the_background_and_fits_the_area() {
        let dir = std::env::temp_dir().join("pc_test_image_preview_fit");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_png(&dir.join("a.png"), 400, 400);

        let mut preview = ImagePreview::default();
        preview.set_picker(Picker::halfblocks());
        let size = Size::new(30, 20);
        assert!(matches!(
            preview.state_for(&dir.join("a.png"), size),
            ImageState::Loading
        ));
        wait_until_done(&mut preview, &dir.join("a.png"), size);
        match preview.state_for(&dir.join("a.png"), size) {
            ImageState::Ready(protocol) => {
                let drawn = protocol.size();
                assert!(drawn.width <= 30 && drawn.height <= 20);
            }
            _ => panic!("expected a picture"),
        }
        assert!(!preview.is_loading());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn broken_file_reports_failure() {
        let dir = std::env::temp_dir().join("pc_test_image_preview_broken");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.png"), "not a picture").unwrap();

        let mut preview = ImagePreview::default();
        preview.set_picker(Picker::halfblocks());
        let size = Size::new(30, 20);
        wait_until_done(&mut preview, &dir.join("a.png"), size);
        assert!(matches!(
            preview.state_for(&dir.join("a.png"), size),
            ImageState::Failed(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn selection_changing_mid_work_ends_on_the_newest_image() {
        let dir = std::env::temp_dir().join("pc_test_image_preview_newest");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        write_png(&dir.join("a.png"), 50, 50);
        write_png(&dir.join("b.png"), 50, 50);

        let mut preview = ImagePreview::default();
        preview.set_picker(Picker::halfblocks());
        let size = Size::new(10, 10);
        let _ = preview.state_for(&dir.join("a.png"), size);
        wait_until_done(&mut preview, &dir.join("b.png"), size);
        assert!(matches!(
            preview.state_for(&dir.join("b.png"), size),
            ImageState::Ready(_)
        ));
        fs::remove_dir_all(&dir).unwrap();
    }
}
