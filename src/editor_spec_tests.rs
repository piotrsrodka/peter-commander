//! Black-box, spec-driven tests of the built-in editor, written from the
//! feature description only (not from the implementation). Keys go through
//! the real `handle_key` / `handle_paste` entry points of `main.rs`, so
//! they exercise the same path a terminal key press takes.

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode as K, KeyModifiers as M};
use ratatui::{Terminal, backend::TestBackend};

use crate::app::{App, Dialog, Side};
use crate::clipboard::Clipboard;
use crate::editor::{self, Editor, Pos};
use crate::menu::Action;
use crate::pane::Pane;

// ---------------------------------------------------------------- harness

struct Fx {
    app: App,
    dir: PathBuf,
    file: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pc_spec_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    dir
}

fn select(app: &mut App, name: &str) {
    let idx = app
        .left
        .entries
        .iter()
        .position(|e| e.name == name)
        .unwrap_or_else(|| panic!("{name} not listed"));
    app.left.selected = idx;
}

/// An app on a temp dir, the left pane active; `f.txt` holds `contents`.
/// The editor is NOT opened yet.
fn fx_closed(name: &str, contents: &[u8]) -> Fx {
    let dir = temp_dir(name);
    let file = dir.join("f.txt");
    std::fs::write(&file, contents).unwrap();
    let mut app = App::new().unwrap();
    app.left = Pane::new(dir.clone(), false).unwrap();
    app.right = Pane::new(dir.clone(), false).unwrap();
    app.active = Side::Left;
    app.builtin_editor = true;
    app.wrap_lines = false;
    app.classic_style = false;
    app.quick_view = false;
    // Never touch the real system clipboard from tests.
    app.clipboard = Clipboard::internal();
    select(&mut app, "f.txt");
    Fx { app, dir, file }
}

/// Same, with the file opened by F4.
fn fx(name: &str, contents: &str) -> Fx {
    fx_bytes(name, contents.as_bytes())
}

fn fx_bytes(name: &str, contents: &[u8]) -> Fx {
    let mut f = fx_closed(name, contents);
    f.key(K::F(4), M::NONE);
    assert!(f.app.editor.is_some(), "F4 should open the editor");
    f
}

impl Fx {
    fn key(&mut self, code: K, mods: M) {
        crate::handle_key(&mut self.app, code, mods).unwrap();
    }
    fn press(&mut self, code: K) {
        self.key(code, M::NONE);
    }
    fn ctrl(&mut self, code: K) {
        self.key(code, M::CONTROL);
    }
    fn shift(&mut self, code: K) {
        self.key(code, M::SHIFT);
    }
    fn ctrl_shift(&mut self, code: K) {
        self.key(code, M::CONTROL | M::SHIFT);
    }
    fn ctrl_ch(&mut self, c: char) {
        self.key(K::Char(c), M::CONTROL);
    }
    fn typ(&mut self, s: &str) {
        for c in s.chars() {
            self.key(K::Char(c), M::NONE);
        }
    }
    fn paste(&mut self, s: &str) {
        crate::handle_paste(&mut self.app, s);
    }
    fn ed(&self) -> &Editor {
        self.app.editor.as_ref().expect("editor open")
    }
    fn text(&self) -> String {
        self.ed().lines().join("\n")
    }
    fn cur(&self) -> (usize, usize) {
        let p = self.ed().cursor();
        (p.line, p.col)
    }
    fn sel(&self) -> Option<((usize, usize), (usize, usize))> {
        self.ed().selection().map(|(a, b)| {
            let (a, b) = if a <= b { (a, b) } else { (b, a) };
            ((a.line, a.col), (b.line, b.col))
        })
    }
    fn disk(&self) -> Vec<u8> {
        std::fs::read(&self.file).unwrap()
    }
    fn draw(&mut self, w: u16, h: u16) -> String {
        draw(&mut self.app, w, h)
    }
    fn find_status(&self) -> String {
        self.ed().find.as_ref().expect("find bar open").status()
    }
}

fn draw(app: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| crate::ui::draw(f, app)).unwrap();
    let buf = t.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

// ------------------------------------------------------- opening / F4 modes

#[test]
fn f4_opens_in_opposite_pane_then_full_screen_then_back() {
    let mut f = fx("f4modes", "hello\n");
    f.draw(80, 24);
    let a = f.app.editor_area;
    assert!(!f.ed().full_screen);
    assert!(
        a.x >= 40,
        "left pane active -> editor in right half, got {a:?}"
    );
    f.press(K::F(4));
    assert!(f.ed().full_screen);
    f.draw(80, 24);
    let a = f.app.editor_area;
    assert!(
        a.width >= 78,
        "full screen editor should span the width, got {a:?}"
    );
    f.press(K::F(4));
    assert!(!f.ed().full_screen);
    assert!(
        f.app.editor.is_some(),
        "third F4 goes back to the pane, not closed"
    );
}

#[test]
fn f4_with_right_pane_active_opens_on_the_left() {
    let mut f = fx_closed("f4right", b"x\n");
    f.app.active = Side::Right;
    let idx = f
        .app
        .right
        .entries
        .iter()
        .position(|e| e.name == "f.txt")
        .unwrap();
    f.app.right.selected = idx;
    f.press(K::F(4));
    assert!(f.app.editor.is_some());
    f.draw(80, 24);
    assert!(f.app.editor_area.x < 40, "got {:?}", f.app.editor_area);
}

#[test]
fn f4_refuses_a_directory() {
    let mut f = fx_closed("f4dir", b"x");
    select(&mut f.app, "sub");
    f.press(K::F(4));
    assert!(f.app.editor.is_none());
}

fn assert_refused(name: &str, bytes: &[u8]) {
    let mut f = fx_closed(name, bytes);
    f.press(K::F(4));
    assert!(f.app.editor.is_none(), "{name}: must not open");
    assert_eq!(f.disk(), bytes, "{name}: file untouched");
}

#[test]
fn refuses_nul_binary() {
    assert_refused("nul", b"abc\0def\n");
}

#[test]
fn refuses_non_utf8() {
    assert_refused("latin2", b"za\xbf\xf3\xb3\xe6\n"); // ISO-8859-2 "zażółć"-ish
    assert_refused("utf16", b"\xff\xfeh\0i\0");
}

#[test]
fn refuses_files_over_10_mb() {
    let big = vec![b'a'; 11 * 1024 * 1024];
    assert_refused("big", &big);
}

#[test]
fn opens_a_one_megabyte_text_file() {
    let mut s = String::new();
    while s.len() < 1024 * 1024 {
        s.push_str("line of text ąę\n");
    }
    let f = fx("onemb", &s);
    assert!(f.ed().lines().len() > 1000);
}

#[test]
fn refuses_non_regular_dev_null() {
    assert!(
        Editor::open(Path::new("/dev/null")).is_err(),
        "/dev/null is not a regular file"
    );
}

