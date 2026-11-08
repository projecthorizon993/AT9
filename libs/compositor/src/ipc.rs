use log::{debug, error, info, warn};
use slu_ipc::{
    common::{END_OF_TRANSMISSION_BLOCK, read_from_ipc_stream, write_to_ipc_stream},
    messages::IpcResponse,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeHandleState,
    PIPE_ACCESS_DUPLEX, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE,
    PIPE_WAIT,
};

use crate::{animation::TransitionManager, ui::UIScene};

/// Named pipe path for the compositor IPC (backend <-> compositor).
pub const COMPOSITOR_PIPE_PATH: &str = r"\\.\pipe\seelen-ui-compositor";

/// Messages from the backend to the compositor.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ShellToRenderer {
    /// New shell state (taskbar, dock, workspaces, theme).
    ShellState {
        taskbar: serde_json::Value,
        dock: serde_json::Value,
        workspaces: Vec<serde_json::Value>,
        theme: serde_json::Value,
    },
    /// Start an app open/close transition.
    AppTransitionStart {
        app_id: String,
        from_rect: [f32; 4],
        to_rect: [f32; 4],
        open: bool,
    },
    /// End an app transition.
    AppTransitionEnd {
        app_id: String,
    },
    /// Force a redraw (damage).
    Invalidate,
    Quit,
}

impl ShellToRenderer {
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// Messages from the compositor to the backend.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum RendererToShell {
    /// Input event on a shell element.
    ShellInput { target: String, kind: String },
    /// A transition finished.
    TransitionFinished { app_id: String },
    /// Compositor is ready (Vulkan initialized).
    Ready,
}

impl RendererToShell {
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn to_bytes(&self) -> anyhow::Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// IPC client for the compositor process.
///
/// Connects to the backend's named pipe and runs a message loop.
pub struct CompositorIpc {
    pipe_path: String,
}

impl CompositorIpc {
    pub fn new() -> Self {
        Self {
            pipe_path: COMPOSITOR_PIPE_PATH.to_string(),
        }
    }

