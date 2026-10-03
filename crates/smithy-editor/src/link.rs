//! Opening Smithy's own web pages in the default browser.
//!
//! Only for addresses written here, never for a link a model wrote: an agent
//! reply that says "open this" is text to read, not a page to launch.

/// Where a reply the user objects to gets reported. The Microsoft Store asks
/// any app that shows generated content for a way to tell its developer about
/// it (Store Policies 11.16); this is that way, from every reply and from the
/// Agent menu.
pub const REPORT_URL: &str =
    "https://github.com/Divhanthelion/Smithy-Windows/issues/new?template=ai-content.yml";

/// Opens `url` in the default browser. Refuses anything that is not https.
pub fn open_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err(format!("not an https address: {url}"));
    }
    open(url)
}

#[cfg(windows)]
fn open(url: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let wide = |s: &str| -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let (verb, target) = (wide("open"), wide(url));
    // SAFETY: both strings are NUL-terminated and outlive the call; the other
    // pointers may be null.
    let code = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Success is any value above 32; at or below it is an error code.
    if code as isize > 32 {
        Ok(())
    } else {
        Err(format!("the browser did not open (code {})", code as isize))
    }
}

#[cfg(not(windows))]
fn open(url: &str) -> Result<(), String> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url)
        .spawn()
        .map(drop)
        .map_err(|e| format!("{program}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_anything_but_https() {
        assert!(open_url("http://example.com").is_err());
        assert!(open_url("file:///C:/Windows/System32/calc.exe").is_err());
        assert!(open_url("calc.exe").is_err());
    }

    #[test]
    fn the_report_form_is_on_the_project_page() {
        assert!(REPORT_URL.starts_with("https://github.com/Divhanthelion/Smithy-Windows/"));
    }
}
