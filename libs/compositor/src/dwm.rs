use log::{error, info, warn};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{self, DWMWINDOWATTRIBUTE};

/// DWM system backdrop type values (undocumented but stable on Windows 11).
///
/// See: `DWMSBT_*` constants used by `DwmSetWindowAttribute` with
/// `DWMWA_SYSTEMBACKDROP_TYPE`.
const DWMSBT_MAINWINDOW: u32 = 2;
const DWMSBT_TRANSIENTWINDOW: u32 = 3;
const DWMSBT_TABBEDWINDOW: u32 = 4;

/// DWM window attribute: system backdrop type (Mica / Acrylic).
const DWMWA_SYSTEMBACKDROP_TYPE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(38);

/// DWM window attribute: immersive dark mode.
const DWMWA_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(19);

/// Apply Windows 11 Mica/Acrylic backdrop blur to the given window.
///
/// `use_mica` selects Mica (wallpaper-tinted, subtle) when true, Acrylic
/// (frosted glass, more transparent) when false.
pub fn apply_backdrop_blur(hwnd: HWND, use_mica: bool) -> anyhow::Result<()> {
    unsafe {
        let backdrop_type = if use_mica {
            DWMSBT_MAINWINDOW
        } else {
            DWMSBT_TRANSIENTWINDOW
        };

        let hr = Dwm::DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop_type as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        match hr {
            Ok(()) => {
                info!(
                    "Applied DWM {} blur to compositor window",
                    if use_mica { "Mica" } else { "Acrylic" }
                );
                Ok(())
            }
            Err(e) => {
                warn!(
                    "Failed to apply DWM blur (this is expected on Windows 10): {}",
                    e
                );
                // Blur is a visual enhancement, not a hard requirement.
                Ok(())
            }
        }
    }
}

/// Enable immersive dark mode for the window title bar / frame.
pub fn enable_immersive_dark_mode(hwnd: HWND, enabled: bool) -> anyhow::Result<()> {
    let value: u32 = if enabled { 1 } else { 0 };

    unsafe {
        let hr = Dwm::DwmSetWindowAttribute(
            hwnd,
            DWMWA_IMMERSIVE_DARK_MODE,
            &value as *const u32 as *const _,
            std::mem::size_of::<u32>() as u32,
        );

        if hr.is_err() {
            warn!("Failed to set immersive dark mode: {}", hr.unwrap_err());
        }
    }
    Ok(())
}

/// Check if DWM composition is enabled (needed for transparency to work correctly).
pub fn is_composition_enabled() -> anyhow::Result<bool> {
    unsafe {
        let enabled = Dwm::DwmIsCompositionEnabled()?;
        Ok(enabled.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backdrop_type_constants() {
        assert_eq!(DWMSBT_MAINWINDOW, 2);
        assert_eq!(DWMSBT_TRANSIENTWINDOW, 3);
        assert_eq!(DWMWA_SYSTEMBACKDROP_TYPE.0, 38);
    }
}