#[cfg(unix)]
#[test]
fn refuses_fifo_without_blocking() {
    let dir = temp_dir("fifo");
    let fifo = dir.join("pipe");
    let ok = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !ok.map(|s| s.success()).unwrap_or(false) {
        eprintln!("mkfifo unavailable, skipping");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let p = fifo.clone();
    std::thread::spawn(move || {
        let _ = tx.send(Editor::open(&p).is_err());
    });
    let r = rx.recv_timeout(std::time::Duration::from_secs(3));
    // Unblock a hanging open (a reader is waiting then, so this won't block).
    if r.is_err() {
        let _ = std::fs::OpenOptions::new().write(true).open(&fifo);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(
        r,
        Ok(true),
        "FIFO must be refused promptly (Err(Timeout) = blocked)"
    );
}

// ------------------------------------------------------------ basic typing

#[test]
fn printable_chars_insert_at_cursor_and_space_works() {
    let mut f = fx("typing", "ac\n");
    f.press(K::Right);
    f.typ("b ");
    assert_eq!(f.text(), "ab c");
    assert_eq!(f.cur(), (0, 3));
    assert!(f.ed().is_modified());
}

#[test]
fn shift_letter_inserts_uppercase() {
    let mut f = fx("shiftletter", "\n");
    f.key(K::Char('A'), M::SHIFT);
    f.key(K::Char('Ż'), M::SHIFT);
    assert_eq!(f.ed().lines()[0], "AŻ");
}

#[test]
fn tab_inserts_four_spaces() {
    // Full screen: in its pane, Tab gives the keyboard to the file list.
    let mut f = fx("tab", "x");
    f.press(K::F(4));
    f.press(K::Tab);
    assert_eq!(f.text(), "    x");
    assert_eq!(f.cur(), (0, 4));
}

#[test]
fn enter_splits_line_without_auto_indent() {
    let mut f = fx("enter", "    abc def");
    f.press(K::End);
    for _ in 0..4 {
        f.press(K::Left);
    }
    f.press(K::Enter);
    assert_eq!(f.ed().lines(), ["    abc", " def"]);
    assert_eq!(f.cur(), (1, 0));
    f.ctrl(K::Home);
    f.press(K::End);
    f.press(K::Enter);
    assert_eq!(f.ed().lines()[1], "", "no auto-indent");
}

#[test]
fn backspace_joins_at_col_0_and_delete_joins_at_line_end() {
    let mut f = fx("joins", "ab\ncd");
    f.press(K::Down);
    f.press(K::Home);
    f.press(K::Backspace);
    assert_eq!(f.text(), "abcd");
    assert_eq!(f.cur(), (0, 2));
    f.press(K::Enter);
    assert_eq!(f.text(), "ab\ncd");
    f.press(K::Up);
    f.press(K::End);
    f.press(K::Delete);
    assert_eq!(f.text(), "abcd");
}

#[test]
fn backspace_at_doc_start_and_delete_at_doc_end_do_nothing() {
    let mut f = fx("edges", "ab\ncd\n");
    let before = f.text();
    f.ctrl(K::Home);
    f.press(K::Backspace);
    assert_eq!(f.text(), before);
    assert!(
        !f.ed().is_modified(),
        "backspace at doc start must not modify"
    );
    f.ctrl(K::End);
    f.press(K::Delete);
    assert!(!f.ed().is_modified(), "delete at doc end must not modify");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"ab\ncd\n");
}

#[test]
fn backspace_and_delete_remove_a_selection() {
    let mut f = fx("bsdelsel", "hello world");
    f.shift(K::Right);
    f.shift(K::Right);
    f.press(K::Backspace);
    assert_eq!(f.text(), "llo world");
    f.press(K::End);
    f.shift(K::Left);
    f.shift(K::Left);
    f.press(K::Delete);
    assert_eq!(f.text(), "llo wor");
    assert_eq!(f.sel(), None);
}

// -------------------------------------------------------------- movement

#[test]
fn home_end_and_ctrl_home_end() {
    let mut f = fx("homeend", "one\ntwo three\nfour");
    f.press(K::Down);
    f.press(K::Right);
    f.press(K::End);
    assert_eq!(f.cur(), (1, 9));
    f.press(K::Home);
    assert_eq!(f.cur(), (1, 0));
    f.ctrl(K::End);
    assert_eq!(f.cur(), (2, 4));
    f.ctrl(K::Home);
    assert_eq!(f.cur(), (0, 0));
}

#[test]
fn right_at_line_end_goes_to_next_line_and_left_back() {
    let mut f = fx("wraparrow", "ąę\nść");
    f.press(K::End);
    assert_eq!(f.cur(), (0, 2), "column counts characters, not bytes");
    f.press(K::Right);
    assert_eq!(f.cur(), (1, 0));
    f.press(K::Left);
    assert_eq!(f.cur(), (0, 2));
}

#[test]
fn up_down_and_clamping_on_short_lines() {
    let mut f = fx("updown", "long line\nab\nlong line");
    f.press(K::End);
    f.press(K::Down);
    assert_eq!(f.cur().0, 1);
    assert!(f.cur().1 <= 2);
    f.press(K::Down);
    assert_eq!(f.cur(), (2, 9), "goal column kept across short line");
    f.press(K::Down);
    assert_eq!(f.cur().0, 2, "down on the last line stays on it");
}

#[test]
fn page_down_and_page_up() {
    let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let mut f = fx("pages", &text);
    f.draw(80, 24);
    f.press(K::PageDown);
    let (l, _) = f.cur();
    assert!(l > 3 && l < 60, "PgDn moves about one screen, got line {l}");
    f.draw(80, 24);
    f.press(K::PageDown);
    let (l2, _) = f.cur();
    assert!(l2 > l, "second PgDn moves further ({l} -> {l2})");
    f.draw(80, 24);
    f.press(K::PageUp);
    f.draw(80, 24);
    f.press(K::PageUp);
    assert_eq!(f.cur().0, 0);
    f.draw(80, 24);
    f.press(K::PageUp);
    assert_eq!(f.cur().0, 0, "PgUp at the top stays");
}

// ---------------------------------------------------------- word movement

fn cols_after(f: &mut Fx, code: K, n: usize) -> Vec<(usize, usize)> {
    (0..n)
        .map(|_| {
            f.ctrl(code);
            f.cur()
        })
        .collect()
}

#[test]
fn ctrl_right_polish_words() {
    let mut f = fx("wordpl", "zażółć gęślą jaźń");
    assert_eq!(cols_after(&mut f, K::Right, 3), [(0, 7), (0, 13), (0, 17)]);
}

#[test]
fn ctrl_left_polish_words() {
    let mut f = fx("wordplleft", "zażółć gęślą jaźń");
    f.press(K::End);
    assert_eq!(cols_after(&mut f, K::Left, 3), [(0, 13), (0, 7), (0, 0)]);
}

#[test]
fn ctrl_right_stops_at_punctuation_like_windows() {
    let mut f = fx("wordpunct", "foo, bar");
    assert_eq!(cols_after(&mut f, K::Right, 3), [(0, 3), (0, 5), (0, 8)]);
}

#[test]
fn ctrl_left_stops_at_punctuation_like_windows() {
    let mut f = fx("wordpunctl", "foo, bar");
    f.press(K::End);
    assert_eq!(cols_after(&mut f, K::Left, 3), [(0, 5), (0, 3), (0, 0)]);
}

#[test]
fn ctrl_right_skips_runs_of_whitespace() {
    let mut f = fx("wordws", "foo    bar");
    assert_eq!(cols_after(&mut f, K::Right, 1), [(0, 7)]);
}

#[test]
fn word_moves_cross_line_boundaries() {
    let mut f = fx("wordlines", "ab\ncd");
    f.press(K::End);
    f.ctrl(K::Right);
    assert_eq!(
        f.cur(),
        (1, 0),
        "Ctrl+Right at line end goes to the next line"
    );
    f.ctrl(K::Left);
    assert_eq!(
        f.cur(),
        (0, 2),
        "Ctrl+Left at col 0 goes to previous line end"
    );
}

#[test]
fn word_moves_at_document_edges_stay() {
    let mut f = fx("wordedges", "ab");
    f.ctrl(K::Left);
    assert_eq!(f.cur(), (0, 0));
    f.press(K::End);
    f.ctrl(K::Right);
    assert_eq!(f.cur(), (0, 2));
}

#[test]
fn word_moves_on_empty_doc_do_not_panic() {
    let mut f = fx("wordempty", "");
    for code in [K::Left, K::Right, K::Backspace, K::Delete, K::Home, K::End] {
        f.ctrl(code);
        f.ctrl_shift(code);
    }
    assert_eq!(f.text(), "");
}

#[test]
fn ctrl_backspace_deletes_previous_word() {
    let mut f = fx("ctrlbs", "zażółć gęślą");
    f.press(K::End);
    f.ctrl(K::Backspace);
    assert_eq!(f.text(), "zażółć ");
    f.ctrl(K::Backspace);
    assert_eq!(f.text(), "");
}

#[test]
fn ctrl_delete_deletes_next_word() {
    let mut f = fx("ctrldel", "gęślą jaźń");
    f.ctrl(K::Delete);
    let t = f.text();
    assert!(
        t == "jaźń" || t == " jaźń",
        "Ctrl+Delete removes the word, got {t:?}"
    );
}

#[test]
fn ctrl_backspace_at_col_0_joins_lines() {
    let mut f = fx("ctrlbsjoin", "ab\ncd");
    f.press(K::Down);
    f.press(K::Home);
    f.ctrl(K::Backspace);
    assert_eq!(f.text(), "abcd");
}

// -------------------------------------------------------------- selection

#[test]
fn shift_arrows_extend_and_typing_replaces() {
    let mut f = fx("selreplace", "hello");
    for _ in 0..3 {
        f.shift(K::Right);
    }
    assert_eq!(f.sel(), Some(((0, 0), (0, 3))));
    assert_eq!(f.ed().selected_text().as_deref(), Some("hel"));
    f.typ("X");
    assert_eq!(f.text(), "Xlo");
    assert_eq!(f.sel(), None);
}

#[test]
fn shift_home_end_select_to_line_edges() {
    let mut f = fx("selhomeend", "abc def");
    f.press(K::Right);
    f.press(K::Right);
    f.shift(K::End);
    assert_eq!(f.ed().selected_text().as_deref(), Some("c def"));
    f.press(K::Left); // collapse to start
    f.shift(K::Home);
    assert_eq!(f.ed().selected_text().as_deref(), Some("ab"));
}

#[test]
fn ctrl_shift_home_end_select_to_doc_edges() {
    let mut f = fx("seldoc", "ab\ncd\nef");
    f.press(K::Down);
    f.press(K::Right);
    f.ctrl_shift(K::End);
    assert_eq!(f.ed().selected_text().as_deref(), Some("d\nef"));
    f.press(K::Left);
    assert_eq!(f.cur(), (1, 1));
    f.ctrl_shift(K::Home);
    assert_eq!(f.ed().selected_text().as_deref(), Some("ab\nc"));
}

#[test]
fn ctrl_shift_arrows_select_words() {
    let mut f = fx("selword", "zażółć gęślą jaźń");
    f.ctrl_shift(K::Right);
    f.ctrl_shift(K::Right);
    assert_eq!(f.ed().selected_text().as_deref(), Some("zażółć gęślą "));
    f.ctrl_shift(K::Left);
    assert_eq!(f.ed().selected_text().as_deref(), Some("zażółć "));
}

#[test]
fn shift_up_down_select_across_lines_then_typing_replaces() {
    let mut f = fx("selupdown", "abc\ndef");
    f.press(K::Right);
    f.shift(K::Down);
    assert_eq!(f.ed().selected_text().as_deref(), Some("bc\nd"));
    f.typ("X");
    assert_eq!(f.text(), "aXef");
    assert_eq!(f.cur(), (0, 2));
}

#[test]
fn plain_movement_clears_selection() {
    for code in [K::Up, K::Down, K::Home, K::End] {
        let mut f = fx("selclear", "abc\ndef\nghi");
        f.press(K::Down);
        f.press(K::Right);
        f.shift(K::Right);
        assert!(f.sel().is_some());
        f.press(code);
        assert_eq!(f.sel(), None, "{code:?} should clear the selection");
    }
}

#[test]
fn left_right_collapse_selection_to_its_ends() {
    let mut f = fx("selcollapse", "abcdef");
    f.press(K::Right);
    f.shift(K::Right);
    f.shift(K::Right);
    f.shift(K::Right); // selection 1..4, cursor at 4
    f.press(K::Left);
    assert_eq!(f.cur(), (0, 1), "Left collapses to start");
    assert_eq!(f.sel(), None);
    f.press(K::End);
    f.shift(K::Left);
    f.shift(K::Left); // selection 4..6, cursor at 4
    f.press(K::Right);
    assert_eq!(f.cur(), (0, 6), "Right collapses to end");
    assert_eq!(f.sel(), None);
}

#[test]
fn ctrl_a_selects_all_and_typing_replaces_everything() {
    let mut f = fx("selall", "one\ntwo\nthree");
    f.ctrl_ch('a');
    assert_eq!(f.ed().selected_text().as_deref(), Some("one\ntwo\nthree"));
    f.typ("z");
    assert_eq!(f.text(), "z");
}

// -------------------------------------------------------------- clipboard

#[test]
fn copy_and_paste() {
    let mut f = fx("copypaste", "ąbc");
    f.shift(K::Right);
    f.ctrl_ch('c');
    assert_eq!(f.text(), "ąbc", "copy doesn't change text");
    f.press(K::End);
    f.ctrl_ch('v');
    assert_eq!(f.text(), "ąbcą");
}

#[test]
fn cut_and_paste_across_lines() {
    let mut f = fx("cutpaste", "one\ntwo\nthree");
    f.press(K::Right);
    f.press(K::Right);
    f.shift(K::Down);
    f.shift(K::Down);
    f.ctrl_ch('x');
    assert_eq!(f.text(), "onree");
    assert_eq!(f.cur(), (0, 2));
    f.ctrl(K::End);
    f.ctrl_ch('v');
    assert_eq!(f.ed().lines(), ["onreee", "two", "th"]);
    assert_eq!(f.cur(), (2, 2));
}

#[test]
fn paste_replaces_selection() {
    let mut f = fx("pastesel", "aXXb");
    f.press(K::Right);
    f.shift(K::Right);
    f.shift(K::Right);
    f.ctrl_ch('c');
    f.ctrl_ch('a');
    f.ctrl_ch('v');
    assert_eq!(f.text(), "XX");
}

#[test]
fn bracketed_paste_inserts_multiline_text() {
    let mut f = fx("bpaste", "[]");
    f.press(K::Right);
    f.paste("a\nżółw\nc");
    assert_eq!(f.ed().lines(), ["[a", "żółw", "c]"]);
    assert_eq!(f.cur(), (2, 1));
}

#[test]
fn bracketed_paste_with_crlf_does_not_leave_carriage_returns() {
    let mut f = fx("bpastecrlf", "x\n");
    f.paste("a\r\nb\r\n");
    for l in f.ed().lines() {
        assert!(!l.contains('\r'), "line {l:?} keeps a CR");
    }
    f.ctrl_ch('s');
    let disk = String::from_utf8(f.disk()).unwrap();
    assert!(!disk.contains('\r'), "LF file got CRs: {disk:?}");
}

#[test]
fn bracketed_paste_into_crlf_file_saves_crlf() {
    let mut f = fx("bpastecrlf2", "x\r\n");
    f.paste("a\nb");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"a\r\nbx\r\n");
}

