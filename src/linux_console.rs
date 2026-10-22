//! Makes a frame come out right on the Linux text console (`TERM=linux`),
//! which has only 16 colors and handles them in its own way. The kernel
//! (`rgb_foreground`/`rgb_background` in drivers/tty/vt/vt.c) rounds every
//! color, RGB or 256-palette, to one of 8 hues plus a bright flag. That
//! bright flag is the same one bold (SGR 1) sets and SGR 22 clears. Two
//! things go wrong with that:
//!
//! - ratatui sends a cell's attributes before its colors, and the colors
//!   only when they change. Text after a bold cell in the same color gets
//!   SGR 22 alone, which dims a bright color (white turns gray).
//! - A muted color can round to the background's own hue and vanish, like
//!   the classic style's dark-cyan disabled menu items on cyan.
//!
//! So each cell gets bold exactly when the kernel would show its color
//! bright, and text that would vanish becomes dark gray, like disabled
//! items in the original Norton Commander.
//!
//! Also, the console can only show the characters its font has. DOS had
//! the double-line frames (`║ ═ ╔`) built into the video card; on Linux
//! they're up to the font, and some (Terminus) lack them, showing a stub
//! in their place. Those become single lines, which every font has.

use std::collections::HashSet;

use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};

/// The double-line and mixed box-drawing characters (U+2550–U+256C) and
/// the single-line one each turns into.
const DOUBLE_TO_SINGLE: [(char, &str); 29] = [
    ('═', "─"),
    ('║', "│"),
    ('╒', "┌"),
    ('╓', "┌"),
    ('╔', "┌"),
    ('╕', "┐"),
    ('╖', "┐"),
    ('╗', "┐"),
    ('╘', "└"),
    ('╙', "└"),
    ('╚', "└"),
    ('╛', "┘"),
    ('╜', "┘"),
    ('╝', "┘"),
    ('╞', "├"),
    ('╟', "├"),
    ('╠', "├"),
    ('╡', "┤"),
    ('╢', "┤"),
    ('╣', "┤"),
    ('╤', "┬"),
    ('╥', "┬"),
    ('╦', "┬"),
    ('╧', "┴"),
    ('╨', "┴"),
    ('╩', "┴"),
    ('╪', "┼"),
    ('╫', "┼"),
    ('╬', "┼"),
];

/// What a frame needs changed on this console.
pub struct Console {
    /// For each of `DOUBLE_TO_SINGLE`, the stand-in when the font lacks it.
    replacements: [Option<&'static str>; 29],
}

impl Console {
    /// `Some` on the Linux text console, with what its current font can
    /// show. Should the font be unknown, frames are left as they are.
    pub fn detect() -> Option<Console> {
        if !std::env::var("TERM").is_ok_and(|term| term == "linux") {
            return None;
        }
        Some(match font_characters() {
            Some(font) => Console::for_font(&font),
            None => Console {
                replacements: [None; 29],
            },
        })
    }

    fn for_font(font: &HashSet<char>) -> Console {
        Console {
            replacements: DOUBLE_TO_SINGLE.map(|(double, single)| {
                let has = |s: &str| s.chars().all(|c| font.contains(&c));
                (!font.contains(&double) && has(single)).then_some(single)
            }),
        }
    }

    pub fn adapt(&self, buffer: &mut Buffer) {
        for cell in &mut buffer.content {
            if let Some(single) = self.replacement(cell.symbol()) {
                cell.set_symbol(single);
            }
            adapt_colors(cell);
        }
    }

    fn replacement(&self, symbol: &str) -> Option<&'static str> {
        let mut chars = symbol.chars();
        let c = chars.next()?;
        if chars.next().is_some() || !('═'..='╬').contains(&c) {
            return None;
        }
        self.replacements[c as usize - '═' as usize]
    }
}

/// The characters the console's current font has, from the kernel's
/// Unicode-to-glyph map (GIO_UNIMAP on stdin, the console itself).
#[cfg(target_os = "linux")]
fn font_characters() -> Option<HashSet<char>> {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct UniPair {
        unicode: u16,
        fontpos: u16,
    }
    #[repr(C)]
    struct UniMapDesc {
        entry_ct: u16,
        entries: *mut UniPair,
    }
    const GIO_UNIMAP: libc::Ioctl = 0x4B66;

    // Asked with no room, the kernel says how many entries there are.
    let mut desc = UniMapDesc {
        entry_ct: 0,
        entries: std::ptr::null_mut(),
    };
    unsafe { libc::ioctl(libc::STDIN_FILENO, GIO_UNIMAP, &mut desc as *mut UniMapDesc) };
    if desc.entry_ct == 0 {
        return None;
    }
    let mut pairs = vec![UniPair::default(); desc.entry_ct.into()];
    desc.entries = pairs.as_mut_ptr();
    if unsafe { libc::ioctl(libc::STDIN_FILENO, GIO_UNIMAP, &mut desc as *mut UniMapDesc) } < 0 {
        return None;
    }
    pairs.truncate(desc.entry_ct.into());
    Some(
        pairs
            .iter()
            .filter_map(|pair| char::from_u32(pair.unicode.into()))
            .collect(),
    )
}

#[cfg(not(target_os = "linux"))]
fn font_characters() -> Option<HashSet<char>> {
    None
}

fn adapt_colors(cell: &mut Cell) {
    let Some((hue, bright)) = foreground(cell.fg) else {
        return;
    };
    let bright = if !bright && background(cell.bg) == Some(hue) {
        cell.fg = Color::DarkGray;
        true
    } else {
        bright
    };
    cell.modifier.set(Modifier::BOLD, bright);
}

