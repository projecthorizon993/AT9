use std::path::PathBuf;
use std::process::Stdio;

use slu_ipc::{
    IPC,
    messages::{RendererToShell, ShellToRenderer},
};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::{
    error::Result,
    platform::{paths, shell},
    utils::was_installed_using_msix,
};

/// Compositor process management.
pub struct CompositorPipe;

impl CompositorPipe {
    /// Path to the compositor binary.
    pub fn compositor_path() -> Result<PathBuf> {
        let compositor_path = if was_installed_using_msix() {
            paths::local_data_dir()?.join("Microsoft\\WindowsApps\\compositor.exe")
        } else {
            std::env::current_exe()?.with_file_name("compositor.exe")
        };
        Ok(compositor_path)
    }

    /// Check if the compositor is running by attempting IPC connection.
    pub fn is_running() -> bool {
        // TODO: Implement proper compositor IPC connection check
        // For now, use a simple named pipe check
        slu_ipc::common::IPC::can_stablish_connection()
    }

    /// Start the compositor process.
    pub async fn start_compositor() -> Result<()> {
        let compositor_path = Self::compositor_path()?;

        log::info!("Starting compositor: {}", compositor_path.display());

        let mut cmd = Command::new(&compositor_path);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let _ = shell::execute(&compositor_path.to_string_lossy(), None, None, false);

        // Wait for compositor to be ready
        let mut counter = 0;
        while !Self::is_running() && counter < 50 {
            log::debug!("Waiting for compositor IPC...");
            counter += 1;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        if counter >= 50 {
            log::error!("Compositor failed to start within timeout");
            return Err("Compositor failed to start".into());
        }

        log::info!("Compositor started successfully");
        Ok(())
    }

    /// Send a message to the compositor.
    pub async fn send(message: ShellToRenderer) -> Result<()> {
        let data = message.to_bytes()?;
        // TODO: Implement actual IPC send
        // This will use the compositor's named pipe
        let _ = data;
        Ok(())
    }

    /// Request the compositor to shut down.
    pub async fn shutdown() -> Result<()> {
        Self::send(ShellToRenderer::Quit).await
    }
}

/// Compositor state management for the backend.
pub struct CompositorManager {
    /// Whether the compositor has been started.
    started: std::sync::atomic::AtomicBool,
}

impl CompositorManager {
    pub fn new() -> Self {
        Self {
            started: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Initialize and start the compositor if not already running.
    pub async fn ensure_started(&self) -> Result<()> {
        if self.started.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }

        if CompositorPipe::is_running() {
            self.started
                .store(true, std::sync::atomic::Ordering::Release);
            return Ok(());
        }

        CompositorPipe::start_compositor().await?;
        self.started
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    /// Send shell state to the compositor.
    pub async fn send_shell_state(
        &self,
        taskbar: serde_json::Value,
        dock: serde_json::Value,
        workspaces: Vec<serde_json::Value>,
        theme: serde_json::Value,
    ) -> Result<()> {
        CompositorPipe::send(ShellToRenderer::ShellState {
            taskbar,
            dock,
            workspaces,
            theme,
        })
        .await
    }

    /// Notify the compositor of an app transition start.
    pub async fn start_app_transition(
        &self,
        app_id: String,
        from_rect: [f32; 4],
        to_rect: [f32; 4],
        open: bool,
    ) -> Result<()> {
        CompositorPipe::send(ShellToRenderer::AppTransitionStart {
            app_id,
            from_rect,
            to_rect,
            open,
        })
        .await
    }

    /// Request the compositor to invalidate (redraw).
    pub async fn invalidate(&self) -> Result<()> {
        CompositorPipe::send(ShellToRenderer::Invalidate).await
    }
}

/// Global compositor manager instance.
static COMPOSITOR_MANAGER: std::sync::OnceLock<CompositorManager> = std::sync::OnceLock::new();

pub fn compositor_manager() -> &'static CompositorManager {
    COMPOSITOR_MANAGER.get_or_init(CompositorManager::new)
}
