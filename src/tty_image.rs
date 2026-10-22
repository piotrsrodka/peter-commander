//! Full-screen image viewer for the Linux text console (tty), where there
//! is no desktop for `xdg-open` to hand the file to. It talks to the
//! kernel's DRM/KMS directly, the same way a Wayland compositor does, just
//! with a single picture instead of windows: become the display's owner
//! (DRM master), draw the picture into a buffer, ask KMS to scan that buffer
//! out instead of the console's, and give the console its buffer back when
//! done. Plain ioctls with the kernel's own struct layouts — no libdrm.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use image::RgbImage;
use signal_hook::SigId;

/// Formats the viewer can decode (the `image` crate features enabled in
/// Cargo.toml). Anything else still goes to `xdg-open`.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp"];

pub fn is_image(name: &str) -> bool {
    crate::app::has_extension(name, IMAGE_EXTENSIONS)
}

/// Whether PC is running straight on a kernel virtual console (`/dev/tty3`
/// and so on) — the only place where it can take over the screen. A desktop
/// terminal, ssh or tmux all show up as `/dev/pts/N` instead.
pub fn on_console() -> bool {
    std::fs::read_link("/proc/self/fd/0").is_ok_and(|path| is_console_path(&path))
}

fn is_console_path(path: &Path) -> bool {
    path.to_str()
        .and_then(|path| path.strip_prefix("/dev/tty"))
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The screen, taken over from the console. Dropping it gives it back.
/// Fields drop in order after `Drop::drop`: closing `card` releases the
/// display before `vt` puts console switching back to normal.
pub struct Screen {
    card: File,
    vt: VtSwitch,
    connector_id: u32,
    /// What the console had on the CRTC, put back on leaving.
    console_crtc: DrmModeCrtc,
    /// Our front buffer on the same CRTC, set again on coming back.
    picture_crtc: DrmModeCrtc,
    /// Whether our picture is on the monitor right now, i.e. this console
    /// is the active one.
    on_screen: bool,
    /// Back on this console, but the program we switched to hasn't let go
    /// of the display yet: until when to keep asking for it.
    reacquire_until: Option<Instant>,
    /// Two buffers, so a new picture is drawn out of sight and then shown
    /// whole, instead of being painted line by line on screen.
    buffers: [DumbBuffer; 2],
    /// The one of `buffers` in `picture_crtc`.
    front: usize,
    width: usize,
    height: usize,
}

/// How long to wait, after coming back to this console, for the program
/// on the other one (a compositor, via logind) to drop the display.
const REACQUIRE_WAIT: Duration = Duration::from_secs(3);

/// Decodes an image file. Done before touching the screen, so an
/// unreadable file never blanks the console.
pub fn decode(path: &Path) -> Result<RgbImage> {
    Ok(image::ImageReader::open(path)?
        .with_guessed_format()?
        .decode()
        .context("decoding")?
        .to_rgb8())
}

impl Screen {
    /// Takes the screen over from the console, showing black until `draw`.
    pub fn open() -> Result<Screen> {
        let vt = VtSwitch::hold()?;
        let (card, output) = find_output()?;
        let fd = card.as_raw_fd();

        // The first opener of a card no one holds becomes master on its own;
        // asking explicitly turns "someone else has the screen" into an error.
        if unsafe { libc::ioctl(fd, DRM_IOCTL_SET_MASTER) } < 0 {
            return Err(io::Error::last_os_error())
                .context("the screen belongs to another program (run PC on a free tty)");
        }

        let mut console_crtc = DrmModeCrtc {
            crtc_id: output.crtc_id,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &mut console_crtc, "GETCRTC")?;
        let console_mode = (console_crtc.mode_valid != 0).then_some(console_crtc.mode);
        let modes = mode_choices(console_mode, output.mode);

        // Big enough for every mode we may end up in; a mode smaller than
        // the buffer just shows its top-left part. On an early error,
        // closing the card frees them.
        let width = modes.iter().map(|m| m.hdisplay).max().unwrap_or(0);
        let height = modes.iter().map(|m| m.vdisplay).max().unwrap_or(0);
        let buffers = [
            DumbBuffer::create(fd, width.into(), height.into())?,
            DumbBuffer::create(fd, width.into(), height.into())?,
        ];

        let mut screen = Screen {
            card,
            vt,
            connector_id: output.connector_id,
            console_crtc,
            // The actual mode setting: this CRTC, in this mode, scanning out
            // this framebuffer (to this connector, added by `set_crtc`).
            picture_crtc: DrmModeCrtc {
                crtc_id: output.crtc_id,
                fb_id: buffers[0].fb_id,
                mode_valid: 1,
                ..Default::default()
            },
            on_screen: false,
            reacquire_until: None,
            buffers,
            front: 0,
            width: 0,
            height: 0,
        };
        let mut result = Ok(());
        for mode in modes {
            screen.picture_crtc.mode = mode;
            result = screen.set_crtc(screen.picture_crtc);
            if result.is_ok() {
                screen.width = mode.hdisplay.into();
                screen.height = mode.vdisplay.into();
                screen.on_screen = true;
                break;
            }
        }
        result.map(|()| screen)
    }

    /// Replaces the picture with `picture`, fit and centered on black:
    /// drawn into the back buffer, which then becomes the front one.
    pub fn draw(&mut self, picture: &RgbImage) -> Result<()> {
        let back = 1 - self.front;
        let buffer = &self.buffers[back];
        let pixels = unsafe { std::slice::from_raw_parts_mut(buffer.map, buffer.size) };
        draw_fit(picture, pixels, self.width, self.height, buffer.pitch);
        self.front = back;
        self.picture_crtc.fb_id = buffer.fb_id;
        // Same mode, so no modeset: KMS just scans out the other buffer
        // from the next frame on. Off screen, it's shown on coming back.
        if self.on_screen {
            self.set_crtc(self.picture_crtc)?;
        }
        Ok(())
    }

    /// Answers the kernel's console-switch requests (Ctrl+Alt+Fn) — call
    /// this often, as the switch waits for it. Leaving: the display is
    /// released and the switch allowed.
    /// Coming back: the display is taken again and the picture, still in
    /// its buffer, put back — retried for a while, as the program on the
    /// console we come from may let go of the display only after the
    /// switch. An error means the display couldn't be taken back (someone
    /// else holds it); the picture is then gone.
    pub fn follow_console_switches(&mut self) -> Result<()> {
        let fd = self.card.as_raw_fd();
        if self.vt.release.swap(false, Ordering::Relaxed) {
            // The picture stays up until the console we switch to sets its
            // own (the kernel for a text one, a compositor for its own);
            // putting the console's buffer back first would flash PC's
            // screen in between.
            self.on_screen = false;
            // Still waiting to get the display back means we don't hold it.
            if self.reacquire_until.take().is_none() {
                unsafe { libc::ioctl(fd, DRM_IOCTL_DROP_MASTER) };
            }
            unsafe { libc::ioctl(libc::STDIN_FILENO, VT_RELDISP, 1 as libc::c_ulong) };
        }
        if self.vt.acquire.swap(false, Ordering::Relaxed) {
            unsafe {
                libc::ioctl(libc::STDIN_FILENO, VT_RELDISP, VT_ACKACQ as libc::c_ulong);
            }
            self.reacquire_until = Some(Instant::now() + REACQUIRE_WAIT);
        }
        if let Some(deadline) = self.reacquire_until {
            if unsafe { libc::ioctl(fd, DRM_IOCTL_SET_MASTER) } < 0 {
                let err = io::Error::last_os_error();
                // Another master still there: EBUSY, or EINVAL on older kernels.
                let held = matches!(err.raw_os_error(), Some(libc::EBUSY | libc::EINVAL));
                if held && Instant::now() < deadline {
                    return Ok(());
                }
                self.reacquire_until = None;
                return Err(err).context("taking the screen back");
            }
            self.reacquire_until = None;
            self.set_crtc(self.picture_crtc)?;
            self.on_screen = true;
        }
        Ok(())
    }

    /// SETCRTC with `crtc` driving our one connector — or none, for a
    /// CRTC the console had switched off.
    fn set_crtc(&self, mut crtc: DrmModeCrtc) -> Result<()> {
        if crtc.mode_valid != 0 {
            crtc.set_connectors_ptr = &self.connector_id as *const u32 as u64;
            crtc.count_connectors = 1;
        }
        ioctl(
            self.card.as_raw_fd(),
            DRM_IOCTL_MODE_SETCRTC,
            &mut crtc,
            "SETCRTC",
        )
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if self.on_screen {
            let _ = self.set_crtc(self.console_crtc);
        }
        let fd = self.card.as_raw_fd();
        for buffer in &mut self.buffers {
            buffer.free(fd);
        }
        // Closing the card drops master, so the console can draw again.
    }
}

/// The modes to try, best first. The console's own mode when it has the
/// monitor's preferred size: no modeset, so the monitor doesn't blink.
/// A console set smaller (`video=` on the kernel command line, say) would
/// make a blurry picture, so then the preferred mode, falling back to the
/// console's if the CRTC won't take it.
fn mode_choices(
    console: Option<DrmModeModeinfo>,
    preferred: DrmModeModeinfo,
) -> Vec<DrmModeModeinfo> {
    match console {
        None => vec![preferred],
        Some(console)
            if (console.hdisplay, console.vdisplay) == (preferred.hdisplay, preferred.vdisplay) =>
        {
            vec![console]
        }
        Some(console) => vec![preferred, console],
    }
}

/// A "dumb buffer": plain pixels in memory the kernel can scan out, no GPU
/// rendering involved. Registered as a framebuffer, then mapped so it can
/// be written like an ordinary array. Freed by `free`, or by closing the
/// card.
struct DumbBuffer {
    handle: u32,
    fb_id: u32,
    map: *mut u8,
    size: usize,
    pitch: usize,
}

impl DumbBuffer {
    /// A black `width`×`height` buffer.
    fn create(fd: i32, width: u32, height: u32) -> Result<DumbBuffer> {
        let mut dumb = DrmModeCreateDumb {
            width,
            height,
            bpp: 32,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &mut dumb, "CREATE_DUMB")?;
        let mut fb = DrmModeFbCmd {
            width: dumb.width,
            height: dumb.height,
            pitch: dumb.pitch,
            bpp: 32,
            depth: 24,
            handle: dumb.handle,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_ADDFB, &mut fb, "ADDFB")?;
        let mut map_req = DrmModeMapDumb {
            handle: dumb.handle,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &mut map_req, "MAP_DUMB")?;
        let size = dumb.size as usize;
        let map = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                map_req.offset as libc::off_t,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error()).context("mmap");
        }
        unsafe { ptr::write_bytes(map as *mut u8, 0, size) };
        Ok(DumbBuffer {
            handle: dumb.handle,
            fb_id: fb.fb_id,
            map: map as *mut u8,
            size,
            pitch: dumb.pitch as usize,
        })
    }

    fn free(&mut self, fd: i32) {
        unsafe {
            libc::munmap(self.map as *mut libc::c_void, self.size);
            libc::ioctl(fd, DRM_IOCTL_MODE_RMFB, &mut self.fb_id as *mut u32);
            let mut destroy = DrmModeDestroyDumb {
                handle: self.handle,
            };
            libc::ioctl(fd, DRM_IOCTL_MODE_DESTROY_DUMB, &mut destroy as *mut _);
        }
    }
}

