use log::{error, info, warn};
use parking_lot::Mutex;
use std::ptr;
use std::sync::Arc;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, PostMessageW,
    PostQuitMessage, RegisterClassExW, TranslateMessage, WM_CLOSE, WM_DESTROY, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    WS_VISIBLE,
};
use windows::core::PCWSTR;

const CLASS_NAME: PCWSTR = windows::core::w!("SeelenCompositor");
const WINDOW_NAME: PCWSTR = windows::core::w!("Seelen Compositor");

/// Handle to the compositor window, shared across modules.
/// We use a raw pointer to avoid Send/Sync issues with HWND.
static COMPOSITOR_HWND: Mutex<*mut std::ffi::c_void> = Mutex::new(ptr::null_mut());

/// Create a fullscreen, transparent, topmost compositor window.
///
/// The window uses `WS_EX_LAYERED | WS_EX_TRANSPARENT` so clicks on empty
/// desktop areas pass through to Windows. It does not activate (no focus steal)
/// and stays topmost so shell UI is always above normal app windows.
pub fn create_fullscreen_window() -> anyhow::Result<HWND> {
    unsafe {
        let hinstance = GetModuleHandleW(None)?;

        let wnd_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: HINSTANCE(hinstance.0),
            hIcon: None,
            hCursor: None,
            hbrBackground: None,
            lpszMenuName: PCWSTR::null(),
            lpszClassName: CLASS_NAME,
            hIconSm: None,
        };

        if RegisterClassExW(&wnd_class) == 0 {
            return Err(anyhow::anyhow!("Failed to register window class"));
        }

        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            CLASS_NAME,
            WINDOW_NAME,
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            std::i32::MAX,
            std::i32::MAX,
            None,
            None,
            Some(HINSTANCE(hinstance.0)),
            None,
        )?;

        if hwnd.is_invalid() {
            return Err(anyhow::anyhow!("Failed to create compositor window"));
        }

        // Mark the window as layered with full opacity; actual per-pixel alpha
        // comes from the Vulkan swapchain alpha channel.
        windows::Win32::Graphics::Gdi::SetLayeredWindowAttributes(
            hwnd,
            0,
            255,
            windows::Win32::Graphics::Gdi::LWA_ALPHA,
        )
        .ok();

        // Store the HWND as a raw pointer to avoid Send/Sync issues
        *COMPOSITOR_HWND.lock() = hwnd.0;

        info!("Compositor window created: {:?}", hwnd);
        Ok(hwnd)
    }
}

/// Return the compositor window handle if it has been created.
pub fn get_compositor_hwnd() -> Option<HWND> {
    let ptr = *COMPOSITOR_HWND.lock();
    if ptr.is_null() { None } else { Some(HWND(ptr)) }
}

/// Window procedure for the compositor window.
///
/// We keep this minimal: the compositor is a transparent overlay. Most
/// messages are passed to `DefWindowProcW`. `WM_DESTROY` posts a quit message.
extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT::default()
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Run the Win32 message loop in a blocking fashion.
///
/// This should be called on a dedicated thread. Returns when `WM_QUIT` is
/// received.
pub fn run_message_loop() {
    unsafe {
        let mut msg = windows::Win32::Foundation::MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Signal the compositor window to close by posting `WM_CLOSE`.
pub fn request_close() {
    if let Some(hwnd) = get_compositor_hwnd() {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Ensure the current thread is ready for Win32 messages.
pub fn attach_thread_input() -> anyhow::Result<()> {
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::AttachThreadInput(
            GetCurrentThreadId(),
            GetCurrentThreadId(),
            true,
        )
        .ok()
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_class_name_is_not_null() {
        assert!(!CLASS_NAME.is_null());
    }
}