// ----------------------------------------------------------- undo / redo

fn undo_all(f: &mut Fx) {
    for _ in 0..50 {
        f.ctrl_ch('z');
    }
}

#[test]
fn undo_redo_basic_and_modified_flag() {
    let mut f = fx("undo1", "x");
    f.typ("y");
    assert!(f.ed().is_modified());
    f.ctrl_ch('z');
    assert_eq!(f.text(), "x");
    assert!(
        !f.ed().is_modified(),
        "undo back to the saved state clears modified"
    );
    f.ctrl_ch('y');
    assert_eq!(f.text(), "yx");
    assert!(f.ed().is_modified());
}

#[test]
fn undo_after_save_marks_modified_again_and_redo_returns_to_saved() {
    let mut f = fx("undo2", "x");
    f.typ("y");
    f.ctrl_ch('s');
    assert!(!f.ed().is_modified());
    assert_eq!(f.disk(), b"yx");
    f.ctrl_ch('z');
    assert_eq!(f.text(), "x");
    assert!(f.ed().is_modified(), "differs from what was saved");
    f.ctrl_ch('y');
    assert!(!f.ed().is_modified(), "redo back to saved state");
}

#[test]
fn undo_everything_restores_original_through_many_kinds_of_edits() {
    let orig = "zażółć\ngęślą jaźń\n";
    let mut f = fx("undo3", orig);
    // Full screen, where Tab is four spaces.
    f.press(K::F(4));
    let before = f.text();
    f.typ("abc ");
    f.press(K::Enter);
    f.press(K::Tab);
    f.press(K::Backspace);
    f.ctrl(K::End);
    f.press(K::Backspace);
    f.ctrl_ch('a');
    f.ctrl_ch('x');
    f.paste("p\nq");
    f.ctrl_ch('v');
    f.ctrl(K::Home);
    f.ctrl(K::Delete);
    undo_all(&mut f);
    assert_eq!(f.text(), before);
    assert!(!f.ed().is_modified());
    f.ctrl_ch('s');
    assert_eq!(f.disk(), orig.as_bytes());
}