/// While PC owns the display, the kernel must not switch consoles behind
/// its back: the keyboard would move to the other console while the
/// picture stayed up, leaving no way to close it. In VT_PROCESS mode it
/// asks first instead — SIGUSR1 to leave this console, SIGUSR2 on coming
/// back — and waits for VT_RELDISP; `Screen::follow_console_switches`
/// answers. Works on stdin, which `on_console` checked is this console.
struct VtSwitch {
    saved: VtMode,
    release: Arc<AtomicBool>,
    acquire: Arc<AtomicBool>,
    signals: [SigId; 2],
}

impl VtSwitch {
    fn hold() -> Result<VtSwitch> {
        let mut saved = VtMode::default();
        ioctl(libc::STDIN_FILENO, VT_GETMODE, &mut saved, "VT_GETMODE")?;
        let release = Arc::new(AtomicBool::new(false));
        let acquire = Arc::new(AtomicBool::new(false));
        let signals = [
            signal_hook::flag::register(libc::SIGUSR1, release.clone())?,
            signal_hook::flag::register(libc::SIGUSR2, acquire.clone())?,
        ];
        let mut mode = VtMode {
            mode: VT_PROCESS,
            relsig: libc::SIGUSR1 as i16,
            acqsig: libc::SIGUSR2 as i16,
            ..Default::default()
        };
        if let Err(err) = ioctl(libc::STDIN_FILENO, VT_SETMODE, &mut mode, "VT_SETMODE") {
            for id in signals {
                signal_hook::low_level::unregister(id);
            }
            return Err(err);
        }
        Ok(VtSwitch {
            saved,
            release,
            acquire,
            signals,
        })
    }
}

