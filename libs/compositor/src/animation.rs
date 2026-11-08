use std::time::Instant;

/// Easing curves used by the compositor's animation engine.
///
/// The compositor owns animation timing entirely; the backend only sends
/// high-level intents (open/close app, duration hints).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Ease {
    /// Constant velocity.
    Linear,
    /// Slow start, accelerating.
    EaseInQuad,
    /// Fast start, smooth deceleration — recommended for app open.
    EaseOutQuad,
    /// Slow start and end, fast middle.
    EaseInOutQuad,
    /// Damped spring oscillation.
    Spring { stiffness: f32, damping: f32 },
}

impl Default for Ease {
    fn default() -> Self {
        Ease::EaseOutQuad
    }
}

impl Ease {
    /// Evaluate the easing function at normalized time `t` in [0, 1].
    pub fn evaluate(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Ease::Linear => t,
            Ease::EaseInQuad => t * t,
            Ease::EaseOutQuad => 1.0 - (1.0 - t) * (1.0 - t),
            Ease::EaseInOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    let u = t - 0.5;
                    0.5 + 2.0 * u * u + 2.0 * u
                }
            }
            Ease::Spring { stiffness, damping } => {
                evaluate_spring(t, stiffness, damping)
            }
        }
    }
}

/// Interpolate between `start` and `end` using the given easing curve.
pub fn lerp_eased(start: f32, end: f32, ease: Ease, t: f32) -> f32 {
    let eased = ease.evaluate(t);
    start + (end - start) * eased
}

/// A single animatable property over time.
#[derive(Debug, Clone)]
pub struct AnimProp {
    pub start: f32,
    pub end: f32,
    pub duration_ms: f32,
    pub ease: Ease,
    pub started_at: Instant,
}

impl AnimProp {
    pub fn new(start: f32, end: f32, duration_ms: f32, ease: Ease) -> Self {
        Self {
            start,
            end,
            duration_ms,
            ease,
            started_at: Instant::now(),
        }
    }

    /// Current interpolated value in [start, end].
    pub fn value(&self) -> f32 {
        let elapsed_ms = self.started_at.elapsed().as_secs_f32() * 1000.0;
        let t = (elapsed_ms / self.duration_ms).clamp(0.0, 1.0);
        lerp_eased(self.start, self.end, self.ease, t)
    }

    /// Whether the animation has completed.
    pub fn finished(&self) -> bool {
        self.started_at.elapsed().as_secs_f32() * 1000.0 >= self.duration_ms
    }

    /// Remaining time in milliseconds.
    pub fn remaining_ms(&self) -> f32 {
        let elapsed_ms = self.started_at.elapsed().as_secs_f32() * 1000.0;
        (self.duration_ms - elapsed_ms).max(0.0)
    }
}

/// An app open/close transition managed by the compositor.
///
/// The compositor interpolates from the source rect (e.g. dock icon) to the
/// target rect (the app window), rendering an overlay quad during the
/// transition. When finished, it notifies the backend via IPC.
#[derive(Debug, Clone)]
pub struct Transition {
    pub app_id: String,
    pub from_rect: [f32; 4], // x, y, w, h
    pub to_rect: [f32; 4],   // x, y, w, h
    pub opacity: AnimProp,
    pub scale: AnimProp,
    pub corner_radius: AnimProp,
    pub finished: bool,
    pub open: bool,
}

impl Transition {
    pub fn new(app_id: String, from_rect: [f32; 4], to_rect: [f32; 4], open: bool) -> Self {
        let (open_duration, close_duration) = (250.0, 200.0);
        let duration = if open { open_duration } else { close_duration };

        // For opening: start small at the icon, scale up to the window.
        // For closing: start at the window, scale down to the icon.
        let (start_scale, end_scale) = if open {
            (0.2, 1.0)
        } else {
            (1.0, 0.2)
        };

        let (start_opacity, end_opacity) = if open { (0.0, 1.0) } else { (1.0, 0.0) };

        let (start_radius, end_radius) = if open {
            (8.0, 4.0)
        } else {
            (4.0, 8.0)
        };

        Self {
            app_id,
            from_rect,
            to_rect,
            opacity: AnimProp::new(
                start_opacity,
                end_opacity,
                duration,
                Ease::EaseOutQuad,
            ),
            scale: AnimProp::new(start_scale, end_scale, duration, Ease::EaseOutQuad),
            corner_radius: AnimProp::new(
                start_radius,
                end_radius,
                duration,
                Ease::EaseOutQuad,
            ),
            finished: false,
            open,
        }
    }