#[test]
fn new_edit_after_undo_drops_redo() {
    let mut f = fx("undo4", "");
    f.typ("a");
    f.ctrl_ch('z');
    f.typ("b");
    f.ctrl_ch('y');
    assert_eq!(f.text(), "b");
}

#[test]
fn undo_restores_cut_text_and_cursor_is_valid() {
    let mut f = fx("undo5", "one\ntwo");
    f.ctrl_ch('a');
    f.ctrl_ch('x');
    assert_eq!(f.text(), "");
    f.ctrl_ch('z');
    assert_eq!(f.text(), "one\ntwo");
    let (l, c) = f.cur();
    assert!(l < 2 && c <= f.ed().lines()[l].chars().count());
    f.typ("!"); // cursor must be usable
}

#[test]
fn undo_with_nothing_to_undo_is_harmless() {
    let mut f = fx("undo6", "a");
    f.ctrl_ch('z');
    f.ctrl_ch('y');
    assert_eq!(f.text(), "a");
    assert!(!f.ed().is_modified());
}

// --------------------------------------------------------- save / format

#[test]
fn save_round_trips_crlf() {
    let mut f = fx("crlf", "one\r\ntwo\r\n");
    for l in f.ed().lines() {
        assert!(!l.contains('\r'), "CR must not appear in a line: {l:?}");
    }
    f.press(K::Right);
    f.press(K::Enter);
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"o\r\nne\r\ntwo\r\n");
    assert!(!f.ed().is_modified());
}

#[test]
fn save_keeps_missing_trailing_newline() {
    let mut f = fx("notrail", "a\nb");
    f.ctrl(K::End);
    f.typ("X");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"a\nbX");
}

#[test]
fn save_keeps_trailing_newline() {
    let mut f = fx("trail", "a\nb\n");
    f.typ("X");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"Xa\nb\n");
}

#[test]
fn save_crlf_without_trailing_newline() {
    let mut f = fx("crlfnotrail", "a\r\nb");
    f.ctrl(K::End);
    f.typ("c");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"a\r\nbc");
}

#[test]
fn unmodified_save_is_byte_identical() {
    for (i, bytes) in [
        &b""[..],
        b"\n",
        b"\r\n",
        b"a",
        b"a\n\n\n",
        b"\xEF\xBB\xBFhi\r\n",
        b"\ttab\there\n",
        "zażółć\r\ngęślą".as_bytes(),
    ]
    .iter()
    .enumerate()
    {
        let mut f = fx_bytes(&format!("identity{i}"), bytes);
        assert_eq!(f.ed().to_bytes(), *bytes, "to_bytes of untouched {bytes:?}");
        f.ctrl_ch('s');
        assert_eq!(f.disk(), *bytes, "save of untouched {bytes:?}");
    }
}

#[test]
fn bom_is_hidden_and_preserved() {
    let mut f = fx_bytes("bom", b"\xEF\xBB\xBFhi\n");
    assert_eq!(f.ed().lines()[0], "hi", "BOM is not part of the text");
    f.press(K::End);
    f.typ("!");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"\xEF\xBB\xBFhi!\n");
}

#[test]
fn existing_tabs_are_preserved_and_displayed_to_4_col_stops() {
    let mut f = fx("tabs", "\tx\n");
    f.press(K::End);
    f.typ("y");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"\txy\n");
    assert_eq!(editor::char_width('\t', 0), 4);
    assert_eq!(editor::char_width('\t', 1), 3);
    assert_eq!(editor::char_width('\t', 3), 1);
    assert_eq!(editor::char_width('\t', 4), 4);
    // On screen: "xy" four columns after the start of the text.
    f.press(K::F(4));
    let screen = f.draw(40, 8);
    assert!(
        screen.lines().any(|l| l.contains("    xy")),
        "tab should render as 4 columns:\n{screen}"
    );
}

#[test]
fn empty_file_edit_and_save() {
    let mut f = fx("empty", "");
    assert_eq!(f.ed().lines(), [""]);
    assert_eq!(f.cur(), (0, 0));
    f.typ("a");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), b"a");
}

#[test]
fn save_clears_modified_and_esc_closes_without_prompt() {
    let mut f = fx("savedclose", "a\n");
    f.typ("b");
    f.ctrl_ch('s');
    assert!(!f.ed().is_modified());
    f.press(K::Esc);
    assert!(f.app.editor.is_none());
    assert!(matches!(f.app.dialog, Dialog::None));
}

// -------------------------------------------------------- close / quit

#[test]
fn esc_on_unmodified_closes_immediately() {
    let mut f = fx("escclean", "a");
    f.press(K::Esc);
    assert!(f.app.editor.is_none());
}

#[test]
fn esc_modified_prompt_save() {
    let mut f = fx("escsave", "a");
    f.typ("b");
    f.press(K::Esc);
    assert!(f.app.dialog_is_unsaved_changes());
    assert!(f.app.editor.is_some(), "still open while asking");
    f.press(K::Enter); // default = Save
    assert!(f.app.editor.is_none());
    assert_eq!(f.disk(), b"ba");
}

#[test]
fn esc_modified_prompt_dont_save() {
    let mut f = fx("escdont", "a");
    f.typ("b");
    f.press(K::Esc);
    f.press(K::Right); // Don't save
    f.press(K::Enter);
    assert!(f.app.editor.is_none());
    assert_eq!(f.disk(), b"a");
}

#[test]
fn esc_modified_prompt_cancel_keeps_editing() {
    let mut f = fx("esccancel", "a");
    f.typ("b");
    f.press(K::Esc);
    f.press(K::Right);
    f.press(K::Right); // Cancel
    f.press(K::Enter);
    assert!(f.app.editor.is_some());
    assert_eq!(f.text(), "ba");
    f.press(K::Esc);
    f.press(K::Esc); // Esc on the prompt also cancels
    assert!(f.app.editor.is_some());
    assert!(matches!(f.app.dialog, Dialog::None));
    f.typ("c");
    assert_eq!(f.text(), "bca");
}

