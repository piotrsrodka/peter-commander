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

use anyhow::{Context, Result, bail};
use image::RgbImage;
use signal_hook::SigId;

/// Formats the viewer can decode (the `image` crate features enabled in
/// Cargo.toml). Anything else still goes to `xdg-open`.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp"];

pub fn is_image(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
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
    /// Our framebuffer on the same CRTC, set again on coming back.
    picture_crtc: DrmModeCrtc,
    /// Whether our picture is on the monitor right now, i.e. this console
    /// is the active one.
    on_screen: bool,
    fb_id: u32,
    handle: u32,
    map: *mut u8,
    size: usize,
    width: usize,
    height: usize,
    pitch: usize,
}

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

        // A "dumb buffer": plain pixels in memory the kernel can scan out, no
        // GPU rendering involved. Registered as a framebuffer, then mapped so
        // it can be written like an ordinary array. On an early error, closing
        // the card frees them.
        let mode = output.mode;
        let mut dumb = DrmModeCreateDumb {
            width: mode.hdisplay.into(),
            height: mode.vdisplay.into(),
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

        let mut screen = Screen {
            card,
            vt,
            connector_id: output.connector_id,
            console_crtc,
            // The actual mode setting: this CRTC, in this mode, scanning out
            // this framebuffer (to this connector, added by `set_crtc`).
            picture_crtc: DrmModeCrtc {
                crtc_id: output.crtc_id,
                fb_id: fb.fb_id,
                mode_valid: 1,
                mode,
                ..Default::default()
            },
            on_screen: false,
            fb_id: fb.fb_id,
            handle: dumb.handle,
            map: map as *mut u8,
            size,
            width: dumb.width as usize,
            height: dumb.height as usize,
            pitch: dumb.pitch as usize,
        };
        screen.set_crtc(screen.picture_crtc)?;
        screen.on_screen = true;
        Ok(screen)
    }

    /// Replaces the picture with `picture`, fit and centered on black. The
    /// buffer is the one being scanned out, so the change shows at once.
    pub fn draw(&mut self, picture: &RgbImage) {
        let pixels = unsafe { std::slice::from_raw_parts_mut(self.map, self.size) };
        draw_fit(picture, pixels, self.width, self.height, self.pitch);
    }

    /// Answers the kernel's console-switch requests (Ctrl+Alt+Fn) — call
    /// this often, as the switch waits for it. Leaving: the console's
    /// buffer goes back, the display is released and the switch allowed.
    /// Coming back: the display is taken again and the picture, still in
    /// its buffer, put back. An error means the display couldn't be taken
    /// back (someone else holds it); the picture is then gone.
    pub fn follow_console_switches(&mut self) -> Result<()> {
        let fd = self.card.as_raw_fd();
        if self.vt.release.swap(false, Ordering::Relaxed) {
            if self.on_screen {
                let _ = self.set_crtc(self.console_crtc);
                self.on_screen = false;
            }
            unsafe {
                libc::ioctl(fd, DRM_IOCTL_DROP_MASTER);
                libc::ioctl(libc::STDIN_FILENO, VT_RELDISP, 1 as libc::c_ulong);
            }
        }
        if self.vt.acquire.swap(false, Ordering::Relaxed) {
            unsafe {
                libc::ioctl(libc::STDIN_FILENO, VT_RELDISP, VT_ACKACQ as libc::c_ulong);
            }
            if unsafe { libc::ioctl(fd, DRM_IOCTL_SET_MASTER) } < 0 {
                return Err(io::Error::last_os_error()).context("taking the screen back");
            }
            self.set_crtc(self.picture_crtc)?;
            self.on_screen = true;
        }
        Ok(())
    }

    /// SETCRTC with `crtc` driving our one connector.
    fn set_crtc(&self, mut crtc: DrmModeCrtc) -> Result<()> {
        crtc.set_connectors_ptr = &self.connector_id as *const u32 as u64;
        crtc.count_connectors = 1;
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
        unsafe {
            libc::munmap(self.map as *mut libc::c_void, self.size);
            libc::ioctl(fd, DRM_IOCTL_MODE_RMFB, &mut self.fb_id as *mut u32);
            let mut destroy = DrmModeDestroyDumb {
                handle: self.handle,
            };
            libc::ioctl(fd, DRM_IOCTL_MODE_DESTROY_DUMB, &mut destroy as *mut _);
        }
        // Closing the card drops master, so the console can draw again.
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

/// A connected monitor: its preferred mode and the CRTC currently driving it.
struct Output {
    connector_id: u32,
    crtc_id: u32,
    mode: DrmModeModeinfo,
}

/// The first `/dev/dri/cardN` with a monitor plugged in. Card numbers vary
/// (a firmware framebuffer can hold card0 early in boot), so try them all.
fn find_output() -> Result<(File, Output)> {
    for n in 0..16 {
        let path = format!("/dev/dri/card{n}");
        let Ok(card) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        // A render-only or virtual card may refuse KMS queries; skip it.
        if let Ok(Some(output)) = connected_output(&card) {
            return Ok((card, output));
        }
    }
    bail!("no graphics card with a connected monitor in /dev/dri")
}

/// Every DRM query is asked twice: first for the counts, then again with
/// arrays of that size for the kernel to fill.
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
        conn = DrmModeGetConnector {
            connector_id,
            modes_ptr: modes.as_mut_ptr() as u64,
            count_modes: conn.count_modes,
            ..Default::default()
        };
        ioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &mut conn, "GETCONNECTOR")?;
        modes.truncate(conn.count_modes as usize);
        let Some(&mode) = modes
            .iter()
            .find(|mode| mode.type_ & DRM_MODE_TYPE_PREFERRED != 0)
            .or(modes.first())
        else {
            continue;
        };

        // The CRTC the console is using for this monitor, reached through
        // its encoder; any CRTC will do if the monitor is currently dark.
        let mut enc = DrmModeGetEncoder {
            encoder_id: conn.encoder_id,
            ..Default::default()
        };
        let crtc_id = if conn.encoder_id != 0
            && ioctl(fd, DRM_IOCTL_MODE_GETENCODER, &mut enc, "GETENCODER").is_ok()
            && enc.crtc_id != 0
        {
            enc.crtc_id
        } else if let Some(&crtc_id) = crtcs.first() {
            crtc_id
        } else {
            continue;
        };
        return Ok(Some(Output {
            connector_id,
            crtc_id,
            mode,
        }));
    }
    Ok(None)
}