    /// Advance the transition by one frame. Returns `true` when finished.
    pub fn tick(&mut self) -> bool {
        if self.opacity.finished() && self.scale.finished() {
            self.finished = true;
            return true;
        }
        false
    }

    /// Current interpolated rectangle for the transition overlay.
    pub fn current_rect(&self) -> [f32; 4] {
        let t = self.opacity.value(); // all props share the same clock
        let x = lerp_eased(
            self.from_rect[0],
            self.to_rect[0],
            Ease::EaseOutQuad,
            t,
        );
        let y = lerp_eased(
            self.from_rect[1],
            self.to_rect[1],
            Ease::EaseOutQuad,
            t,
        );
        let w = lerp_eased(
            self.from_rect[2],
            self.to_rect[2],
            Ease::EaseOutQuad,
            t,
        );
        let h = lerp_eased(
            self.from_rect[3],
            self.to_rect[3],
            Ease::EaseOutQuad,
            t,
        );
        [x, y, w, h]
    }

    /// Current opacity value (0.0 - 1.0).
    pub fn current_opacity(&self) -> f32 {
        self.opacity.value()
    }

    /// Current scale value.
    pub fn current_scale(&self) -> f32 {
        self.scale.value()
    }

    /// Current corner radius.
    pub fn current_corner_radius(&self) -> f32 {
        self.corner_radius.value()
    }
}

/// Manages all active transitions in the compositor.
#[derive(Debug, Default)]
pub struct TransitionManager {
    pub transitions: Vec<Transition>,
}

impl TransitionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a new app open/close transition.
    pub fn start_transition(
        &mut self,
        app_id: String,
        from_rect: [f32; 4],
        to_rect: [f32; 4],
        open: bool,
    ) {
        // Remove any existing transition for the same app (avoid duplicates).
        self.transitions.retain(|t| t.app_id != app_id);
        self.transitions.push(Transition::new(app_id, from_rect, to_rect, open));
    }

    /// Tick all transitions. Returns a list of app IDs whose transitions
    /// just finished this frame.
    pub fn tick(&mut self) -> Vec<String> {
        let mut finished = Vec::new();
        let mut i = 0;
        while i < self.transitions.len() {
            if self.transitions[i].tick() {
                finished.push(self.transitions[i].app_id.clone());
                self.transitions.remove(i);
            } else {
                i += 1;
            }
        }
        finished
    }

    /// Whether any transition is currently active (used for damage tracking).
    pub fn is_animating(&self) -> bool {
        !self.transitions.is_empty()
    }

    /// Get all active transitions for rendering.
    pub fn active_transitions(&self) -> &[Transition] {
        &self.transitions
    }
}

/// Evaluate a damped spring function at time `t`.
fn evaluate_spring(t: f32, stiffness: f32, damping: f32) -> f32 {
    if stiffness <= 0.0 {
        return t;
    }
    let omega = stiffness.sqrt();
    let z = damping / (2.0 * omega);
    if z < 1.0 {
        // Underdamped: oscillating
        let wd = omega * (1.0 - z * z).sqrt();
        let a = 1.0;
        let b = z / (1.0 - z * z).sqrt();
        1.0 - (-damping * t).exp() * (wd * t).cos() / (wd * b)
    } else {
        // Critically damped or overdamped: fall back to ease-out
        1.0 - (1.0 - t) * (1.0 - t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ease_out_quad_bounds() {
        assert!((Ease::EaseOutQuad.evaluate(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((Ease::EaseOutQuad.evaluate(1.0) - 1.0).abs() < f32::EPSILON);
        // EaseOutQuad should be > Linear in the middle
        assert!(Ease::EaseOutQuad.evaluate(0.5) > Ease::Linear.evaluate(0.5));
    }

    #[test]
    fn test_anim_prop_finished() {
        let mut prop = AnimProp::new(0.0, 100.0, 100.0, Ease::Linear);
        assert!(!prop.finished());
        // value starts at start
        assert!((prop.value() - 0.0).abs() < 1.0);
    }

    #[test]
    fn test_transition_tick() {
        let mut mgr = TransitionManager::new();
        mgr.start_transition("test".to_string(), [0.0, 0.0, 100.0, 100.0], [200.0, 200.0, 300.0, 300.0], true);
        assert_eq!(mgr.active_transitions().len(), 1);
        assert!(!mgr.active_transitions()[0].finished);
    }
}
