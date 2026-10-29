//! One-shot desktop screen capture, used by the RWS calibration reference frame.
//!
//! Windows: a plain GDI `BitBlt` of the primary screen DC. Two useful
//! properties for our case: (1) a plain `SRCCOPY` blit (no `CAPTUREBLT`) skips
//! layered windows, so our transparent always-on-top overlay is NOT captured —
//! the game behind it is; (2) it needs no D3D/WinRT plumbing. The trade-off is
//! that a game in true *exclusive* fullscreen blits black; those games should be
//! run borderless (as overlays require anyway).
//!
//! Linux: KWin's screenshot interface over D-Bus, so KDE Plasma only. KWin
//! renders the shot and writes it down a pipe, which takes a frame or two, so
//! a capture is a [`PendingCapture`] the caller polls. Either way the result
//! is CPU pixels, so the caller loads the texture into the right egui context.

use std::sync::{Arc, Mutex};

/// A capture on its way: Windows has it at once, KWin a moment later.
#[derive(Clone)]
pub struct PendingCapture(Arc<Mutex<Option<Option<egui::ColorImage>>>>);

impl PendingCapture {
    fn new(image: Option<Option<egui::ColorImage>>) -> Self {
        Self(Arc::new(Mutex::new(image)))
    }

    /// The image once the capture is over, `Some(None)` if it failed. Handed
    /// out once.
    pub fn take(&self) -> Option<Option<egui::ColorImage>> {
        self.0.lock().ok()?.take()
    }
}

/// Capture the primary monitor (on KDE, the active screen) without
/// FlexInput's overlays on it. Nothing here ever panics; a failure, a black
/// exclusive-fullscreen game included, is just no image.
pub fn capture_primary_monitor() -> PendingCapture {
    #[cfg(windows)]
    return PendingCapture::new(Some(blit_primary_monitor()));

    #[cfg(target_os = "linux")]
    {
        let pending = PendingCapture::new(None);
        let slot = pending.0.clone();
        let spawned = std::thread::Builder::new().name("screen-capture".into()).spawn(move || {
            let image = kwin::capture().inspect_err(|e| eprintln!("[capture] {e}")).ok();
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(image);
            }
        });
        if spawned.is_err() {
            return PendingCapture::new(Some(None));
        }
        pending
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    PendingCapture::new(Some(None))
}

#[cfg(windows)]
fn blit_primary_monitor() -> Option<egui::ColorImage> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HDC,
        HGDIOBJ, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        if w <= 0 || h <= 0 {
            return None;
        }
        let screen = GetDC(None);
        if screen.0.is_null() {
            return None;
        }

        // All the fallible GDI work in one closure so we always ReleaseDC below.
        let work = || -> Option<egui::ColorImage> {
            let mem = CreateCompatibleDC(Some(screen));
            if mem.0.is_null() {
                return None;
            }
            let bmp = CreateCompatibleBitmap(screen, w, h);
            if bmp.0.is_null() {
                let _ = DeleteDC(mem);
                return None;
            }
            let old = SelectObject(mem, HGDIOBJ(bmp.0));
            // Plain SRCCOPY (no CAPTUREBLT) → layered windows (our overlay) are
            // excluded; the composited game shows through.
            let blt_ok = BitBlt(mem, 0, 0, w, h, Some(screen), 0, 0, SRCCOPY).is_ok();
            let img = if blt_ok { read_bits(mem, bmp.0, w, h) } else { None };
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            img
        };
        let out = work();
        ReleaseDC(None, screen);
        return out;

        // Read the DIB bits (top-down BGRA) and convert to opaque RGBA.
        unsafe fn read_bits(mem: HDC, bmp: *mut core::ffi::c_void, w: i32, h: i32) -> Option<egui::ColorImage> {
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = w;
            bmi.bmiHeader.biHeight = -h; // negative = top-down rows
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = 0; // BI_RGB
            let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
            let lines = GetDIBits(
                mem,
                windows::Win32::Graphics::Gdi::HBITMAP(bmp),
                0,
                h as u32,
                Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if lines == 0 {
                return None;
            }
            let mut px = Vec::with_capacity(buf.len());
            for c in buf.chunks_exact(4) {
                // BGRA → RGBA; GDI leaves alpha 0, so force opaque.
                px.push(c[2]);
                px.push(c[1]);
                px.push(c[0]);
                px.push(255);
            }
            Some(egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px))
        }
    }
}

/// KWin's `org.kde.KWin.ScreenShot2`. From interface version 5 (Plasma 6.6.2)
/// it captures the active screen and leaves the caller's windows (the
/// overlays) out by default. Before that it can't, so it captures the active
/// window instead: the game, since the overlays never take focus. KWin serves
/// only apps whose desktop entry lists the interface under
/// `X-KDE-DBUS-Restricted-Interfaces`, which `desktop_entry` writes.
#[cfg(target_os = "linux")]
mod kwin {
    use std::io::Read;
    use std::os::fd::{FromRawFd, IntoRawFd};
    use std::time::Duration;