impl Drop for VtSwitch {
    fn drop(&mut self) {
        unsafe {
            // A switch requested but not yet answered; VT_RELDISP is only
            // valid while still in VT_PROCESS mode.
            if self.release.load(Ordering::Relaxed) {
                libc::ioctl(libc::STDIN_FILENO, VT_RELDISP, 1 as libc::c_ulong);
            }
            libc::ioctl(
                libc::STDIN_FILENO,
                VT_SETMODE,
                &mut self.saved as *mut VtMode,
            );
        }
        for id in self.signals {
            signal_hook::low_level::unregister(id);
        }
    }
}

/// A connected monitor: its preferred mode and the CRTC to drive it with.
struct Output {
    connector_id: u32,
    crtc_id: u32,
    mode: DrmModeModeinfo,
    /// Whether the console is showing on it right now.
    lit: bool,
}

/// The first `/dev/dri/cardN` showing the console on a monitor, or else
/// the first with one plugged in. Card numbers vary (a firmware
/// framebuffer can hold card0 early in boot), so try them all.
fn find_output() -> Result<(File, Output)> {
    let mut dark = None;
    for n in 0..16 {
        let path = format!("/dev/dri/card{n}");
        let Ok(card) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        // A render-only or virtual card may refuse KMS queries; skip it.
        match connected_output(&card) {
            Ok(Some(output)) if output.lit => return Ok((card, output)),
            Ok(Some(output)) if dark.is_none() => dark = Some((card, output)),
            _ => {}
        }
    }
    dark.context("no graphics card with a connected monitor in /dev/dri")
}