/// The color as the kernel shows it in front: its hue (VGA bits, red 4,
/// green 2, blue 1) and whether it's bright. `None` for the default color,
/// which the kernel leaves to its own setting.
fn foreground(color: Color) -> Option<(u8, bool)> {
    let (r, g, b) = rgb(color)?;
    let max = r.max(g).max(b);
    let mut hue = 0;
    if r > max / 2 {
        hue |= 4;
    }
    if g > max / 2 {
        hue |= 2;
    }
    if b > max / 2 {
        hue |= 1;
    }
    Some(if hue == 7 && max <= 0x55 {
        (0, true)
    } else {
        (hue, max > 0xaa)
    })
}

/// The hue the kernel shows behind; backgrounds are never bright there.
fn background(color: Color) -> Option<u8> {
    let (r, g, b) = rgb(color)?;
    Some((r >> 7) << 2 | (g >> 7) << 1 | b >> 7)
}

/// What crossterm sends for `color`: named colors go out as their
/// 256-palette index (`38;5;n`), turned into RGB by the kernel's own table.
fn rgb(color: Color) -> Option<(u8, u8, u8)> {
    let index = match color {
        Color::Reset => return None,
        Color::Rgb(r, g, b) => return Some((r, g, b)),
        Color::Indexed(n) => n,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
    };
    let bit = |mask: u8, on: u8, off: u8| if index & mask != 0 { on } else { off };
    Some(match index {
        0..8 => (bit(1, 0xaa, 0), bit(2, 0xaa, 0), bit(4, 0xaa, 0)),
        8..16 => (bit(1, 0xff, 0x55), bit(2, 0xff, 0x55), bit(4, 0xff, 0x55)),
        16..232 => {
            let i = u16::from(index - 16);
            let level = |step: u16| (step * 85 / 2) as u8;
            (level(i / 36), level(i / 6 % 6), level(i % 6))
        }
        232.. => {
            let level = (index - 232) * 10 + 8;
            (level, level, level)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn foreground_rounds_like_the_kernel() {
        assert_eq!(foreground(Color::Rgb(255, 255, 255)), Some((7, true)));
        assert_eq!(foreground(Color::Rgb(0, 85, 85)), Some((3, false)));
        assert_eq!(foreground(Color::Rgb(0, 0, 0)), Some((0, false)));
        assert_eq!(foreground(Color::Rgb(0x50, 0xff, 0xff)), Some((3, true)));
        assert_eq!(foreground(Color::DarkGray), Some((0, true)));
        assert_eq!(foreground(Color::White), Some((7, true)));
        assert_eq!(foreground(Color::Gray), Some((7, false)));
        assert_eq!(foreground(Color::Indexed(231)), Some((7, true)));
        assert_eq!(foreground(Color::Reset), None);
    }

    #[test]
    fn background_rounds_down() {
        assert_eq!(background(Color::Rgb(0, 170, 170)), Some(3));
        assert_eq!(background(Color::Rgb(0, 0, 170)), Some(1));
        assert_eq!(background(Color::Rgb(0, 85, 85)), Some(0));
        assert_eq!(background(Color::Gray), Some(7));
    }

    #[test]
    fn double_lines_become_single_only_where_the_font_lacks_them() {
        // A Terminus-like font: single lines and ╬, no other double ones.
        let font: HashSet<char> = "─│┌┐└┘├┤┬┴┼╬".chars().collect();
        let console = Console::for_font(&font);
        assert_eq!(console.replacement("║"), Some("│"));
        assert_eq!(console.replacement("╔"), Some("┌"));
        assert_eq!(console.replacement("╝"), Some("┘"));
        assert_eq!(console.replacement("╬"), None);
        assert_eq!(console.replacement("│"), None);
        assert_eq!(console.replacement("a"), None);

        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer[(0, 0)].set_symbol("═");
        buffer[(1, 0)].set_symbol("x");
        console.adapt(&mut buffer);
        assert_eq!(buffer[(0, 0)].symbol(), "─");
        assert_eq!(buffer[(1, 0)].symbol(), "x");

        // A font with every double line keeps them all.
        let full: HashSet<char> = DOUBLE_TO_SINGLE.iter().map(|&(double, _)| double).collect();
        assert!(
            Console::for_font(&full)
                .replacements
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn double_to_single_covers_the_whole_block_in_order() {
        for (i, &(double, _)) in DOUBLE_TO_SINGLE.iter().enumerate() {
            assert_eq!(double as usize, '═' as usize + i);
        }
        assert_eq!(DOUBLE_TO_SINGLE.last().unwrap().0, '╬');
    }

    #[test]
    fn adapt_sets_bold_by_brightness_and_rescues_hidden_text() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        let cyan = Color::Rgb(0, 170, 170);
        let styles = [
            // White without bold, after a bold cell: gets bold.
            Style::new().fg(Color::Rgb(255, 255, 255)).bg(cyan),
            // Black with bold: loses it, as the kernel shows it dark.
            Style::new()
                .fg(Color::Black)
                .bg(cyan)
                .add_modifier(Modifier::BOLD),
            // Dark cyan on cyan: would vanish.
            Style::new().fg(Color::Rgb(0, 85, 85)).bg(cyan),
            // The terminal's default color: left alone.
            Style::new().add_modifier(Modifier::BOLD),
        ];
        for (x, style) in styles.into_iter().enumerate() {
            buffer[(x as u16, 0)].set_style(style);
        }
        for cell in &mut buffer.content {
            adapt_colors(cell);
        }
        let bold = |x: u16| buffer[(x, 0)].modifier.contains(Modifier::BOLD);
        assert!(bold(0));
        assert!(!bold(1));
        assert_eq!(buffer[(2, 0)].fg, Color::DarkGray);
        assert!(bold(2));
        assert!(bold(3));
    }
}
