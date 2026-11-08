#![windows_subsystem = "windows"]

use log::{error, info, warn};
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Foundation::HWND;

use crate::{
    animation::{Ease, Transition},
    dwm::{apply_backdrop_blur, enable_immersive_dark_mode},
    ipc::{CompositorState, RendererToShell, ShellToRenderer},
    text::TextRenderer,
    ui::{Color, RectF, UISurface, UiElement},
    vulkan::VulkanContext,
    window::{create_fullscreen_window, get_compositor_hwnd, request_close, run_message_loop},
};

static COMPOSITOR_STATE: Mutex<Option<Arc<parking_lot::Mutex<CompositorState>>>> = Mutex::new(None);

static VULKAN_CTX: Mutex<Option<Arc<parking_lot::Mutex<VulkanContext>>>> = Mutex::new(None);

fn main() -> std::process::ExitCode {
    if let Err(err) = fern::Dispatch::new()
        .format(|out, message, record| {
            out.finish(format!(
                "[{}][{}] {}",
                record.level(),
                record.target(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .chain(std::io::stdout())
        .apply()
    {
        eprintln!("Failed to initialize logger: {}", err);
    }

    info!("Seelen Compositor starting...");

    if let Err(err) = run_compositor() {
        error!("Compositor fatal error: {:?}", err);
        return std::process::ExitCode::from(1);
    }

    info!("Seelen Compositor exited cleanly.");
    std::process::ExitCode::from(0)
}

fn run_compositor() -> anyhow::Result<()> {
    // 1. Create the fullscreen transparent window.
    let hwnd = create_fullscreen_window()?;
    let hwnd_clone = hwnd;

    // 2. Apply DWM blur (Mica).
    if let Err(err) = apply_backdrop_blur(hwnd, true) {
        warn!("DWM blur setup warning: {:?}", err);
    }
    if let Err(err) = enable_immersive_dark_mode(hwnd, true) {
        warn!("Dark mode setup warning: {:?}", err);
    }

    // 3. Get window dimensions.
    let rect = unsafe {
        let mut r = windows::Win32::Foundation::RECT::default();
        windows::Win32::Graphics::Gdi::GetWindowRect(hwnd, &mut r).ok();
        r
    };
    let width = (rect.right - rect.left) as u32;
    let height = (rect.bottom - rect.top) as u32;

    // 4. Initialize Vulkan.
    let vulkan_ctx = unsafe { VulkanContext::new(hwnd, width, height)? };
    let vulkan_ctx = Arc::new(parking_lot::Mutex::new(vulkan_ctx));
    *VULKAN_CTX.lock() = Some(Arc::clone(&vulkan_ctx));

    // 5. Initialize text renderer (DirectWrite).
    let _text_renderer = TextRenderer::new();

    // 6. Initialize compositor state.
    let state = Arc::new(parking_lot::Mutex::new(CompositorState::new()));
    *COMPOSITOR_STATE.lock() = Some(Arc::clone(&state));

    // 7. Start Win32 message loop on a background thread.
    let msg_thread = std::thread::spawn(move || {
        run_message_loop();
    });

    // 8. Run the render loop + IPC on the main thread.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        // Notify backend we're ready.
        {
            let state_guard = state.lock();
            if let Err(err) = state_guard.ipc.notify_ready().await {
                warn!("Failed to notify backend of readiness: {:?}", err);
            }
        }

        let mut last_frame = Instant::now();
        let mut frame_count = 0u64;

        loop {
            // Tick animations.
            let finished_apps = {
                let mut state_guard = state.lock();
                let finished = state_guard.tick_animations();
                for app_id in &finished {
                    let app_id_clone = app_id.clone();
                    // Drop guard before async send.
                    drop(state_guard);
                    // Notify backend transition finished.
                    if let Err(err) = state
                        .lock()
                        .unwrap()
                        .ipc
                        .notify_transition_finished(app_id_clone)
                        .await
                    {
                        warn!("Failed to notify transition finished: {:?}", err);
                    }
                    // Re-acquire (the lock was dropped above).
                }
            };

            // Determine if we need to render this frame.
            let needs_redraw = {
                let state_guard = state.lock();
                state_guard.needs_redraw()
            };

            let now = Instant::now();
            let delta = now - last_frame;
            let target_frame_time = std::time::Duration::from_millis(16); // ~60fps cap when animating

            if needs_redraw || delta >= target_frame_time {
                if let Err(err) = render_frame(&vulkan_ctx, &state) {
                    error!("Render error: {:?}", err);
                }
                last_frame = now;
                frame_count += 1;
            } else {
                // Sleep briefly to avoid busy-waiting when idle.
                tokio::time::sleep(std::time::Duration::from_millis(4)).await;
            }

            // Check for quit signal.
            if get_compositor_hwnd().is_none() {
                info!("Compositor window was destroyed, exiting.");
                break;
            }
        }
    });

    // Wait for message loop thread.
    let _ = msg_thread.join();

    Ok(())
}

/// Render a single frame.
fn render_frame(
    vulkan_ctx: &Arc<parking_lot::Mutex<VulkanContext>>,
    state: &Arc<parking_lot::Mutex<CompositorState>>,
) -> anyhow::Result<()> {
    let mut vk = vulkan_ctx.lock();

    // Skip if not dirty and nothing animating (already handled by caller,
    // but double-check here).
    if !vk.is_dirty() && {
        let s = state.lock();
        !s.needs_redraw()
    } {
        return Ok(());
    }

    unsafe {
        let image_index = vk.begin_frame()?;

        // Clear to fully transparent (compositor is an overlay).
        let clear_color = ash::vk::ClearColorValue {
            float32: [0.0, 0.0, 0.0, 0.0],
        };

        // Build UI draw commands from the scene + transitions.
        let (draw_commands, needs_present) = {
            let s = state.lock();
            let mut commands = Vec::new();

            // Draw UI surfaces.
            for surface in &s.scene.surfaces {
                if !surface.visible {
                    continue;
                }
                for element in &surface.elements {
                    commands.push(format!("{:?}", element));
                }
            }

            // Draw active transitions.
            for transition in s.transitions.active_transitions() {
                let rect = transition.current_rect();
                let opacity = transition.current_opacity();
                let radius = transition.current_corner_radius();
                commands.push(format!(
                    "Transition {}: rect=[{:.0},{:.0},{:.0},{:.0}] opacity={:.2} radius={:.1}",
                    transition.app_id, rect[0], rect[1], rect[2], rect[3], opacity, radius
                ));
                // Draw transition quad.
                commands.push(
                    UiElement::rounded_rect(
                        RectF::new(rect[0], rect[1], rect[2], rect[3]),
                        Color::rgba(200, 200, 255, (opacity * 255.0) as u8),
                        radius,
                    )
                    .to_string(),
                );
            }

            (
                commands,
                s.transitions.is_animating() || s.scene.has_visible_content(),
            )
        };

        // TODO: Actually record draw commands into the Vulkan command buffer
        // using pipelines for rects, text, and images. For MVP we clear only.
        let _ = draw_commands;

        vk.draw_frame(image_index, clear_color)?;

        if !needs_present {
            // Mark dirty so next animation frame re-records.
            vk.mark_dirty();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transition_element_creation() {
        let elem = UiElement::rounded_rect(
            RectF::new(0.0, 0.0, 100.0, 50.0),
            Color::rgba(255, 0, 0, 128),
            8.0,
        );
        match elem {
            UiElement::Rect {
                color,
                corner_radius,
                ..
            } => {
                assert_eq!(color.a, 128);
                assert_eq!(corner_radius, 8.0);
            }
            _ => panic!("Expected rounded rect"),
        }
    }

    #[test]
    fn test_ease_enum_serialization() {
        let ease = Ease::EaseOutQuad;
        let json = serde_json::to_string(&ease).unwrap();
        let decoded: Ease = serde_json::from_str(&json).unwrap();
        assert_eq!(ease, decoded);
    }
}