/// Scales `picture` (nearest neighbor) to fit `width`×`height` keeping its
/// proportions, centered on black. `screen` is XRGB8888: each pixel is a
/// little-endian u32, i.e. bytes B, G, R, unused.
fn draw_fit(picture: &RgbImage, screen: &mut [u8], width: usize, height: usize, pitch: usize) {
    screen.fill(0);
    let (pw, ph) = (picture.width() as usize, picture.height() as usize);
    if pw == 0 || ph == 0 {
        return;
    }
    let (dw, dh) = fit(pw, ph, width, height);
    let (ox, oy) = ((width - dw) / 2, (height - dh) / 2);
    for y in 0..dh {
        let row = &mut screen[(oy + y) * pitch + ox * 4..][..dw * 4];
        let sy = (y * ph / dh) as u32;
        for (x, out) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let [r, g, b] = picture.get_pixel((x * pw / dw) as u32, sy).0;
            *out = [b, g, r, 0];
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

    /// Reads the real card without touching the screen (no master needed).
    /// Run by hand: `cargo test tty_image -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn probe_real_card() {
        let (_, output) = find_output().unwrap();
        println!(
            "connector {} crtc {} {}x{}@{}",
            output.connector_id,
            output.crtc_id,
            output.mode.hdisplay,
            output.mode.vdisplay,
            output.mode.vrefresh
        );
    }
}
