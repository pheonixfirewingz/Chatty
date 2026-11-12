//! Keeps chat content out of screenshots and screen recordings.
//!
//! Release builds ask the operating system to leave the window out of
//! captures on Windows and macOS. Wayland has no client-side equivalent, so
//! Linux release builds hide content while the window is unfocused and set a
//! stable app ID that compositor capture-exclusion rules can match. Debug
//! builds and `--inspect` stay capturable for development.

use iced::window;

/// Wayland app ID and X11 class; matches the Flatpak ID.
pub(crate) const APP_ID: &str = "io.github.pheonixfirewingz.Chatty";

/// Whether this build protects its window from capture.
pub(crate) const ENABLED: bool = !cfg!(debug_assertions);

/// Whether content must be hidden while the window is unfocused, because the
/// platform offers no capture exclusion the app can request.
pub(crate) const HIDE_UNFOCUSED: bool = !cfg!(any(target_os = "windows", target_os = "macos"));

/// Excludes the window from screen capture where the platform supports it.
/// Returns whether the request succeeded.
pub(crate) fn exclude_from_capture(window: &dyn window::Window) -> bool {
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    platform::exclude(handle.as_raw())
}

#[cfg(target_os = "windows")]
mod platform {
    use iced::window::raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR,
    };

    pub(super) fn exclude(handle: RawWindowHandle) -> bool {
        let RawWindowHandle::Win32(handle) = handle else {
            return false;
        };
        let hwnd = handle.hwnd.get() as HWND;
        // SAFETY: the handle comes from the live window on its own thread.
        // WDA_EXCLUDEFROMCAPTURE needs Windows 10 2004; older versions fall
        // back to WDA_MONITOR, which captures the window as black.
        unsafe {
            SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) != 0
                || SetWindowDisplayAffinity(hwnd, WDA_MONITOR) != 0
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use iced::window::raw_window_handle::RawWindowHandle;
    use objc2_app_kit::{NSView, NSWindowSharingType};

    pub(super) fn exclude(handle: RawWindowHandle) -> bool {
        let RawWindowHandle::AppKit(handle) = handle else {
            return false;
        };
        // SAFETY: the handle is the live window's NSView, and iced runs
        // window callbacks on the main thread.
        let view: &NSView = unsafe { handle.ns_view.cast().as_ref() };
        let Some(window) = view.window() else {
            return false;
        };
        window.setSharingType(NSWindowSharingType::None);
        true
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use iced::window::raw_window_handle::RawWindowHandle;

    pub(super) fn exclude(_: RawWindowHandle) -> bool {
        false
    }
}
