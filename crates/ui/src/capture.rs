//! One-shot desktop screen capture, used by the RWS calibration reference frame.
//!
//! Implemented with a plain GDI `BitBlt` of the primary screen DC. Two useful
//! properties for our case: (1) a plain `SRCCOPY` blit (no `CAPTUREBLT`) skips
//! layered windows, so our transparent always-on-top overlay is NOT captured —
//! the game behind it is; (2) it needs no D3D/WinRT plumbing. The trade-off is
//! that a game in true *exclusive* fullscreen blits black; those games should be
//! run borderless (as overlays require anyway). The public signature returns CPU
//! pixels so the caller loads the texture into the right egui context.

/// Capture the primary monitor as an egui image, or `None` on any failure (a
/// black exclusive-fullscreen game included — nothing here ever panics).
#[cfg(windows)]
pub fn capture_primary_monitor() -> Option<egui::ColorImage> {
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

#[cfg(not(windows))]
pub fn capture_primary_monitor() -> Option<egui::ColorImage> {
    None
}
