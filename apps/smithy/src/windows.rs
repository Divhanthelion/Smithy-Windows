//! Windows-only window placement.
//!
//! The window opens at a fixed 1440×900 logical size. On a 1920×1080 laptop
//! at 125% scaling the screen is 1536×864 logical, so the window was taller
//! than the display: its bottom — the frame's rail, where the fisherman lives
//! — hung off the edge of the screen.

use floem::peniko::kurbo::{Point, Size};
use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWORKAREA};

/// The wanted size, shrunk to fit the work area, and where to put it.
///
/// The work area is the screen less the taskbar. Called before the event loop
/// has made the process DPI-aware, Windows reports it in 96-DPI units and
/// `GetDpiForSystem` says 96; after, it reports physical pixels and the real
/// DPI. Dividing one by the other is logical pixels either way.
pub fn fit_to_work_area(wanted: Size) -> Option<(Size, Point)> {
    let mut area = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: SPI_GETWORKAREA writes one RECT through the pointer, which
    // points at a RECT we own for the duration of the call.
    let ok =
        unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut area as *mut RECT).cast(), 0) };
    if ok == 0 {
        return None;
    }
    // SAFETY: no arguments; returns the system DPI.
    let scale = f64::from(unsafe { GetDpiForSystem() }).max(96.0) / 96.0;
    let (left, top) = (f64::from(area.left) / scale, f64::from(area.top) / scale);
    let width = f64::from(area.right - area.left) / scale;
    let height = f64::from(area.bottom - area.top) / scale;

    // The title bar is outside the client size floem sets; leave room for it
    // and for the window border, so the frame's bottom rail is on screen.
    const TITLE_BAR: f64 = 40.0;
    let fitted = Size::new(
        wanted.width.min(width * 0.96),
        wanted.height.min(height - TITLE_BAR - 8.0),
    );
    let origin = Point::new(
        left + (width - fitted.width) / 2.0,
        top + ((height - fitted.height - TITLE_BAR) / 2.0).max(0.0),
    );
    Some((fitted, origin))
}