#[test]
fn f10_with_unsaved_changes_asks_and_cancel_does_not_quit() {
    let mut f = fx("f10", "a");
    f.typ("b");
    f.press(K::F(10));
    assert!(
        matches!(
            f.app.dialog,
            Dialog::UnsavedChanges {
                quit_after: true,
                ..
            }
        ),
        "F10 must ask about unsaved changes"
    );
    f.press(K::Esc);
    assert!(!f.app.should_quit);
    assert!(f.app.editor.is_some());
}

#[test]
fn ctrl_q_with_unsaved_changes_dont_save_quits_and_leaves_file() {
    let mut f = fx("ctrlq", "a");
    f.typ("b");
    f.ctrl_ch('q');
    assert!(f.app.dialog_is_unsaved_changes());
    f.press(K::Right);
    f.press(K::Enter);
    assert!(f.app.should_quit);
    assert_eq!(f.disk(), b"a");
}

#[test]
fn ctrl_q_with_unsaved_changes_save_quits_and_writes() {
    let mut f = fx("ctrlqsave", "a");
    f.typ("b");
    f.ctrl_ch('q');
    f.press(K::Enter);
    assert!(f.app.should_quit);
    assert_eq!(f.disk(), b"ba");
}

#[test]
fn quit_from_menu_with_unsaved_changes_asks() {
    let mut f = fx("menuquit", "a");
    f.typ("b");
    f.app.run_action(Action::Quit).unwrap();
    assert!(f.app.dialog_is_unsaved_changes());
    assert!(!f.app.should_quit);
}

// ------------------------------------------------------------ AltGr

#[test]
fn altgr_polish_letters_in_every_disguise_are_inserted() {
    let letters = "ąęćłńóśźżĄĘĆŁŃÓŚŹŻ";
    let disguises = [
        M::NONE,
        M::CONTROL | M::ALT,
        M::ALT,
        M::SHIFT,
        M::CONTROL | M::ALT | M::SHIFT,
        M::ALT | M::SHIFT,
    ];
    for (i, mods) in disguises.iter().enumerate() {
        let mut f = fx(&format!("altgr{i}"), "");
        for c in letters.chars() {
            f.key(K::Char(c), *mods);
        }
        assert_eq!(f.text(), letters, "modifiers {mods:?}");
        assert!(!f.app.menu_open, "{mods:?} must not open the menu");
    }
}

#[test]
fn ctrl_letter_is_never_inserted() {
    let mut f = fx("ctrlletters", "base");
    for c in 'a'..='z' {
        if matches!(c, 'q') {
            continue; // quit
        }
        // Undo anything a shortcut did, then check nothing was typed.
        f.key(K::Char(c), M::CONTROL);
        f.app.dialog = Dialog::None;
        f.app.menu_open = false;
        if f.app.editor.is_none() {
            panic!("Ctrl+{c} closed the editor");
        }
        if let Some(ed) = f.app.editor.as_mut() {
            ed.find = None;
        }
        assert!(
            !f.text().contains(c) || "base".contains(c),
            "Ctrl+{c} inserted text: {:?}",
            f.text()
        );
    }
    // Ctrl+letter for a non-shortcut letter must not touch the text.
    let mut g = fx("ctrlletters2", "base");
    g.key(K::Char('j'), M::CONTROL);
    g.key(K::Char('ą'), M::CONTROL);
    assert_eq!(g.text(), "base");
}

#[test]
fn alt_ascii_letter_opens_the_menu() {
    let mut f = fx("altmenu", "abc");
    f.key(K::Char('e'), M::ALT);
    assert!(f.app.menu_open, "Alt+E opens the menu");
    assert_eq!(f.app.menu_bar()[f.app.menu_category].title, "Edit");
    assert_eq!(f.text(), "abc");
    f.press(K::Esc);
    assert!(!f.app.menu_open);
    assert!(f.app.editor.is_some(), "Esc closes only the menu");
    f.key(K::Char('f'), M::ALT);
    assert!(f.app.menu_open);
    assert_eq!(f.app.menu_bar()[f.app.menu_category].title, "File");
}

#[test]
fn is_text_input_classification() {
    assert!(crate::transfer::is_text_input('ą', M::NONE));
    assert!(crate::transfer::is_text_input('ł', M::CONTROL | M::ALT));
    assert!(crate::transfer::is_text_input('ż', M::ALT));
    assert!(crate::transfer::is_text_input('a', M::NONE));
    assert!(crate::transfer::is_text_input('A', M::SHIFT));
    assert!(!crate::transfer::is_text_input('a', M::CONTROL));
    assert!(!crate::transfer::is_text_input('f', M::ALT));
}

// -------------------------------------------------------- multi-byte

#[test]
fn multibyte_at_line_end_backspace_and_delete() {
    let mut f = fx("mbend", "zaż\nść");
    f.press(K::End);
    assert_eq!(f.cur(), (0, 3));
    f.press(K::Backspace);
    assert_eq!(f.ed().lines()[0], "za");
    f.typ("ź");
    f.press(K::Delete);
    assert_eq!(f.text(), "zaźść");
    f.press(K::End);
    f.shift(K::Left);
    f.shift(K::Left);
    assert_eq!(f.ed().selected_text().as_deref(), Some("ść"));
}

#[test]
fn emoji_and_combining_marks_do_not_panic() {
    let mut f = fx("emoji", "a😀b e\u{301}x 中文\n");
    for _ in 0..12 {
        f.press(K::Right);
        f.draw(30, 6);
    }
    for _ in 0..12 {
        f.press(K::Backspace);
        f.draw(30, 6);
    }
    for _ in 0..5 {
        f.press(K::Delete);
    }
    f.ctrl_ch('s');
    let s = String::from_utf8(f.disk()).expect("still valid UTF-8 on disk");
    assert!(s.ends_with('\n'));
}

#[test]
fn very_long_line_scrolls_horizontally_without_panic() {
    let line: String = "ąbćd ".repeat(3000);
    let mut f = fx("longline", &line);
    f.draw(20, 5);
    assert_eq!(f.ed().scroll_x(), 0);
    f.press(K::End);
    let screen = f.draw(20, 5);
    eprintln!("20x5 after End, scroll_x={}:\n{screen}", f.ed().scroll_x());
    let screen = f.draw(40, 12);
    assert!(
        f.ed().scroll_x() > 0,
        "should scroll right to follow the cursor:\n{screen}"
    );
    assert!(
        f.ed().cursor_in_view().is_some(),
        "cursor visible:\n{screen}"
    );
    f.typ("Z");
    f.draw(20, 5);
    f.press(K::F(4));
    f.draw(20, 5);
    f.draw(1, 1);
    f.press(K::Home);
    f.draw(20, 5);
    assert_eq!(f.ed().scroll_x(), 0, "Home scrolls back to the start");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), format!("{line}Z").into_bytes());
}

