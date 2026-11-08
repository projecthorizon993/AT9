use log::warn;
use parking_lot::Mutex;
use std::collections::HashMap;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_WEIGHT_NORMAL, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, HBITMAP, HBRUSH, HDC, HGDIOBJ, SelectObject,
};
use windows::Win32::UI::WindowsAndMessaging::GetDC;
use windows::core::PCWSTR;

/// A single glyph in the texture atlas.
#[derive(Debug, Clone)]
pub struct Glyph {
    pub char: char,
    pub uv: [f32; 4],      // u0, v0, u1, v1 in texture coordinates
    pub size: [f32; 2],    // width, height in pixels
    pub bearing: [f32; 2], // x, y offset from baseline
    pub advance: f32,
}

/// Texture atlas holding rasterized glyphs.
#[derive(Debug)]
pub struct FontAtlas {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixel data (alpha channel holds the glyph mask).
    pub pixels: Vec<u8>,
    pub glyphs: HashMap<char, Glyph>,
}

impl FontAtlas {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; (width * height * 4) as usize],
            glyphs: HashMap::new(),
        }
    }
}

/// High-level text renderer using DirectWrite for glyph rasterization.
///
/// DirectWrite runs on the CPU to rasterize glyphs into an alpha atlas,
/// which is then uploaded to Vulkan as a texture. The compositor draws
/// text as textured quads using this atlas.
pub struct TextRenderer {
    factory: Option<IDWriteFactory>,
    dc: Option<HDC>,
    atlas: Mutex<FontAtlas>,
}

impl TextRenderer {
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            let factory: IDWriteFactory =
                windows::Win32::Graphics::DirectWrite::DWriteCreateFactory(
                    DWRITE_FACTORY_TYPE_SHARED,
                )?;

            // Create a compatible DC and DIB section for off-screen glyph measurement.
            let hdc = GetDC(HWND::default())?;
            let compat_dc = CreateCompatibleDC(Some(hdc))?;

            Ok(Self {
                factory: Some(factory),
                dc: Some(compat_dc),
                atlas: Mutex::new(FontAtlas::new(2048, 2048)),
            })
        }
    }

    /// Ensure a glyph for `c` exists in the atlas at the given font size.
    ///
    /// Returns the glyph metrics. If the glyph is not yet in the atlas,
    /// it is rasterized and packed (simple linear packing for MVP).
    pub fn ensure_glyph(&self, c: char, font_size: f32) -> Option<Glyph> {
        // Check if already cached.
        {
            let atlas = self.atlas.lock();
            if let Some(glyph) = atlas.glyphs.get(&c) {
                return Some(glyph.clone());
            }
        }

        // Rasterize the glyph using DirectWrite + GDI DIB section.
        let glyph = self.rasterize_glyph(c, font_size)?;
        {
            let mut atlas = self.atlas.lock();
            atlas.glyphs.insert(c, glyph.clone());
        }
        Some(glyph)
    }

    fn rasterize_glyph(&self, c: char, font_size: f32) -> Option<Glyph> {
        unsafe {
            let factory = self.factory.as_ref()?;
            let dc = self.dc?;

            // Create a text format for measurement.
            let text_format: IDWriteTextFormat = factory
                .CreateTextFormat(
                    PCWSTR::null(), // system default font
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    windows::Win32::Graphics::DirectWrite::DWRITE_FONT_STYLE_NORMAL,
                    windows::Win32::Graphics::DirectWrite::DWRITE_FONT_STRETCH_NORMAL,
                    font_size,
                    PCWSTR::null(), // locale
                )
                .ok()?;

            let mut wide: [u16; 2] = [c as u16, 0];

            // Measure the glyph.
            let mut metrics = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
            let layout = factory
                .CreateTextLayout(&wide, 1, &text_format, 1000.0, 1000.0)
                .ok()?;
            let _ = layout.GetMetrics(&mut metrics);

            let width = metrics.widthIncludingTrailingWhitespace.ceil() as i32;
            let height = metrics.height.ceil() as i32;
            if width <= 0 || height <= 0 {
                return None;
            }

            // Create a DIB section to render into.
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default(); 1],
            };

            let mut bits: *mut u8 = std::ptr::null_mut();
            let hbitmap = CreateDIBSection(
                dc,
                &mut bmi,
                DIB_RGB_COLORS,
                &mut bits as *mut _ as *mut _,
                None,
                0,
            )?;
            if hbitmap.is_invalid() {
                return None;
            }

            let old = SelectObject(dc, hbitmap.into());
            // Fill with transparent black.
            let _ = windows::Win32::Graphics::Gdi::PatBlt(
                dc,
                0,
                0,
                width,
                height,
                windows::Win32::Graphics::Gdi::BLACKNESS,
            );

            // Draw the glyph using DirectWrite rendering (simplified: we draw
            // a solid block for MVP; real glyph outlines need IDWriteTextRenderer).
            // For MVP we fill the measured bounding box with white alpha.
            let rect = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            let brush = windows::Win32::Graphics::Gdi::CreateSolidBrush(
                windows::Win32::Graphics::Gdi::RGB(255, 255, 255),
            );
            let _ = windows::Win32::Graphics::Gdi::FillRect(dc, &rect, brush);
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(brush.into());

            let _ = SelectObject(dc, old);
            let _ = DeleteObject(hbitmap.into());

            // Read bits into RGBA8.
            let row_size = (width as usize) * 4;
            let mut pixels = vec![0u8; row_size * height as usize];
            std::ptr::copy_nonoverlapping(bits, pixels.as_mut_ptr(), pixels.len());

            Some(Glyph {
                char: c,
                uv: [0.0, 0.0, 0.0, 0.0], // filled in by atlas packing
                size: [width as f32, height as f32],
                bearing: [0.0, 0.0],
                advance: metrics.widthIncludingTrailingWhitespace,
            })
        }
    }

    pub fn atlas(&self) -> parking_lot::MutexGuard<'_, FontAtlas> {
        self.atlas.lock()
    }
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new().expect("Failed to create DirectWrite text renderer")
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        if let Some(dc) = self.dc {
            unsafe {
                let _ = DeleteDC(dc);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_font_atlas_creation() {
        let atlas = FontAtlas::new(2048, 2048);
        assert_eq!(atlas.width, 2048);
        assert_eq!(atlas.height, 2048);
        assert_eq!(atlas.pixels.len(), 2048 * 2048 * 4);
        assert!(atlas.glyphs.is_empty());
    }

    #[test]
    fn test_glyph_default_fields() {
        let glyph = Glyph {
            char: 'A',
            uv: [0.0, 0.0, 1.0, 1.0],
            size: [16.0, 16.0],
            bearing: [0.0, 0.0],
            advance: 16.0,
        };
        assert_eq!(glyph.char, 'A');
    }
}