    use dbus::arg::{PropMap, RefArg, Variant};
    use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;

    const SERVICE: &str = "org.kde.KWin.ScreenShot2";
    const PATH: &str = "/org/kde/KWin/ScreenShot2";
    const INTERFACE: &str = "org.kde.KWin.ScreenShot2";

    pub(super) fn capture() -> Result<egui::ColorImage, String> {
        let conn = dbus::blocking::Connection::new_session().map_err(|e| format!("no session bus: {e}"))?;
        let proxy = conn.with_proxy(SERVICE, PATH, Duration::from_secs(5));
        let version: u32 = proxy
            .get(INTERFACE, "Version")
            .map_err(|e| format!("screen capture needs KDE Plasma's KWin: {e}"))?;
        let mut options = PropMap::new();
        options.insert("native-resolution".into(), Variant(Box::new(true)));
        let (mut reader, writer) = std::io::pipe().map_err(|e| format!("pipe: {e}"))?;
        // SAFETY: the descriptor comes straight out of `into_raw_fd`, so
        // nothing else owns it.
        let fd = unsafe { dbus::arg::OwnedFd::from_raw_fd(writer.into_raw_fd()) };
        let method = if version >= 5 { "CaptureActiveScreen" } else { "CaptureActiveWindow" };
        // The call consumes our write end, so the read below ends when KWin
        // closes its copy.
        let (results,): (PropMap,) = proxy.method_call(INTERFACE, method, (options, fd)).map_err(|e| {
            let hint = if e.name() == Some("org.kde.KWin.ScreenShot2.Error.NoAuthorized") {
                " (KWin hasn't seen FlexInput's desktop entry yet; `kbuildsycoca6` refreshes KDE's copy)"
            } else {
                ""
            };
            format!("KWin {method}: {e}{hint}")
        })?;
        let field = |key: &str| results.get(key).and_then(|v| v.0.as_u64());
        let (Some(width), Some(height), Some(stride), Some(format)) =
            (field("width"), field("height"), field("stride"), field("format"))
        else {
            return Err(format!("KWin {method}: unexpected reply {results:?}"));
        };
        let mut data = Vec::new();
        reader.read_to_end(&mut data).map_err(|e| format!("reading the capture: {e}"))?;
        to_image(&data, width as usize, height as usize, stride as usize, format)
    }

    /// QImage pixels (KWin writes them raw) to opaque RGBA.
    fn to_image(data: &[u8], width: usize, height: usize, stride: usize, format: u64) -> Result<egui::ColorImage, String> {
        // QImage::Format: RGB32, ARGB32 and ARGB32_Premultiplied are 0xAARRGGBB
        // words (B, G, R, A in memory here); RGBX8888, RGBA8888 and
        // RGBA8888_Premultiplied are R, G, B, A bytes. KWin's GL path uses
        // the latter. Alpha is dropped: the screen is opaque.
        let bgra = match format {
            4..=6 => true,
            16..=18 => false,
            _ => return Err(format!("KWin capture in unsupported pixel format {format}")),
        };
        if width == 0 || height == 0 || stride < width * 4 || data.len() < stride * height {
            return Err(format!("KWin capture is {} bytes for {width}x{height}, stride {stride}", data.len()));
        }
        let mut px = Vec::with_capacity(width * height * 4);
        for row in data.chunks_exact(stride).take(height) {
            for c in row[..width * 4].chunks_exact(4) {
                let (r, b) = if bgra { (c[2], c[0]) } else { (c[0], c[2]) };
                px.extend_from_slice(&[r, c[1], b, 255]);
            }
        }
        Ok(egui::ColorImage::from_rgba_unmultiplied([width, height], &px))
    }

    #[cfg(test)]
    mod tests {
        use super::to_image;

        #[test]
        fn converts_both_byte_orders_and_skips_row_padding() {
            // 1x2, stride 8: one pixel and 4 padding bytes per row.
            let data = [1, 2, 3, 9, 0, 0, 0, 0, 4, 5, 6, 9, 0, 0, 0, 0];
            let rgba = to_image(&data, 1, 2, 8, 17).unwrap();
            assert_eq!(rgba.pixels, [egui::Color32::from_rgb(1, 2, 3), egui::Color32::from_rgb(4, 5, 6)]);
            let bgra = to_image(&data, 1, 2, 8, 6).unwrap();
            assert_eq!(bgra.pixels, [egui::Color32::from_rgb(3, 2, 1), egui::Color32::from_rgb(6, 5, 4)]);
            assert!(to_image(&data, 1, 2, 8, 30).is_err());
            assert!(to_image(&data[..12], 1, 2, 8, 17).is_err());
        }
    }
}