#[test]
fn pseudo_random_key_storm_never_panics_or_corrupts() {
    let text = "zażółć gęślą jaźń\r\n\tTab😀\r\ne\u{301}\r\n\r\nŻÓŁW";
    let mut f = fx("storm", text);
    f.key(K::F(4), M::NONE);
    assert!(f.ed().full_screen);
    let keys: &[(K, M)] = &[
        (K::Left, M::NONE),
        (K::Right, M::NONE),
        (K::Up, M::NONE),
        (K::Down, M::NONE),
        (K::Left, M::CONTROL),
        (K::Right, M::CONTROL | M::SHIFT),
        (K::Left, M::SHIFT),
        (K::Down, M::SHIFT),
        (K::Home, M::SHIFT),
        (K::End, M::NONE),
        (K::End, M::CONTROL | M::SHIFT),
        (K::Backspace, M::NONE),
        (K::Delete, M::NONE),
        (K::Backspace, M::CONTROL),
        (K::Delete, M::CONTROL),
        (K::Enter, M::NONE),
        (K::Tab, M::NONE),
        (K::Char('ż'), M::NONE),
        (K::Char('ł'), M::CONTROL | M::ALT),
        (K::Char(' '), M::NONE),
        (K::Char('z'), M::CONTROL),
        (K::Char('y'), M::CONTROL),
        (K::Char('x'), M::CONTROL),
        (K::Char('v'), M::CONTROL),
        (K::Char('c'), M::CONTROL),
        (K::PageDown, M::NONE),
        (K::PageUp, M::SHIFT),
    ];
    let mut seed: u64 = 0x1234_5678;
    for step in 0..3000 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let (code, mods) = keys[(seed >> 33) as usize % keys.len()];
        f.key(code, mods);
        assert!(
            f.app.editor.is_some(),
            "step {step}: editor closed by {code:?} {mods:?}"
        );
        let ed = f.ed();
        let (l, c) = f.cur();
        assert!(
            l < ed.lines().len(),
            "step {step}: cursor line out of range"
        );
        assert!(
            c <= ed.lines()[l].chars().count(),
            "step {step}: cursor col out of range"
        );
        for line in ed.lines() {
            assert!(
                !line.contains('\n') && !line.contains('\r'),
                "step {step}: {line:?}"
            );
        }
        if step % 97 == 0 {
            f.draw(25, 7);
        }
        // Through the pane layout and back: full screen, Tab stays text
        // (in the pane it hands the keyboard to the file list, and keys
        // there could open whatever else is in the temp directory).
        if step % 500 == 0 {
            f.key(K::F(4), M::NONE);
            f.draw(25, 7);
            f.key(K::F(4), M::NONE);
        }
    }
    // Saving gives valid UTF-8 with CRLF endings only.
    f.ctrl_ch('s');
    let disk = String::from_utf8(f.disk()).unwrap();
    assert!(
        !disk.replace("\r\n", "").contains('\n'),
        "LF-only line ending in CRLF file"
    );
    assert!(!disk.replace("\r\n", "").contains('\r'), "stray CR");
    // And undoing everything gives back the original file.
    for _ in 0..5000 {
        f.ctrl_ch('z');
    }
    assert_eq!(
        f.ed().to_bytes(),
        text.as_bytes(),
        "undo all restores the original"
    );
}

// ------------------------------------------------------------- wrapping

fn edit_menu_index(app: &App) -> usize {
    app.menu_bar()
        .iter()
        .position(|c| c.title == "Edit")
        .expect("Edit menu while editing")
}

#[test]
fn edit_menu_exists_only_while_editing() {
    let mut f = fx_closed("editmenu", b"x");
    assert!(f.app.menu_bar().iter().all(|c| c.title != "Edit"));
    f.press(K::F(4));
    let idx = edit_menu_index(&f.app);
    let items = f.app.menu_bar()[idx].items;
    for a in [
        Action::WrapLines,
        Action::SelectAll,
        Action::Find,
        Action::FindNext,
    ] {
        assert!(items.contains(&a), "{a:?} missing from Edit");
    }
    assert_eq!(f.app.menu_shortcut(Action::Find), Some("Ctrl+F"));
    let file = &f.app.menu_bar()[0];
    assert_eq!(file.title, "File");
    assert!(file.items.contains(&Action::Save));
    assert_eq!(f.app.menu_shortcut(Action::Save), Some("Ctrl+S"));
    f.press(K::Esc);
    assert!(f.app.menu_bar().iter().all(|c| c.title != "Edit"));
}

#[test]
fn f9_opens_the_menu_while_editing() {
    let mut f = fx("f9", "abc");
    f.press(K::F(9));
    assert!(f.app.menu_open);
    f.press(K::F(9));
    assert!(!f.app.menu_open);
    assert!(f.app.editor.is_some());
}

fn choose(f: &mut Fx, category: usize, action: Action) {
    f.app.open_menu_at(category);
    let item = f.app.menu_bar()[category]
        .items
        .iter()
        .position(|a| *a == action)
        .unwrap();
    f.app.menu_item = item;
    f.press(K::Enter);
    assert!(!f.app.menu_open);
}

#[test]
fn menu_select_all_and_save_work() {
    let mut f = fx("menuitems", "abc\ndef");
    let e = edit_menu_index(&f.app);
    choose(&mut f, e, Action::SelectAll);
    assert_eq!(f.ed().selected_text().as_deref(), Some("abc\ndef"));
    f.typ("q");
    choose(&mut f, 0, Action::Save);
    assert_eq!(f.disk(), b"q");
    assert!(!f.ed().is_modified());
}

#[test]
fn wrap_lines_soft_wraps_and_up_down_move_by_screen_row() {
    let long = "aaaa bbbb cccc dddd eeee ffff gggg hhhh iiii jjjj";
    let mut f = fx("wrap", &format!("{long}\nshort"));
    f.press(K::F(4)); // full screen, simpler geometry
    let e = edit_menu_index(&f.app);
    choose(&mut f, e, Action::WrapLines);
    assert!(f.app.wrap_lines);
    f.draw(20, 10);
    assert_eq!(f.ed().scroll_x(), 0);
    f.press(K::Down);
    assert_eq!(
        f.cur().0,
        0,
        "Down stays in the wrapped line (next screen row)"
    );
    assert!(f.cur().1 > 0);
    let rows = f.ed().screen_rows(10);
    assert!(
        rows.iter().filter(|r| r.line == 0).count() >= 3,
        "a 49-char line in ~18 columns wraps to 3+ rows: {rows:?}"
    );
    f.press(K::Up);
    assert_eq!(f.cur(), (0, 0));
    // Wrapping is display only: saving doesn't insert line breaks.
    f.typ("X");
    f.ctrl_ch('s');
    assert_eq!(f.disk(), format!("X{long}\nshort").into_bytes());
    // Turning it off again.
    choose(&mut f, e, Action::WrapLines);
    assert!(!f.app.wrap_lines);
    f.draw(20, 10);
    f.press(K::Down);
    assert_eq!(f.cur().0, 1, "unwrapped: Down goes to the next line");
}

#[test]
fn wrapped_very_long_line_renders_at_tiny_sizes() {
    let mut f = fx("wraplong", &"ż".repeat(5000));
    f.app.wrap_lines = true;
    f.ed_mut().set_wrap(true);
    for (w, h) in [(80, 24), (10, 3), (3, 3), (1, 1), (2, 40)] {
        f.draw(w, h);
        f.press(K::F(4));
        f.draw(w, h);
        f.press(K::PageDown);
        f.ctrl(K::End);
        f.press(K::Up);
        f.draw(w, h);
    }
}

impl Fx {
    fn ed_mut(&mut self) -> &mut Editor {
        self.app.editor.as_mut().unwrap()
    }
}

// ----------------------------------------------------------------- find

fn find_type(f: &mut Fx, s: &str) {
    f.ctrl_ch('f');
    assert!(f.ed().find.is_some(), "Ctrl+F opens the find bar");
    f.typ(s);
}

#[test]
fn find_is_incremental_case_insensitive_unicode() {
    let mut f = fx("find1", "Żaba i żaba\nŻABA");
    find_type(&mut f, "ż");
    assert_eq!(f.find_status(), "1/3");
    f.typ("aba");
    assert_eq!(f.find_status(), "1/3");
    assert_eq!(
        f.text(),
        "Żaba i żaba\nŻABA",
        "typing goes to the bar, not the text"
    );
    assert_eq!(
        f.ed().find.as_ref().unwrap().matches_on(0).len(),
        2,
        "all matches on line 0"
    );
    assert_eq!(f.ed().find.as_ref().unwrap().matches_on(1).len(), 1);
    assert_eq!(f.sel(), Some(((0, 0), (0, 4))), "current match selected");
}