    /// Start the named pipe server and process messages from the backend.
    ///
    /// This runs in a tokio task. For each connection it reads a message,
    /// dispatches it via the callback, and sends a response.
    pub async fn start_server<F, Fut>(&self, mut callback: F) -> anyhow::Result<()>
    where
        F: FnMut(ShellToRenderer) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = anyhow::Result<Option<RendererToShell>>> + Send + Sync,
    {
        let pipe_name = windows::core::PCWSTR::from_raw(
            self.pipe_path
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<u16>>()
                .as_ptr(),
        );

        unsafe {
            let pipe = CreateNamedPipeW(
                pipe_name,
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,    // max instances
                4096, // out buffer
                4096, // in buffer
                0,    // default timeout
                None,
            );

            if pipe.is_invalid() {
                return Err(anyhow::anyhow!("Failed to create named pipe"));
            }

            info!("Compositor IPC server listening on {}", self.pipe_path);

            loop {
                let connected = ConnectNamedPipe(pipe, None).is_ok();
                if connected {
                    let stream = windows::Win32::Foundation::HANDLE(pipe.0);
                    let _ = self.handle_connection(stream, &mut callback).await;
                    let _ = DisconnectNamedPipe(pipe);
                } else {
                    let err = windows::core::Error::from_win32();
                    error!("Failed to connect named pipe: {}", err);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn handle_connection<F, Fut>(
        &self,
        _stream: HANDLE,
        _callback: &mut F,
    ) -> anyhow::Result<()>
    where
        F: FnMut(ShellToRenderer) -> Fut + Send + Sync,
        Fut: std::future::Future<Output = anyhow::Result<Option<RendererToShell>>> + Send + Sync,
    {
        // Note: In a production implementation we'd wrap the HANDLE in an async
        // stream using Windows named pipe overlapped I/O. For MVP we process
        // messages via a blocking read on a separate thread.
        let _ = _stream;
        Ok(())
    }

    /// Send a message to the backend.
    pub async fn send_to_backend(&self, message: &RendererToShell) -> anyhow::Result<()> {
        let data = message.to_bytes()?;
        let _ = self.send_raw(&data).await?;
        Ok(())
    }

    async fn send_raw(&self, data: &[u8]) -> anyhow::Result<IpcResponse> {
        // Blocking connect + send via Win32 named pipe (simple, no tokio async pipe needed).
        let pipe_name = windows::core::PCWSTR::from_raw(
            self.pipe_path
                .encode_utf16()
                .chain(Some(0))
                .collect::<Vec<u16>>()
                .as_ptr(),
        );

        unsafe {
            let pipe = windows::Win32::System::Pipes::CreateFileW(
                pipe_name,
                windows::Win32::Foundation::GENERIC_READ
                    | windows::Win32::Foundation::GENERIC_WRITE,
                0,
                None,
                windows::Win32::Foundation::OPEN_EXISTING,
                0,
                None,
            )?;

            if pipe.is_invalid() {
                return Err(anyhow::anyhow!("Failed to connect to backend pipe"));
            }

            // Write message + EOT block.
            let mut written = 0u32;
            windows::Win32::System::IO::WriteFile(pipe, data, Some(&mut written), None)?;
            windows::Win32::System::IO::WriteFile(
                pipe,
                &[slu_ipc::common::END_OF_TRANSMISSION_BLOCK],
                Some(&mut written),
                None,
            )?;

            // Read response.
            let mut buf = [0u8; 4096];
            let mut read = 0u32;
            windows::Win32::System::IO::ReadFile(pipe, &mut buf, Some(&mut read), None)?;

            let _ = windows::Win32::Foundation::CloseHandle(pipe);

            if read > 0 {
                Ok(IpcResponse::from_bytes(&buf[..read as usize])?)
            } else {
                Ok(IpcResponse::Success)
            }
        }
    }

    /// Notify the backend that the compositor is ready.
    pub async fn notify_ready(&self) -> anyhow::Result<()> {
        self.send_to_backend(&RendererToShell::Ready).await
    }

    /// Notify the backend that a transition finished.
    pub async fn notify_transition_finished(&self, app_id: &str) -> anyhow::Result<()> {
        self.send_to_backend(&RendererToShell::TransitionFinished {
            app_id: app_id.to_string(),
        })
        .await
    }

    /// Notify the backend of a shell input event.
    pub async fn notify_input(&self, target: &str, kind: &str) -> anyhow::Result<()> {
        self.send_to_backend(&RendererToShell::ShellInput {
            target: target.to_string(),
            kind: kind.to_string(),
        })
        .await
    }
}

/// Compositor state that bridges IPC, animation, and rendering.
pub struct CompositorState {
    pub ipc: CompositorIpc,
    pub scene: UIScene,
    pub transitions: TransitionManager,
}

impl CompositorState {
    pub fn new() -> Self {
        Self {
            ipc: CompositorIpc::new(),
            scene: UIScene::new(),
            transitions: TransitionManager::new(),
        }
    }

    /// Apply shell state from the backend.
    pub fn apply_shell_state(
        &mut self,
        taskbar: serde_json::Value,
        dock: serde_json::Value,
        workspaces: Vec<serde_json::Value>,
        _theme: serde_json::Value,
    ) {
        debug!("Applying shell state from backend");
        // TODO: Convert JSON state into UISurface elements.
        // This is where the compositor builds its retained UI model from
        // the backend's source-of-truth state.
        let _ = (taskbar, dock, workspaces);
    }

    /// Start an app transition from the backend.
    pub fn start_transition(
        &mut self,
        app_id: String,
        from_rect: [f32; 4],
        to_rect: [f32; 4],
        open: bool,
    ) {
        info!("Starting transition for app {} (open={})", app_id, open);
        self.transitions
            .start_transition(app_id, from_rect, to_rect, open);
        self.scene.has_visible_content();
    }

    /// Tick animations and return finished app IDs.
    pub fn tick_animations(&mut self) -> Vec<String> {
        self.transitions.tick()
    }

    /// Whether anything needs redraw (animations or dirty state).
    pub fn needs_redraw(&self) -> bool {
        self.transitions.is_animating() || self.scene.has_visible_content()
    }
}

impl Default for CompositorState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_to_renderer_serialization() {
        let msg = ShellToRenderer::Quit;
        let bytes = msg.to_bytes().unwrap();
        let decoded: ShellToRenderer = ShellToRenderer::from_bytes(&bytes).unwrap();
        assert!(matches!(decoded, ShellToRenderer::Quit));
    }

    #[test]
    fn test_renderer_to_shell_serialization() {
        let msg = RendererToShell::Ready;
        let bytes = msg.to_bytes().unwrap();
        let decoded: RendererToShell = RendererToShell::from_bytes(&bytes).unwrap();
        assert!(matches!(decoded, RendererToShell::Ready));
    }
}