/// Every DRM query is asked twice: first for the counts, then again with
/// arrays of that size for the kernel to fill. A monitor the console shows
/// on wins; failing that, a dark one with a free CRTC that can drive it.
fn connected_output(card: &File) -> Result<Option<Output>> {
    let fd = card.as_raw_fd();
    let mut res = DrmModeCardRes::default();
    ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &mut res, "GETRESOURCES")?;
    let mut connectors = vec![0u32; res.count_connectors as usize];
    let mut crtcs = vec![0u32; res.count_crtcs as usize];
    res = DrmModeCardRes {
        connector_id_ptr: connectors.as_mut_ptr() as u64,
        crtc_id_ptr: crtcs.as_mut_ptr() as u64,
        count_connectors: res.count_connectors,
        count_crtcs: res.count_crtcs,
        ..Default::default()
    };
    ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &mut res, "GETRESOURCES")?;
    // A monitor plugged in between the two calls could change the counts.
    connectors.truncate(res.count_connectors as usize);
    crtcs.truncate(res.count_crtcs as usize);

    let mut dark = None;
    for &connector_id in &connectors {
        let mut conn = DrmModeGetConnector {
            connector_id,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &mut conn, "GETCONNECTOR")?;
        if conn.connection != DRM_MODE_CONNECTED || conn.count_modes == 0 {
            continue;
        }
        let mut modes = vec![DrmModeModeinfo::default(); conn.count_modes as usize];
        let mut encoders = vec![0u32; conn.count_encoders as usize];
        conn = DrmModeGetConnector {
            connector_id,
            modes_ptr: modes.as_mut_ptr() as u64,
            count_modes: conn.count_modes,
            encoders_ptr: encoders.as_mut_ptr() as u64,
            count_encoders: conn.count_encoders,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &mut conn, "GETCONNECTOR")?;
        modes.truncate(conn.count_modes as usize);
        encoders.truncate(conn.count_encoders as usize);
        let Some(&mode) = modes
            .iter()
            .find(|mode| mode.type_ & DRM_MODE_TYPE_PREFERRED != 0)
            .or(modes.first())
        else {
            continue;
        };

        // The CRTC the console is using for this monitor, reached through
        // its current encoder.
        if let Some(enc) = get_encoder(fd, conn.encoder_id)
            && enc.crtc_id != 0
        {
            return Ok(Some(Output {
                connector_id,
                crtc_id: enc.crtc_id,
                mode,
                lit: true,
            }));
        }
        if dark.is_some() {
            continue;
        }
        // `possible_crtcs` is a bitmask over positions in `crtcs`.
        let possible = encoders
            .iter()
            .filter_map(|&id| get_encoder(fd, id))
            .fold(0, |mask, enc| mask | enc.possible_crtcs);
        let free = |crtc_id| {
            let mut crtc = DrmModeCrtc {
                crtc_id,
                ..Default::default()
            };
            ioctl(fd, DRM_IOCTL_MODE_GETCRTC, &mut crtc, "GETCRTC").is_ok() && crtc.mode_valid == 0
        };
        if let Some(crtc_id) = pick_crtc(possible, &crtcs, free) {
            dark = Some(Output {
                connector_id,
                crtc_id,
                mode,
                lit: false,
            });
        }
    }
    Ok(dark)
}