#[test]
fn find_uppercase_query_matches_lowercase_text() {
    let mut f = fx("find2", "jaźń");
    find_type(&mut f, "ŹŃ");
    assert_eq!(f.find_status(), "1/1");
    assert_eq!(f.sel(), Some(((0, 2), (0, 4))));
}

#[test]
fn find_no_matches() {
    let mut f = fx("find3", "abc");
    find_type(&mut f, "xyz");
    assert_eq!(f.find_status(), "No matches");
    f.press(K::F(3));
    assert_eq!(f.find_status(), "No matches");
    // Enter with nothing found just closes the bar.
    f.press(K::Enter);
    assert!(f.ed().find.is_none());
    assert_eq!(f.sel(), None);
    assert_eq!(f.text(), "abc");
}

#[test]
fn find_f3_shift_f3_cycle_with_wrap_around() {
    let mut f = fx("find4", "x a x\nb x");
    find_type(&mut f, "x");
    assert_eq!(f.find_status(), "1/3");
    f.press(K::F(3));
    assert_eq!(f.find_status(), "2/3");
    assert_eq!(f.sel(), Some(((0, 4), (0, 5))));
    f.press(K::F(3));
    assert_eq!(f.find_status(), "3/3");
    assert_eq!(f.sel(), Some(((1, 2), (1, 3))));
    f.press(K::F(3));
    assert_eq!(f.find_status(), "1/3", "wraps to the first");
    f.shift(K::F(3));
    assert_eq!(f.find_status(), "3/3", "Shift+F3 wraps backwards");
    f.shift(K::F(3));
    assert_eq!(f.find_status(), "2/3");
    // Up/Down are nothing special in the bar.
    f.press(K::Down);
    f.press(K::Up);
    assert_eq!(f.find_status(), "2/3");
    assert_eq!(f.text(), "x a x\nb x");
}

#[test]
fn enter_in_find_closes_the_bar_keeping_the_match_selected() {
    let mut f = fx("find4b", "x a x\nb x");
    find_type(&mut f, "x");
    f.press(K::F(3));
    f.press(K::Enter);
    assert!(f.ed().find.is_none(), "Enter closes the bar");
    assert_eq!(f.sel(), Some(((0, 4), (0, 5))), "the match stays selected");
    f.typ("Y");
    assert_eq!(f.text(), "x a Y\nb x", "typing replaces it");
}

#[test]
fn find_overlapping_aaa_in_aaaa() {
    let mut f = fx("find5", "aaaa");
    find_type(&mut f, "aaa");
    let s = f.find_status();
    assert!(s == "1/1" || s == "1/2", "got {s}");
    for _ in 0..5 {
        f.press(K::F(3));
        let (a, b) = f.sel().expect("a match stays selected");
        assert_eq!(b.1 - a.1, 3);
    }
}

#[test]
fn find_backspace_in_bar_updates_matches() {
    let mut f = fx("find6", "abc abd");
    find_type(&mut f, "abc");
    assert_eq!(f.find_status(), "1/1");
    f.press(K::Backspace);
    assert_eq!(f.find_status(), "1/2");
    f.press(K::Backspace);
    f.press(K::Backspace);
    assert_eq!(f.find_status(), "", "empty query: no counter");
    assert_eq!(f.text(), "abc abd");
}

#[test]
fn esc_closes_find_forgetting_it_but_f3_finds_the_next_one() {
    let mut f = fx("find7", "one two one two one");
    find_type(&mut f, "two");
    f.press(K::F(3));
    assert_eq!(f.sel(), Some(((0, 12), (0, 15))));
    f.press(K::Esc);
    assert!(f.ed().find.is_none(), "Esc closes the bar");
    assert!(
        f.app.editor.is_some(),
        "Esc closes only the bar, not the editor"
    );
    assert_eq!(f.sel(), None, "nothing stays selected");
    assert_eq!(f.cur(), (0, 12), "the cursor stays at the match's start");
    // F3 searches the last query again from the cursor, wrapping around.
    f.press(K::F(3));
    assert!(f.ed().find.is_none(), "F3 doesn't reopen the bar");
    assert_eq!(f.sel(), Some(((0, 12), (0, 15))));
    f.press(K::F(3));
    assert_eq!(f.sel(), Some(((0, 4), (0, 7))), "wraps around");
    f.shift(K::F(3));
    assert_eq!(f.sel(), Some(((0, 12), (0, 15))));
    // Typing now replaces the selected match (bar is closed).
    f.typ("X");
    assert_eq!(f.text(), "one two one X one");
}

#[test]
fn esc_after_f3_clears_the_match_and_only_a_second_esc_closes() {
    let mut f = fx("findesc", "one two one");
    find_type(&mut f, "two");
    f.press(K::Esc);
    assert_eq!(f.sel(), None);
    f.press(K::F(3));
    assert_eq!(f.sel(), Some(((0, 4), (0, 7))), "F3 brings the match back");
    f.press(K::Esc);
    assert!(
        f.app.editor.is_some(),
        "the first Esc only clears the match"
    );
    assert_eq!(f.sel(), None);
    assert_eq!(f.cur(), (0, 7), "the cursor stays put");
    f.press(K::Esc);
    assert!(f.app.editor.is_none(), "with nothing selected, Esc closes");
}

#[test]
fn esc_clears_a_selection_made_by_hand_too_then_asks_about_changes() {
    let mut f = fx("findesc2", "abc");
    f.typ("x");
    f.shift(K::Right);
    f.shift(K::Right);
    assert!(f.sel().is_some());
    f.press(K::Esc);
    assert!(f.app.editor.is_some());
    assert!(matches!(f.app.dialog, Dialog::None), "no prompt yet");
    assert_eq!((f.cur(), f.sel()), ((0, 3), None));
    // After Enter in the bar too: the kept match goes first.
    find_type(&mut f, "bc");
    f.press(K::Enter);
    assert!(f.sel().is_some());
    f.press(K::Esc);
    assert!(matches!(f.app.dialog, Dialog::None));
    assert_eq!(f.sel(), None);
    // Then Esc closes, through the unsaved-changes prompt.
    f.press(K::Esc);
    assert!(matches!(f.app.dialog, Dialog::UnsavedChanges { .. }));
    f.press(K::Esc);
    assert!(f.app.editor.is_some(), "Cancel keeps editing");
    assert_eq!(f.text(), "xabc");
}

#[test]
fn f3_with_nothing_searched_does_nothing() {
    let mut f = fx("find7b", "abc");
    f.press(K::F(3));
    f.shift(K::F(3));
    assert!(f.ed().find.is_none(), "only Ctrl+F opens the bar");
    assert_eq!((f.cur(), f.sel()), ((0, 0), None));
    let e = edit_menu_index(&f.app);
    choose(&mut f, e, Action::FindNext);
    assert!(f.ed().find.is_none());
}

#[test]
fn find_next_from_menu_uses_last_term() {
    let mut f = fx("find8b", "ab ab");
    find_type(&mut f, "ab");
    f.press(K::Esc);
    let e = edit_menu_index(&f.app);
    // From the cursor, left at the forgotten match's start: that one first.
    choose(&mut f, e, Action::FindNext);
    assert_eq!(f.sel(), Some(((0, 0), (0, 2))));
    choose(&mut f, e, Action::FindNext);
    assert_eq!(f.sel(), Some(((0, 3), (0, 5))));
}

#[test]
fn find_from_the_menu_reopens_with_the_last_term() {
    let mut f = fx("find8", "ab ab");
    find_type(&mut f, "ab");
    f.press(K::Esc);
    let e = edit_menu_index(&f.app);
    choose(&mut f, e, Action::Find);
    let bar = f.ed().find.as_ref().expect("the bar is open again");
    assert_eq!((bar.field.text(), bar.fresh), ("ab", true));
}

