use log::debug;
use windows::Win32::Foundation::{POINT, RECT};

/// A rectangle with floating-point precision, used for UI layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }

    pub fn to_array(&self) -> [f32; 4] {
        [self.x, self.y, self.w, self.h]
    }
}

/// A color in RGBA8 format.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn black() -> Self {
        Self::rgba(0, 0, 0, 255)
    }

    pub const fn white() -> Self {
        Self::rgba(255, 255, 255, 255)
    }

    pub const fn transparent() -> Self {
        Self::rgba(0, 0, 0, 0)
    }

    pub fn to_f32(&self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }
}

/// A UI element that can be drawn by the compositor.
#[derive(Debug, Clone)]
pub enum UiElement {
    Rect {
        rect: RectF,
        color: Color,
        corner_radius: f32,
    },
    Border {
        rect: RectF,
        color: Color,
        thickness: f32,
        corner_radius: f32,
    },
    Text {
        rect: RectF,
        text: String,
        color: Color,
        font_size: f32,
    },
    Image {
        rect: RectF,
        texture_id: u32,
        color: Color,
    },
}

impl UiElement {
    pub fn rect(rect: RectF, color: Color) -> Self {
        Self::Rect {
            rect,
            color,
            corner_radius: 0.0,
        }
    }

    pub fn rounded_rect(rect: RectF, color: Color, corner_radius: f32) -> Self {
        Self::Rect {
            rect,
            color,
            corner_radius,
        }
    }

    pub fn bounds(&self) -> RectF {
        match self {
            UiElement::Rect { rect, .. }
            | UiElement::Border { rect, .. }
            | UiElement::Text { rect, .. }
            | UiElement::Image { rect, .. } => *rect,
        }
    }
}

/// A retained UI surface: a collection of elements that form a shell UI
/// piece (taskbar, dock, flyout, etc.).
#[derive(Debug, Default, Clone)]
pub struct UISurface {
    pub id: String,
    pub elements: Vec<UiElement>,
    pub visible: bool,
    pub z_order: i32,
}

impl UISurface {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            elements: Vec::new(),
            visible: true,
            z_order: 0,
        }
    }

    pub fn add(&mut self, element: UiElement) {
        self.elements.push(element);
    }

    /// Hit-test a point against this surface's elements, returning the
    /// first element that contains the point (for input forwarding).
    pub fn hit_test(&self, x: f32, y: f32) -> Option<usize> {
        if !self.visible {
            return None;
        }
        for (i, element) in self.elements.iter().enumerate() {
            if element.bounds().contains(x, y) {
                return Some(i);
            }
        }
        None
    }
}

/// The UI scene: all surfaces currently being composited.
#[derive(Debug, Default)]
pub struct UIScene {
    pub surfaces: Vec<UISurface>,
}

impl UIScene {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_surface(&mut self, surface: UISurface) {
        self.surfaces.push(surface);
    }

    pub fn remove_surface(&mut self, id: &str) {
        self.surfaces.retain(|s| s.id != id);
    }

    pub fn get_surface_mut(&mut self, id: &str) -> Option<&mut UISurface> {
        self.surfaces.iter_mut().find(|s| s.id == id)
    }

    /// Hit-test across all surfaces (topmost first), returning the element index.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<(String, usize)> {
        // Surfaces are stored bottom-to-top; iterate in reverse for topmost first.
        for surface in self.surfaces.iter().rev() {
            if let Some(elem_idx) = surface.hit_test(x, y) {
                return Some((surface.id.clone(), elem_idx));
            }
        }
        None
    }

    /// Whether any surface is visible (used for damage tracking).
    pub fn has_visible_content(&self) -> bool {
        self.surfaces
            .iter()
            .any(|s| s.visible && !s.elements.is_empty())
    }
}

/// Convert a Win32 POINT to f32 coordinates.
pub fn point_to_f32(pt: POINT) -> (f32, f32) {
    (pt.x as f32, pt.y as f32)
}

/// Convert a Win32 RECT to RectF.
pub fn rect_to_f32(rect: RECT) -> RectF {
    RectF::new(
        rect.left as f32,
        rect.top as f32,
        (rect.right - rect.left) as f32,
        (rect.bottom - rect.top) as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rect_contains() {
        let rect = RectF::new(10.0, 20.0, 100.0, 50.0);
        assert!(rect.contains(15.0, 25.0));
        assert!(!rect.contains(5.0, 25.0));
        assert!(!rect.contains(15.0, 5.0));
        assert!(!rect.contains(115.0, 25.0));
        assert!(!rect.contains(15.0, 75.0));
    }

    #[test]
    fn test_ui_surface_hit_test() {
        let mut surface = UISurface::new("test");
        surface.add(UiElement::rect(
            RectF::new(0.0, 0.0, 50.0, 50.0),
            Color::black(),
        ));
        assert!(surface.hit_test(10.0, 10.0).is_some());
        assert!(surface.hit_test(100.0, 100.0).is_none());
    }

    #[test]
    fn test_scene_hit_test_topmost_first() {
        let mut scene = UIScene::new();
        let mut bottom = UISurface::new("bottom");
        bottom.add(UiElement::rect(
            RectF::new(0.0, 0.0, 100.0, 100.0),
            Color::black(),
        ));
        let mut top = UISurface::new("top");
        top.add(UiElement::rect(
            RectF::new(0.0, 0.0, 100.0, 100.0),
            Color::white(),
        ));
        scene.add_surface(bottom);
        scene.add_surface(top);
        // Top surface should win the hit test.
        let result = scene.hit_test(10.0, 10.0);
        assert!(result.is_some());
        assert_eq!(result.unwrap().0, "top");
    }
}