fn get_encoder(fd: i32, encoder_id: u32) -> Option<DrmModeGetEncoder> {
    let mut enc = DrmModeGetEncoder {
        encoder_id,
        ..Default::default()
    };
    (encoder_id != 0 && ioctl(fd, DRM_IOCTL_MODE_GETENCODER, &mut enc, "GETENCODER").is_ok())
        .then_some(enc)
}

/// The first CRTC in `possible` (a bitmask over positions in `crtcs`) that
/// is `free` — not one driving another monitor, which would go dark.
fn pick_crtc(possible: u32, crtcs: &[u32], free: impl Fn(u32) -> bool) -> Option<u32> {
    crtcs
        .iter()
        .enumerate()
        .filter(|&(i, _)| i < 32 && possible & (1 << i) != 0)
        .map(|(_, &id)| id)
        .find(|&id| free(id))
}

/// Scales `picture` (nearest neighbor) to fit `width`×`height` keeping its
/// proportions, centered on black. `screen` is XRGB8888: each pixel is a
/// little-endian u32, i.e. bytes B, G, R, unused.
/// `screen` still holds an older picture, so the bars around this one are
/// cleared too.
fn draw_fit(picture: &RgbImage, screen: &mut [u8], width: usize, height: usize, pitch: usize) {
    let (pw, ph) = (picture.width() as usize, picture.height() as usize);
    let (dw, dh) = if pw == 0 || ph == 0 {
        (0, 0)
    } else {
        fit(pw, ph, width, height)
    };
    let (ox, oy) = ((width - dw) / 2, (height - dh) / 2);
    let src = picture.as_raw();
    // Byte offset in a source row for each screen column.
    let src_x: Vec<usize> = (0..dw).map(|x| x * pw / dw * 3).collect();
    for y in 0..height {
        let row = &mut screen[y * pitch..][..width * 4];
        if !(oy..oy + dh).contains(&y) {
            row.fill(0);
            continue;
        }
        let (left, rest) = row.split_at_mut(ox * 4);
        let (middle, right) = rest.split_at_mut(dw * 4);
        left.fill(0);
        right.fill(0);
        let src_row = &src[(y - oy) * ph / dh * pw * 3..][..pw * 3];
        for (out, &sx) in middle.as_chunks_mut::<4>().0.iter_mut().zip(&src_x) {
            *out = [src_row[sx + 2], src_row[sx + 1], src_row[sx], 0];
        }
    }
}