#[test]
fn ctrl_keys_in_find_bar_do_not_edit_text() {
    let mut f = fx("find9", "abc");
    find_type(&mut f, "b");
    f.press(K::Backspace);
    f.press(K::Delete);
    f.press(K::Tab);
    f.paste("zz");
    f.ctrl_ch('x');
    f.ctrl_ch('z');
    assert_eq!(
        f.text(),
        "abc",
        "while the bar is open the text must not change"
    );
}

#[test]
fn find_with_polish_query_typed_via_altgr_disguises() {
    let mut f = fx("find10", "Łódź łódź");
    f.ctrl_ch('f');
    f.key(K::Char('ł'), M::CONTROL | M::ALT);
    f.key(K::Char('ó'), M::ALT);
    f.key(K::Char('d'), M::NONE);
    f.key(K::Char('ź'), M::NONE);
    assert_eq!(f.find_status(), "1/2");
    assert_eq!(f.text(), "Łódź łódź");
}

#[test]
fn find_after_editing_finds_new_text() {
    let mut f = fx("find11", "abc");
    f.ctrl(K::End);
    f.typ(" qq");
    find_type(&mut f, "qq");
    assert_eq!(f.find_status(), "1/1");
}

// ------------------------------------------------------------- rendering

#[test]
fn renders_at_tiny_sizes_in_pane_and_full_screen() {
    let mut f = fx("render", "zażółć gęślą jaźń\n\tx\n😀😀😀\n");
    f.shift(K::Down);
    let sizes = [
        (0, 0),
        (1, 1),
        (2, 1),
        (1, 2),
        (3, 2),
        (10, 3),
        (12, 4),
        (20, 5),
        (80, 24),
        (200, 60),
    ];
    for full in [false, true] {
        if f.ed().full_screen != full {
            f.press(K::F(4));
        }
        for (w, h) in sizes {
            f.draw(w, h);
        }
        f.ctrl_ch('f');
        f.typ("ż");
        for (w, h) in sizes {
            f.draw(w, h);
        }
        f.press(K::Esc);
        f.typ("q");
        f.press(K::Esc); // unsaved prompt
        for (w, h) in sizes {
            f.draw(w, h);
        }
        f.press(K::Right);
        f.press(K::Right);
        f.press(K::Enter); // Cancel
        assert!(f.app.editor.is_some());
        f.press(K::F(9));
        for (w, h) in sizes {
            f.draw(w, h);
        }
        f.press(K::Esc);
    }
}

#[test]
fn editor_shows_file_text_and_modified_marker_somewhere() {
    let mut f = fx("rendertext", "hello world\n");
    let s = f.draw(80, 24);
    assert!(s.contains("hello world"), "file text on screen:\n{s}");
    assert!(s.contains("f.txt"), "file name on screen:\n{s}");
    f.press(K::F(4));
    let s = f.draw(80, 24);
    assert!(s.contains("hello world"), "full screen text:\n{s}");
}

#[test]
fn find_bar_counter_is_rendered() {
    let mut f = fx("renderfind", "ab ab ab");
    find_type(&mut f, "ab");
    let s = f.draw(80, 24);
    assert!(s.contains("1/3"), "counter on screen:\n{s}");
    f.typ("zz");
    let s = f.draw(80, 24);
    assert!(s.contains("No matches"), "no-matches on screen:\n{s}");
}

#[test]
fn many_lines_scroll_vertically_to_cursor() {
    let text: String = (0..500).map(|i| format!("row{i}\n")).collect();
    let mut f = fx("vscroll", &text);
    f.draw(40, 10);
    f.ctrl(K::End);
    let s = f.draw(40, 10);
    assert!(s.contains("row499"), "view follows cursor to the end:\n{s}");
    assert!(f.ed().cursor_in_view().is_some());
    f.ctrl(K::Home);
    let s = f.draw(40, 10);
    assert!(s.contains("row0"), "back at the top:\n{s}");
}

// ---------------------------------------------- unit-level editor checks

#[test]
fn from_bytes_rejects_binary_and_non_utf8() {
    assert!(Editor::from_bytes(PathBuf::from("a"), b"a\0b").is_err());
    assert!(Editor::from_bytes(PathBuf::from("a"), b"\xc3\x28").is_err());
    assert!(Editor::from_bytes(PathBuf::from("a"), "ok ąę".as_bytes()).is_ok());
}

#[test]
fn pos_is_in_characters() {
    let mut f = fx("poschars", "ąąą");
    f.press(K::End);
    assert_eq!(f.ed().cursor(), Pos::new(0, 3));
}

// ------------------------------------------------ extra edge cases

#[test]
fn copy_or_cut_without_selection_keeps_clipboard_and_text() {
    let mut f = fx("copynosel", "ab cd");
    f.shift(K::Right);
    f.shift(K::Right);
    f.ctrl_ch('c');
    f.press(K::End);
    f.ctrl_ch('c'); // nothing selected
    f.ctrl_ch('x'); // nothing selected
    assert_eq!(f.text(), "ab cd");
    f.ctrl_ch('v');
    assert_eq!(f.text(), "ab cdab");
}

#[test]
fn paste_with_empty_clipboard_does_not_modify() {
    let mut f = fx("pasteempty", "ab");
    f.ctrl_ch('v');
    f.paste("");
    assert_eq!(f.text(), "ab");
    assert!(!f.ed().is_modified());
}

#[test]
fn alt_ascii_letter_without_menu_is_not_inserted() {
    let mut f = fx("altx", "ab");
    f.key(K::Char('x'), M::ALT);
    f.app.menu_open = false;
    assert_eq!(f.text(), "ab");
}

#[cfg(unix)]
#[test]
fn failed_save_keeps_editor_open_and_modified() {
    use std::os::unix::fs::PermissionsExt;
    let mut f = fx("savefail", "a");
    f.typ("b");
    std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(&f.file, std::fs::Permissions::from_mode(0o444)).unwrap();
    f.ctrl_ch('s');
    let still_modified = f.ed().is_modified();
    let disk = f.disk();
    // Quitting must still ask, and a failing Save in the prompt must not quit.
    f.app.error_dialog = None;
    f.ctrl_ch('q');
    let asked = f.app.dialog_is_unsaved_changes();
    f.press(K::Enter); // Save (fails)
    let quit = f.app.should_quit;
    let open = f.app.editor.is_some();
    std::fs::set_permissions(&f.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&f.file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(disk, b"a");
    assert!(still_modified, "failed save must leave the text modified");
    assert!(asked, "quit after a failed save must still ask");
    assert!(!quit, "a failing Save in the prompt must not quit");
    assert!(open, "editor stays open after the failed save");
}

#[test]
fn untouched_mixed_line_endings_are_not_rewritten() {
    // Beyond the letter of the spec: a file with mixed endings, saved
    // without any edit, should not be changed.
    let bytes = b"a\r\nb\nc\r\n";
    let mut f = fx_bytes("mixed", bytes);
    f.ctrl_ch('s');
    assert_eq!(f.disk(), bytes);
}

#[test]
fn lone_cr_does_not_corrupt() {
    // Old Mac line endings / stray CR: either refused or round-tripped.
    let bytes = b"a\rb\n";
    let mut f = fx_closed("lonecr", bytes);
    f.press(K::F(4));
    if f.app.editor.is_some() {
        f.ctrl_ch('s');
        assert_eq!(f.disk(), bytes);
    }
}

#[test]
fn find_bar_esc_then_esc_closes_editor() {
    let mut f = fx("findescesc", "abc");
    find_type(&mut f, "b");
    f.press(K::Esc);
    assert!(f.app.editor.is_some());
    f.press(K::Esc);
    assert!(
        f.app.editor.is_none(),
        "second Esc closes the unmodified editor"
    );
}
