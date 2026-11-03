//! Shared application state: the active device session, configuration and the
//! bridge to the GTK UI.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::android::{Adb, LogLevel, LogSink, Session, SessionOptions};
use crate::config::Config;
use crate::mcp::McpHandle;

#[derive(Debug, Clone)]
pub enum UiEvent {
    Log(String),
    SessionChanged,
    McpChanged,
    ToolCall {
        name: String,
        ok: bool,
        detail: String,
    },
    Stats {
        battery: Option<u32>,
        charging: bool,
        app: Option<String>,
    },
}

pub struct AppState {
    pub cfg: Mutex<Config>,
    pub adb: Adb,
    pub ui_tx: Mutex<Option<mpsc::Sender<UiEvent>>>,
    pub mcp: Mutex<Option<McpHandle>>,
    pub manual_input: AtomicBool,
    session: RwLock<Option<Arc<Session>>>,
}

impl AppState {
    pub fn new(cfg: Config) -> Arc<Self> {
        let adb = Adb::new(cfg.adb_path.clone());
        let manual_input = AtomicBool::new(cfg.manual_input);
        Arc::new(Self {
            cfg: Mutex::new(cfg),
            adb,
            ui_tx: Mutex::new(None),
            mcp: Mutex::new(None),
            manual_input,
            session: RwLock::new(None),
        })
    }

    pub fn set_ui(&self, tx: Option<mpsc::Sender<UiEvent>>) {
        *self.ui_tx.lock().unwrap() = tx;
    }

    pub fn emit(&self, event: UiEvent) {
        if let Some(tx) = self.ui_tx.lock().unwrap().as_ref() {
            let _ = tx.send(event);
        }
    }

    pub fn log(&self, message: impl Into<String>) {
        let message = message.into();
        log::info!("{message}");
        self.emit(UiEvent::Log(message));
    }

    pub fn log_sink(&self) -> LogSink {
        let tx = self.ui_tx.lock().unwrap().clone();
        crate::android::session::log_sink(move |level, message| {
            match level {
                LogLevel::Debug => log::debug!("{message}"),
                LogLevel::Info => log::info!("{message}"),
                LogLevel::Warn => log::warn!("{message}"),
                LogLevel::Error => log::error!("{message}"),
            }
            if let Some(tx) = &tx {
                let level = match level {
                    LogLevel::Debug => "debug",
                    LogLevel::Info => "info",
                    LogLevel::Warn => "warn",
                    LogLevel::Error => "error",
                };
                let _ = tx.send(UiEvent::Log(format!("[{level}] {message}")));
            }
        })
    }

    pub fn session(&self) -> Option<Arc<Session>> {
        self.session.read().unwrap().clone()
    }

    pub fn set_session(&self, session: Option<Arc<Session>>) {
        *self.session.write().unwrap() = session;
        self.emit(UiEvent::SessionChanged);
    }

    pub fn open_session(&self, serial: &str) -> Result<Arc<Session>> {
        if self.session().is_some() {
            anyhow::bail!("a session is already running; stop it first");
        }
        let cfg = self.cfg.lock().unwrap().clone();
        let options = SessionOptions {
            max_fps: cfg.max_fps,
            max_size: cfg.max_size,
            video_bit_rate: cfg.video_bit_rate,
            stay_awake: cfg.stay_awake,
        };
        let session = Session::start(serial.to_string(), &self.adb, options, self.log_sink())?;
        *self.session.write().unwrap() = Some(session.clone());
        self.emit(UiEvent::SessionChanged);
        Ok(session)
    }

    pub fn close_session(&self) {
        let session = self.session.write().unwrap().take();
        if let Some(session) = session {
            session.stop();
        }
        self.emit(UiEvent::SessionChanged);
    }

    pub fn status_line(&self) -> String {
        match self.session() {
            None => "未接続".to_string(),
            Some(session) => {
                let status = session.status();
                let dims = status
                    .dims
                    .map(|dims| format!("{}x{}", dims.width, dims.height))
                    .unwrap_or_else(|| "size?".to_string());
                let age = status
                    .frame_age_ms
                    .map(|age| format!("{age}ms"))
                    .unwrap_or_else(|| "-".to_string());
                format!(
                    "{} | {} | {} | frame {} | {}s",
                    status.device_name, status.state, dims, age, status.uptime_s
                )
            }
        }
    }

    pub fn mcp_url(&self) -> Option<String> {
        self.mcp.lock().unwrap().as_ref().map(|mcp| mcp.url.clone())
    }

    pub fn start_mcp(self: &Arc<Self>) -> Result<String> {
        let cfg = self.cfg.lock().unwrap().clone();
        let addr: SocketAddr = format!("{}:{}", cfg.mcp_bind, cfg.mcp_port)
            .parse()
            .with_context(|| format!("invalid MCP address {}:{}", cfg.mcp_bind, cfg.mcp_port))?;
        let std_listener = std::net::TcpListener::bind(addr)
            .with_context(|| format!("bind MCP server on {addr}"))?;
        std_listener.set_nonblocking(true)?;
        let url = format!("http://{addr}/mcp");

        let state = Arc::clone(self);
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let runtime = crate::mcp::runtime();
        let join = runtime.spawn(async move {
            match tokio::net::TcpListener::from_std(std_listener) {
                Ok(listener) => {
                    if let Err(err) = crate::mcp::serve(listener, state, shutdown_rx).await {
                        log::error!("MCP server stopped: {err:#}");
                    }
                }
                Err(err) => log::error!("MCP listener: {err}"),
            }
        });

        *self.mcp.lock().unwrap() = Some(McpHandle {
            url: url.clone(),
            shutdown: Some(shutdown_tx),
            join: Some(join),
        });
        self.emit(UiEvent::McpChanged);
        Ok(url)
    }

    pub fn stop_mcp(&self) {
        let handle = self.mcp.lock().unwrap().take();
        if let Some(mut handle) = handle {
            if let Some(shutdown) = handle.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(join) = handle.join.take() {
                let _ = crate::mcp::runtime().block_on(join);
            }
        }
        self.emit(UiEvent::McpChanged);
    }

    /// Reads battery and foreground app in the background and emits a Stats
    /// event. Safe to call repeatedly (the UI polls it every few seconds).
    pub fn refresh_stats(self: &Arc<Self>) {
        let Some(session) = self.session() else {
            return;
        };
        let serial = session.serial.clone();
        let adb = self.adb.clone();
        let state = Arc::clone(self);
        std::thread::spawn(move || {
            let battery_out = adb
                .shell_stdout(&serial, "dumpsys battery", Duration::from_secs(3))
                .unwrap_or_default();
            let level = battery_out.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("level:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            });
            let status = battery_out.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("status:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            });
            let charging = matches!(status, Some(2) | Some(5));

            let app_out = adb
                .shell_stdout(
                    &serial,
                    "dumpsys activity activities | grep -m1 -E 'topResumedActivity|mResumedActivity'",
                    Duration::from_secs(3),
                )
                .unwrap_or_default();
            let app = app_out
                .split_whitespace()
                .find(|token| token.contains('/'))
                .and_then(|token| token.split('/').next())
                .map(str::to_string);

            state.emit(UiEvent::Stats {
                battery: level,
                charging,
                app,
            });
        });
    }

    pub fn notify_tool_call(&self, name: &str, ok: bool, detail: &str) {
        self.emit(UiEvent::ToolCall {
            name: name.to_string(),
            ok,
            detail: detail.to_string(),
        });
    }

    pub fn manual_input_enabled(&self) -> bool {
        self.manual_input.load(Ordering::Relaxed)
    }
}