/// The largest size with `pw`:`ph` proportions that fits in `w`×`h`.
fn fit(pw: usize, ph: usize, w: usize, h: usize) -> (usize, usize) {
    if pw * h > ph * w {
        (w, (ph * w / pw).max(1))
    } else {
        ((pw * h / ph).max(1), h)
    }
}

fn ioctl<T>(fd: i32, request: libc::Ioctl, arg: &mut T, name: &str) -> Result<()> {
    if unsafe { libc::ioctl(fd, request, arg as *mut T) } < 0 {
        return Err(io::Error::last_os_error()).context(name.to_string());
    }
    Ok(())
}

// Linux's `_IO`/`_IOWR` encoding for DRM's ioctl type 'd': direction in the
// top two bits, argument size, type, command number.
const fn drm_io(nr: u8) -> libc::Ioctl {
    ((b'd' as u32) << 8 | nr as u32) as libc::Ioctl
}

const fn drm_iowr<T>(nr: u8) -> libc::Ioctl {
    (3u32 << 30 | (size_of::<T>() as u32) << 16 | (b'd' as u32) << 8 | nr as u32) as libc::Ioctl
}

const DRM_IOCTL_SET_MASTER: libc::Ioctl = drm_io(0x1e);
const DRM_IOCTL_DROP_MASTER: libc::Ioctl = drm_io(0x1f);
const DRM_IOCTL_MODE_GETRESOURCES: libc::Ioctl = drm_iowr::<DrmModeCardRes>(0xa0);
const DRM_IOCTL_MODE_GETCRTC: libc::Ioctl = drm_iowr::<DrmModeCrtc>(0xa1);
const DRM_IOCTL_MODE_SETCRTC: libc::Ioctl = drm_iowr::<DrmModeCrtc>(0xa2);
const DRM_IOCTL_MODE_GETENCODER: libc::Ioctl = drm_iowr::<DrmModeGetEncoder>(0xa6);
const DRM_IOCTL_MODE_GETCONNECTOR: libc::Ioctl = drm_iowr::<DrmModeGetConnector>(0xa7);
const DRM_IOCTL_MODE_ADDFB: libc::Ioctl = drm_iowr::<DrmModeFbCmd>(0xae);
const DRM_IOCTL_MODE_RMFB: libc::Ioctl = drm_iowr::<u32>(0xaf);
const DRM_IOCTL_MODE_CREATE_DUMB: libc::Ioctl = drm_iowr::<DrmModeCreateDumb>(0xb2);
const DRM_IOCTL_MODE_MAP_DUMB: libc::Ioctl = drm_iowr::<DrmModeMapDumb>(0xb3);
const DRM_IOCTL_MODE_DESTROY_DUMB: libc::Ioctl = drm_iowr::<DrmModeDestroyDumb>(0xb4);

// From <linux/vt.h>.
const VT_GETMODE: libc::Ioctl = 0x5601;
const VT_SETMODE: libc::Ioctl = 0x5602;
const VT_RELDISP: libc::Ioctl = 0x5605;
const VT_PROCESS: u8 = 1;
const VT_ACKACQ: u8 = 2;

const DRM_MODE_CONNECTED: u32 = 1;
const DRM_MODE_TYPE_PREFERRED: u32 = 1 << 3;

// Mirrors of the structs in the kernel's `<drm/drm_mode.h>` and
// `<linux/vt.h>`, field for field.

#[repr(C)]
#[derive(Default)]
struct VtMode {
    mode: u8,
    waitv: u8,
    relsig: i16,
    acqsig: i16,
    frsig: i16,
}

#[repr(C)]
#[derive(Default)]
struct DrmModeCardRes {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DrmModeModeinfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    type_: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct DrmModeCrtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: DrmModeModeinfo,
}

#[repr(C)]
#[derive(Default)]
struct DrmModeGetEncoder {
    encoder_id: u32,
    encoder_type: u32,
    crtc_id: u32,
    possible_crtcs: u32,
    possible_clones: u32,
}

#[repr(C)]
#[derive(Default)]
struct DrmModeGetConnector {
    encoders_ptr: u64,
    modes_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    count_modes: u32,
    count_props: u32,
    count_encoders: u32,
    encoder_id: u32,
    connector_id: u32,
    connector_type: u32,
    connector_type_id: u32,
    connection: u32,
    mm_width: u32,
    mm_height: u32,
    subpixel: u32,
    pad: u32,
}

#[repr(C)]
#[derive(Default)]
struct DrmModeFbCmd {
    fb_id: u32,
    width: u32,
    height: u32,
    pitch: u32,
    bpp: u32,
    depth: u32,
    handle: u32,
}

#[repr(C)]
#[derive(Default)]
struct DrmModeCreateDumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Default)]
struct DrmModeMapDumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
struct DrmModeDestroyDumb {
    handle: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_layouts_match_kernel_headers() {
        // sizeof() of each struct as compiled against <drm/drm.h>.
        assert_eq!(size_of::<DrmModeCardRes>(), 64);
        assert_eq!(size_of::<DrmModeModeinfo>(), 68);
        assert_eq!(size_of::<DrmModeCrtc>(), 104);
        assert_eq!(size_of::<DrmModeGetEncoder>(), 20);
        assert_eq!(size_of::<DrmModeGetConnector>(), 80);
        assert_eq!(size_of::<DrmModeFbCmd>(), 28);
        assert_eq!(size_of::<DrmModeCreateDumb>(), 32);
        assert_eq!(size_of::<DrmModeMapDumb>(), 16);
        assert_eq!(size_of::<DrmModeDestroyDumb>(), 4);
        assert_eq!(size_of::<VtMode>(), 8);
    }

    #[test]
    fn ioctl_numbers_match_kernel_headers() {
        // The DRM_IOCTL_* macro values from <drm/drm.h>.
        assert_eq!(DRM_IOCTL_SET_MASTER, 0x641e);
        assert_eq!(DRM_IOCTL_DROP_MASTER, 0x641f);
        assert_eq!(DRM_IOCTL_MODE_GETRESOURCES, 0xc04064a0);
        assert_eq!(DRM_IOCTL_MODE_GETCRTC, 0xc06864a1);
        assert_eq!(DRM_IOCTL_MODE_SETCRTC, 0xc06864a2);
        assert_eq!(DRM_IOCTL_MODE_GETENCODER, 0xc01464a6);
        assert_eq!(DRM_IOCTL_MODE_GETCONNECTOR, 0xc05064a7);
        assert_eq!(DRM_IOCTL_MODE_ADDFB, 0xc01c64ae);
        assert_eq!(DRM_IOCTL_MODE_RMFB, 0xc00464af);
        assert_eq!(DRM_IOCTL_MODE_CREATE_DUMB, 0xc02064b2);
        assert_eq!(DRM_IOCTL_MODE_MAP_DUMB, 0xc01064b3);
        assert_eq!(DRM_IOCTL_MODE_DESTROY_DUMB, 0xc00464b4);
    }

    #[test]
    fn console_paths() {
        assert!(is_console_path(Path::new("/dev/tty3")));
        assert!(is_console_path(Path::new("/dev/tty12")));
        assert!(!is_console_path(Path::new("/dev/tty")));
        assert!(!is_console_path(Path::new("/dev/ttyS0")));
        assert!(!is_console_path(Path::new("/dev/pts/2")));
    }

    #[test]
    fn image_extensions() {
        assert!(is_image("a.JPG"));
        assert!(is_image("b.webp"));
        assert!(!is_image("c.svg"));
        assert!(!is_image("jpg"));
    }

    #[test]
    fn mode_choices_avoid_modeset_and_blur() {
        let mode = |w: u16, h: u16, refresh: u32| DrmModeModeinfo {
            hdisplay: w,
            vdisplay: h,
            vrefresh: refresh,
            ..Default::default()
        };
        let sizes = |modes: Vec<DrmModeModeinfo>| {
            modes
                .iter()
                .map(|m| (m.hdisplay, m.vdisplay, m.vrefresh))
                .collect::<Vec<_>>()
        };
        let preferred = mode(2560, 1440, 144);
        // Same size: the console's mode as it is, refresh rate included.
        assert_eq!(
            sizes(mode_choices(Some(mode(2560, 1440, 60)), preferred)),
            [(2560, 1440, 60)]
        );
        assert_eq!(
            sizes(mode_choices(Some(mode(1280, 720, 60)), preferred)),
            [(2560, 1440, 144), (1280, 720, 60)]
        );
        assert_eq!(sizes(mode_choices(None, preferred)), [(2560, 1440, 144)]);
    }

    #[test]
    fn pick_crtc_takes_a_free_possible_one() {
        let crtcs = [40, 41, 42];
        assert_eq!(pick_crtc(0b110, &crtcs, |_| true), Some(41));
        assert_eq!(pick_crtc(0b110, &crtcs, |id| id != 41), Some(42));
        assert_eq!(pick_crtc(0b001, &crtcs, |id| id != 40), None);
        assert_eq!(pick_crtc(0, &crtcs, |_| true), None);
    }

    #[test]
    fn fit_keeps_proportions() {
        assert_eq!(fit(5009, 3404, 2560, 1440), (2118, 1440));
        assert_eq!(fit(4000, 1000, 2560, 1440), (2560, 640));
        assert_eq!(fit(100, 100, 2560, 1440), (1440, 1440));
    }

    #[test]
    fn draw_fit_centers_and_swaps_to_bgr() {
        let picture = RgbImage::from_pixel(1, 1, image::Rgb([10, 20, 30]));
        // 4×2 screen with a padded 20-byte pitch: the image becomes a 2×2
        // square in columns 1–2.
        let mut screen = vec![0xff; 20 * 2];
        draw_fit(&picture, &mut screen, 4, 2, 20);
        for y in 0..2 {
            let row = &screen[y * 20..][..16];
            assert_eq!(row, [0, 0, 0, 0, 30, 20, 10, 0, 30, 20, 10, 0, 0, 0, 0, 0]);
        }
    }

    #[test]
    fn draw_fit_scales_and_clears_bars_above_and_below() {
        // A 2×1 picture (red, blue) on a 4×4 screen: rows 1–2, two pixels
        // per source pixel; rows 0 and 3 are bars over the old content.
        let mut picture = RgbImage::new(2, 1);
        picture.put_pixel(0, 0, image::Rgb([255, 0, 0]));
        picture.put_pixel(1, 0, image::Rgb([0, 0, 255]));
        let mut screen = vec![0xff; 16 * 4];
        draw_fit(&picture, &mut screen, 4, 4, 16);
        let red = [0, 0, 255, 0];
        let blue = [255, 0, 0, 0];
        assert_eq!(screen[..16], [0; 16]);
        for y in 1..3 {
            assert_eq!(screen[y * 16..][..16], [red, red, blue, blue].concat());
        }
        assert_eq!(screen[48..], [0; 16]);
    }

    /// Reads the real card without touching the screen (no master needed).
    /// Run by hand: `cargo test tty_image -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe_real_card() {
        let (card, output) = find_output().unwrap();
        let show = |m: &DrmModeModeinfo| format!("{}x{}@{}", m.hdisplay, m.vdisplay, m.vrefresh);
        println!(
            "connector {} crtc {} lit {} preferred {}",
            output.connector_id,
            output.crtc_id,
            output.lit,
            show(&output.mode)
        );
        let mut crtc = DrmModeCrtc {
            crtc_id: output.crtc_id,
            ..Default::default()
        };
        ioctl(
            card.as_raw_fd(),
            DRM_IOCTL_MODE_GETCRTC,
            &mut crtc,
            "GETCRTC",
        )
        .unwrap();
        let console = (crtc.mode_valid != 0).then_some(crtc.mode);
        println!(
            "now on the crtc: {}; viewer would try: {}",
            console.as_ref().map_or("off".to_string(), show),
            mode_choices(console, output.mode)
                .iter()
                .map(show)
                .collect::<Vec<_>>()
                .join(", then ")
        );
    }
}
