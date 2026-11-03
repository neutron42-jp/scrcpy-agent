//! MCP tool definitions and dispatch.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::android::{ActionReport, Session, TextMode};
use crate::app::AppState;

const NO_SESSION: &str = "no Android session is active: press 接続 in the scrcpy-agent window, or call start_session with a device serial";

pub struct ToolOutput {
    pub content: Vec<Value>,
    pub structured: Option<Value>,
    pub is_error: bool,
    detail: String,
}

impl ToolOutput {
    pub fn ok_json(value: Value) -> Self {
        let detail = value.to_string();
        let text = json!({"type": "text", "text": pretty(&value)});
        Self {
            content: vec![text],
            structured: Some(value),
            is_error: false,
            detail,
        }
    }

    pub fn image(png: Vec<u8>, meta: Value) -> Self {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&png);
        let detail = format!("screenshot {}x{}", meta["width"], meta["height"]);
        let content = vec![
            json!({"type": "image", "data": encoded, "mimeType": "image/png"}),
            json!({"type": "text", "text": pretty(&meta)}),
        ];
        Self {
            content,
            structured: Some(meta),
            is_error: false,
            detail,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        let message = message.into();
        let content = vec![json!({"type": "text", "text": message})];
        Self {
            content,
            structured: None,
            is_error: true,
            detail: message,
        }
    }

    pub fn summary(&self) -> String {
        if self.is_error {
            format!("error: {}", self.detail)
        } else {
            self.detail.clone()
        }
    }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn schema(properties: Value, required: Value) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": input_schema})
}

pub fn definitions() -> Value {
    Value::Array(vec![
        tool(
            "list_devices",
            "List Android devices attached to adb with their state and model. Unauthorized devices need to accept the USB debugging prompt.",
            schema(json!({}), json!([])),
        ),
        tool(
            "session_status",
            "Report the active device session: serial, video size, stream state, frame age and the MCP endpoint.",
            schema(json!({}), json!([])),
        ),
        tool(
            "start_session",
            "Start mirroring and control for a device. Usually the window's 接続 button does this, but an agent may call it directly when the app runs headless.",
            schema(
                json!({
                    "serial": {"type": "string", "description": "adb serial; defaults to the only attached device"},
                    "max_fps": {"type": "integer", "minimum": 0, "description": "Cap frames per second (0 = device default)"},
                    "max_size": {"type": "integer", "minimum": 0, "description": "Cap the longest video edge in pixels (0 = native)"}
                }),
                json!([]),
            ),
        ),
        tool(
            "stop_session",
            "Stop the active session, release the adb tunnel and kill scrcpy-server on the device.",
            schema(json!({}), json!([])),
        ),
        tool(
            "screenshot",
            "Return the current device screen as a PNG image plus its pixel dimensions. Coordinates for every input tool use this image's pixel space.",
            schema(
                json!({
                    "wait_ms": {"type": "integer", "minimum": 0, "description": "Wait up to this long for the first frame (default 500)"},
                    "source": {"type": "string", "enum": ["video", "adb"], "description": "video (fast, default) or adb screencap (works even when the stream is idle)"}
                }),
                json!([]),
            ),
        ),
        tool(
            "screen_info",
            "Report video dimensions, physical display size, density and rotation.",
            schema(json!({}), json!([])),
        ),
        tool(
            "current_app",
            "Report the foreground package/activity from dumpsys.",
            schema(json!({}), json!([])),
        ),
        tool(
            "tap",
            "Tap once at a point.",
            schema(
                json!({
                    "x": {"type": "integer", "minimum": 0},
                    "y": {"type": "integer", "minimum": 0},
                    "settle_ms": {"type": "integer", "minimum": 0, "description": "How long to wait for the screen to change (default 120)"}
                }),
                json!(["x", "y"]),
            ),
        ),
        tool(
            "double_tap",
            "Tap twice at the same point.",
            schema(
                json!({
                    "x": {"type": "integer", "minimum": 0},
                    "y": {"type": "integer", "minimum": 0},
                    "interval_ms": {"type": "integer", "minimum": 0, "description": "Gap between taps (default 80)"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["x", "y"]),
            ),
        ),
        tool(
            "long_press",
            "Press and hold a point.",
            schema(
                json!({
                    "x": {"type": "integer", "minimum": 0},
                    "y": {"type": "integer", "minimum": 0},
                    "duration_ms": {"type": "integer", "minimum": 1, "description": "Hold duration (default 700)"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["x", "y"]),
            ),
        ),
        tool(
            "swipe",
            "Swipe from one point to another (scrolling lists, unlocking, dismissing).",
            schema(
                json!({
                    "x1": {"type": "integer", "minimum": 0},
                    "y1": {"type": "integer", "minimum": 0},
                    "x2": {"type": "integer", "minimum": 0},
                    "y2": {"type": "integer", "minimum": 0},
                    "duration_ms": {"type": "integer", "minimum": 1, "description": "Gesture duration (default 300)"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["x1", "y1", "x2", "y2"]),
            ),
        ),
        tool(
            "drag",
            "Press, hold briefly and drag to another point (long-press then move, e.g. reordering or dragging a slider).",
            schema(
                json!({
                    "x1": {"type": "integer", "minimum": 0},
                    "y1": {"type": "integer", "minimum": 0},
                    "x2": {"type": "integer", "minimum": 0},
                    "y2": {"type": "integer", "minimum": 0},
                    "hold_ms": {"type": "integer", "minimum": 0, "description": "Pause after pressing (default 250)"},
                    "duration_ms": {"type": "integer", "minimum": 1, "description": "Movement duration (default 400)"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["x1", "y1", "x2", "y2"]),
            ),
        ),
        tool(
            "scroll",
            "Wheel-scroll at a point. Positive dy scrolls down, positive dx scrolls right; units are wheel notches.",
            schema(
                json!({
                    "x": {"type": "integer", "minimum": 0},
                    "y": {"type": "integer", "minimum": 0},
                    "dx": {"type": "number", "description": "Horizontal notches (default 0)"},
                    "dy": {"type": "number", "description": "Vertical notches, positive = down (default 4)"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["x", "y"]),
            ),
        ),
        tool(
            "type_text",
            "Type text into the focused field. ASCII uses key events; other scripts go through the device clipboard and a paste key event (the soft keyboard may appear).",
            schema(
                json!({
                    "text": {"type": "string"},
                    "mode": {"type": "string", "enum": ["auto", "key", "clipboard"], "description": "auto (default) picks clipboard for non-ASCII"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["text"]),
            ),
        ),
        tool(
            "press_key",
            "Press a named Android key (ENTER, BACK, HOME, TAB, ESC, DEL, VOLUME_UP, WAKEUP, ...) or a numeric keycode, with optional modifiers.",
            schema(
                json!({
                    "key": {"type": ["string", "integer"], "description": "Name or numeric Android keycode"},
                    "modifiers": {"type": "array", "items": {"type": "string", "enum": ["shift", "ctrl", "alt", "meta"]}},
                    "repeat": {"type": "integer", "minimum": 1, "description": "How many times to press (default 1)"},
                    "settle_ms": {"type": "integer", "minimum": 0}
                }),
                json!(["key"]),
            ),
        ),
        tool(
            "back",
            "Press the system BACK button (turns the screen on if it was off).",
            schema(json!({"settle_ms": {"type": "integer", "minimum": 0}}), json!([])),
        ),
        tool(
            "rotate",
            "Rotate the device screen by 90 degrees (respects the rotation lock).",
            schema(json!({"settle_ms": {"type": "integer", "minimum": 0}}), json!([])),
        ),
        tool(
            "notifications",
            "Open or close the notification shade.",
            schema(json!({"open": {"type": "boolean", "description": "true opens, false closes (default true)"}}), json!([])),
        ),
        tool(
            "clipboard_get",
            "Read the device clipboard.",
            schema(json!({"timeout_ms": {"type": "integer", "minimum": 1, "description": "Default 1500"}}), json!([])),
        ),
        tool(
            "clipboard_set",
            "Set the device clipboard (without pasting).",
            schema(json!({"text": {"type": "string"}}), json!(["text"])),
        ),
        tool(
            "launch_app",
            "Launch an app by package name, optionally force-stopping it first.",
            schema(
                json!({
                    "package": {"type": "string", "description": "e.g. com.android.settings"},
                    "force_stop": {"type": "boolean", "description": "Run am force-stop first (default false)"}
                }),
                json!(["package"]),
            ),
        ),
        tool(
            "list_apps",
            "List installed packages (third-party only by default).",
            schema(json!({"third_party_only": {"type": "boolean", "description": "Default true"}}), json!([])),
        ),
        tool(
            "wake",
            "Wake the screen and dismiss the keyguard swipe.",
            schema(json!({}), json!([])),
        ),
        tool(
            "shell",
            "Run a command on the device through adb shell. Powerful; prefer dedicated tools and keep commands read-only unless the task really needs it.",
            schema(
                json!({
                    "command": {"type": "string"},
                    "timeout_ms": {"type": "integer", "minimum": 1, "description": "Default 10000"}
                }),
                json!(["command"]),
            ),
        ),
    ])
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    #[serde(default)]
    wait_ms: Option<u32>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartSessionArgs {
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    max_fps: Option<u32>,
    #[serde(default)]
    max_size: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointArgs {
    x: u32,
    y: u32,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DoubleTapArgs {
    x: u32,
    y: u32,
    #[serde(default)]
    interval_ms: Option<u32>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LongPressArgs {
    x: u32,
    y: u32,
    #[serde(default)]
    duration_ms: Option<u32>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SwipeArgs {
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    #[serde(default)]
    duration_ms: Option<u32>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DragArgs {
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    #[serde(default)]
    hold_ms: Option<u32>,
    #[serde(default)]
    duration_ms: Option<u32>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    x: u32,
    y: u32,
    #[serde(default)]
    dx: Option<f32>,
    #[serde(default)]
    dy: Option<f32>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TypeTextArgs {
    text: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PressKeyArgs {
    key: Value,
    #[serde(default)]
    modifiers: Option<Vec<String>>,
    #[serde(default)]
    repeat: Option<u32>,
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettleArgs {
    #[serde(default)]
    settle_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationsArgs {
    #[serde(default)]
    open: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardGetArgs {
    #[serde(default)]
    timeout_ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardSetArgs {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchAppArgs {
    package: String,
    #[serde(default)]
    force_stop: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListAppsArgs {
    #[serde(default)]
    third_party_only: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    command: String,
    #[serde(default)]
    timeout_ms: Option<u32>,
}

fn parse_args<T: for<'de> Deserialize<'de>>(args: &Value) -> Result<T, String> {
    serde_json::from_value(args.clone()).map_err(|err| format!("invalid arguments: {err}"))
}

async fn blocking<T, F>(work: F) -> anyhow::Result<T>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(err) => Err(anyhow::anyhow!("background task failed: {err}")),
    }
}

fn require_session(state: &AppState) -> Result<Arc<Session>, String> {
    state.session().ok_or_else(|| NO_SESSION.to_string())
}

fn require_serial(state: &AppState) -> Result<String, String> {
    if let Some(session) = state.session() {
        return Ok(session.serial.clone());
    }
    let devices = state.adb.devices().map_err(|err| format!("{err:#}"))?;
    let usable: Vec<_> = devices
        .into_iter()
        .filter(|device| device.state.is_usable())
        .collect();
    match usable.len() {
        1 => Ok(usable[0].serial.clone()),
        0 => Err(
            "no usable Android device is attached (check USB debugging and authorization)"
                .to_string(),
        ),
        _ => Err("several devices are attached; pass the serial explicitly".to_string()),
    }
}

fn report_json(report: ActionReport) -> Value {
    json!({
        "changed": report.changed,
        "elapsed_ms": report.elapsed_ms,
        "width": report.width,
        "height": report.height,
    })
}

async fn run_action<F>(state: Arc<AppState>, settle_ms: Option<u32>, action: F) -> ToolOutput
where
    F: FnOnce(Arc<Session>, u32) -> anyhow::Result<ActionReport> + Send + 'static,
{
    let session = match require_session(&state) {
        Ok(session) => session,
        Err(err) => return ToolOutput::err(err),
    };
    let settle = settle_ms.unwrap_or_else(|| state.cfg.lock().unwrap().settle_ms);
    match blocking(move || action(session, settle)).await {
        Ok(report) => ToolOutput::ok_json(report_json(report)),
        Err(err) => ToolOutput::err(format!("{err:#}")),
    }
}

pub async fn call(state: Arc<AppState>, name: &str, args: Value) -> ToolOutput {
    match name {
        "list_devices" => {
            let adb = state.adb.clone();
            match blocking(move || adb.devices()).await {
                Ok(devices) => {
                    let list: Vec<Value> = devices
                        .iter()
                        .map(|device| {
                            json!({
                                "serial": device.serial,
                                "state": device.state.as_str(),
                                "model": device.model,
                                "product": device.product,
                                "label": device.label(),
                            })
                        })
                        .collect();
                    ToolOutput::ok_json(json!({
                        "devices": list,
                        "hint": "unauthorized devices must accept the USB debugging prompt",
                    }))
                }
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "session_status" => {
            let session = state.session();
            let status = session.as_ref().map(|session| session.status());
            let value = match status {
                Some(status) => json!({
                    "active": true,
                    "serial": status.serial,
                    "device_name": status.device_name,
                    "state": status.state,
                    "video": status.dims.map(|dims| json!({"width": dims.width, "height": dims.height})),
                    "uptime_s": status.uptime_s,
                    "frame_age_ms": status.frame_age_ms,
                    "mcp_url": state.mcp_url(),
                }),
                None => json!({
                    "active": false,
                    "mcp_url": state.mcp_url(),
                    "hint": "start a session from the app window or call start_session",
                }),
            };
            ToolOutput::ok_json(value)
        }

        "start_session" => {
            let parsed: StartSessionArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let serial = match parsed.serial {
                Some(serial) => serial,
                None => match require_serial(&state) {
                    Ok(serial) => serial,
                    Err(err) => return ToolOutput::err(err),
                },
            };
            {
                let mut cfg = state.cfg.lock().unwrap();
                if let Some(max_fps) = parsed.max_fps {
                    cfg.max_fps = max_fps;
                }
                if let Some(max_size) = parsed.max_size {
                    cfg.max_size = max_size;
                }
            }
            let state_for_task = Arc::clone(&state);
            match blocking(move || state_for_task.open_session(&serial)).await {
                Ok(session) => {
                    let status = session.status();
                    ToolOutput::ok_json(json!({
                        "serial": status.serial,
                        "device_name": status.device_name,
                        "state": status.state,
                        "video": status.dims.map(|dims| json!({"width": dims.width, "height": dims.height})),
                    }))
                }
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "stop_session" => {
            state.close_session();
            ToolOutput::ok_json(json!({"stopped": true}))
        }

        "screenshot" => {
            let parsed: ScreenshotArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let wait_ms = parsed.wait_ms.unwrap_or(500);
            let source = parsed.source.unwrap_or_else(|| "video".to_string());

            if source == "adb" {
                let serial = match require_serial(&state) {
                    Ok(serial) => serial,
                    Err(err) => return ToolOutput::err(err),
                };
                let state_for_task = Arc::clone(&state);
                let serial_for_task = serial.clone();
                return match blocking(move || adb_screenshot(&state_for_task, &serial_for_task))
                    .await
                {
                    Ok((png, width, height)) => ToolOutput::image(
                        png,
                        json!({"source": "adb", "width": width, "height": height, "serial": serial}),
                    ),
                    Err(err) => ToolOutput::err(format!("{err:#}")),
                };
            }

            let session = match require_session(&state) {
                Ok(session) => session,
                Err(err) => return ToolOutput::err(err),
            };
            let serial = session.serial.clone();
            match blocking(move || {
                if session.frames().latest().is_none() {
                    session.wait_first_frame(Duration::from_millis(wait_ms as u64));
                }
                session.screenshot()
            })
            .await
            {
                Ok(shot) => ToolOutput::image(
                    shot.png,
                    json!({
                        "source": "video",
                        "width": shot.width,
                        "height": shot.height,
                        "frame_age_ms": shot.age_ms,
                        "serial": serial,
                    }),
                ),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "screen_info" => {
            let session = state.session();
            let video = session.as_ref().and_then(|session| session.dims());
            let serial = match require_serial(&state) {
                Ok(serial) => serial,
                Err(err) => return ToolOutput::err(err),
            };
            let adb = state.adb.clone();
            match blocking(move || {
                let wm_size = adb
                    .shell_stdout(&serial, "wm size", Duration::from_secs(5))
                    .unwrap_or_default();
                let density = adb
                    .shell_stdout(&serial, "wm density", Duration::from_secs(5))
                    .unwrap_or_default();
                Ok(json!({
                    "serial": serial,
                    "video": video.map(|dims| json!({"width": dims.width, "height": dims.height})),
                    "wm_size": wm_size,
                    "wm_density": density,
                }))
            })
            .await
            {
                Ok(value) => ToolOutput::ok_json(value),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "current_app" => {
            let serial = match require_serial(&state) {
                Ok(serial) => serial,
                Err(err) => return ToolOutput::err(err),
            };
            let adb = state.adb.clone();
            match blocking(move || {
                let activity = adb
                    .shell_stdout(
                        &serial,
                        "dumpsys activity activities | grep -m1 -E 'mResumedActivity|topResumedActivity|ResumedActivity'",
                        Duration::from_secs(8),
                    )
                    .unwrap_or_default();
                let focus = adb
                    .shell_stdout(
                        &serial,
                        "dumpsys window | grep -m1 mCurrentFocus",
                        Duration::from_secs(8),
                    )
                    .unwrap_or_default();
                let package = extract_package(&activity)
                    .or_else(|| extract_package(&focus));
                Ok(json!({
                    "serial": serial,
                    "package": package,
                    "activity_line": activity,
                    "focus_line": focus,
                }))
            })
            .await
            {
                Ok(value) => ToolOutput::ok_json(value),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "tap" => {
            let parsed: PointArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.tap(parsed.x, parsed.y, settle)
            })
            .await
        }

        "double_tap" => {
            let parsed: DoubleTapArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.double_tap(parsed.x, parsed.y, parsed.interval_ms.unwrap_or(80), settle)
            })
            .await
        }

        "long_press" => {
            let parsed: LongPressArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.long_press(
                    parsed.x,
                    parsed.y,
                    parsed.duration_ms.unwrap_or(700),
                    settle,
                )
            })
            .await
        }

        "swipe" => {
            let parsed: SwipeArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.swipe(
                    parsed.x1,
                    parsed.y1,
                    parsed.x2,
                    parsed.y2,
                    parsed.duration_ms.unwrap_or(300),
                    settle,
                )
            })
            .await
        }

        "drag" => {
            let parsed: DragArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.drag(
                    parsed.x1,
                    parsed.y1,
                    parsed.x2,
                    parsed.y2,
                    parsed.hold_ms.unwrap_or(250),
                    parsed.duration_ms.unwrap_or(400),
                    settle,
                )
            })
            .await
        }

        "scroll" => {
            let parsed: ScrollArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let dx = parsed.dx.unwrap_or(0.0);
            let dy = parsed.dy.unwrap_or(4.0);
            if dx == 0.0 && dy == 0.0 {
                return ToolOutput::err("scroll needs a non-zero dx or dy");
            }
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.scroll(parsed.x, parsed.y, dx, dy, settle)
            })
            .await
        }

        "type_text" => {
            let parsed: TypeTextArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let mode = match parsed.mode.as_deref() {
                Some("key") => TextMode::Key,
                Some("clipboard") => TextMode::Clipboard,
                Some("auto") | None => TextMode::Auto,
                Some(other) => {
                    return ToolOutput::err(format!(
                        "unknown mode {other:?}; use auto, key or clipboard"
                    ))
                }
            };
            let text = parsed.text;
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.type_text(&text, mode, settle)
            })
            .await
        }

        "press_key" => {
            let parsed: PressKeyArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let key = match parsed.key {
                Value::String(value) => value,
                Value::Number(value) => value.to_string(),
                _ => return ToolOutput::err("key must be a name or a numeric keycode"),
            };
            let modifiers = parsed.modifiers.unwrap_or_default();
            let repeat = parsed.repeat.unwrap_or(1);
            run_action(state, parsed.settle_ms, move |session, settle| {
                session.press_key(&key, &modifiers, repeat, settle)
            })
            .await
        }

        "back" => {
            let parsed: SettleArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, |session, settle| {
                session.back(settle)
            })
            .await
        }

        "rotate" => {
            let parsed: SettleArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            run_action(state, parsed.settle_ms, |session, settle| {
                session.rotate(settle)
            })
            .await
        }

        "notifications" => {
            let parsed: NotificationsArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let open = parsed.open.unwrap_or(true);
            run_action(state, None, move |session, settle| {
                session.notifications(open, settle)
            })
            .await
        }

        "clipboard_get" => {
            let parsed: ClipboardGetArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let timeout = Duration::from_millis(parsed.timeout_ms.unwrap_or(1500) as u64);
            let session = match require_session(&state) {
                Ok(session) => session,
                Err(err) => return ToolOutput::err(err),
            };
            match blocking(move || session.clipboard_text(timeout)).await {
                Ok(text) => ToolOutput::ok_json(json!({"text": text})),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "clipboard_set" => {
            let parsed: ClipboardSetArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let session = match require_session(&state) {
                Ok(session) => session,
                Err(err) => return ToolOutput::err(err),
            };
            let text = parsed.text;
            match blocking(move || session.set_clipboard(&text)).await {
                Ok(()) => ToolOutput::ok_json(json!({"ok": true})),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "launch_app" => {
            let parsed: LaunchAppArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            if !valid_package(&parsed.package) {
                return ToolOutput::err(format!("invalid package name {:?}", parsed.package));
            }
            let serial = match require_serial(&state) {
                Ok(serial) => serial,
                Err(err) => return ToolOutput::err(err),
            };
            let package = parsed.package.clone();
            let force_stop = parsed.force_stop.unwrap_or(false);
            let adb = state.adb.clone();
            match blocking(move || launch(&adb, &serial, &package, force_stop)).await {
                Ok(value) => ToolOutput::ok_json(value),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "list_apps" => {
            let parsed: ListAppsArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let third_party = parsed.third_party_only.unwrap_or(true);
            let serial = match require_serial(&state) {
                Ok(serial) => serial,
                Err(err) => return ToolOutput::err(err),
            };
            let adb = state.adb.clone();
            match blocking(move || {
                let command = if third_party {
                    "pm list packages -3"
                } else {
                    "pm list packages"
                };
                let output = adb.shell_stdout(&serial, command, Duration::from_secs(15))?;
                let packages: Vec<String> = output
                    .lines()
                    .filter_map(|line| line.trim().strip_prefix("package:").map(str::to_string))
                    .collect();
                Ok(packages)
            })
            .await
            {
                Ok(packages) => {
                    ToolOutput::ok_json(json!({"packages": packages, "count": packages.len()}))
                }
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "wake" => {
            let serial = match require_serial(&state) {
                Ok(serial) => serial,
                Err(err) => return ToolOutput::err(err),
            };
            let adb = state.adb.clone();
            match blocking(move || {
                let _ = adb.shell(
                    &serial,
                    "input keyevent KEYCODE_WAKEUP",
                    Duration::from_secs(8),
                );
                let dismiss =
                    adb.shell_stdout(&serial, "wm dismiss-keyguard", Duration::from_secs(8));
                Ok(json!({"woke": true, "dismiss_keyguard": dismiss.ok()}))
            })
            .await
            {
                Ok(value) => ToolOutput::ok_json(value),
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        "shell" => {
            let parsed: ShellArgs = match parse_args(&args) {
                Ok(parsed) => parsed,
                Err(err) => return ToolOutput::err(err),
            };
            let serial = match require_serial(&state) {
                Ok(serial) => serial,
                Err(err) => return ToolOutput::err(err),
            };
            let adb = state.adb.clone();
            let timeout = Duration::from_millis(parsed.timeout_ms.unwrap_or(10_000) as u64);
            match blocking(move || {
                let output = adb.shell(&serial, &parsed.command, timeout)?;
                let code = output.status.code();
                Ok(json!({
                    "serial": serial,
                    "command": parsed.command,
                    "exit_code": code,
                    "stdout": String::from_utf8_lossy(&output.stdout),
                    "stderr": String::from_utf8_lossy(&output.stderr),
                }))
            })
            .await
            {
                Ok(value) => {
                    let failed = value["exit_code"]
                        .as_i64()
                        .map(|code| code != 0)
                        .unwrap_or(false);
                    if failed {
                        ToolOutput::err(format!("command failed: {}", value))
                    } else {
                        ToolOutput::ok_json(value)
                    }
                }
                Err(err) => ToolOutput::err(format!("{err:#}")),
            }
        }

        other => ToolOutput::err(format!("unknown tool {other:?}")),
    }
}

fn adb_screenshot(state: &AppState, serial: &str) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let output = state.adb.run(
        &["-s", serial, "exec-out", "screencap", "-p"],
        Duration::from_secs(20),
    )?;
    if !output.status.success() {
        anyhow::bail!(
            "screencap failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let png = output.stdout;
    let (width, height) = png_dimensions(&png)
        .ok_or_else(|| anyhow::anyhow!("screencap did not return a PNG image"))?;
    Ok((png, width, height))
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Some((width, height))
}

fn extract_package(line: &str) -> Option<String> {
    let slash = line.find('/')?;
    let before = &line[..slash];
    let candidate = before.split_whitespace().last()?;
    if candidate.contains('.') {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn valid_package(package: &str) -> bool {
    !package.is_empty()
        && package.len() <= 200
        && package
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_'))
}

fn launch(
    adb: &crate::android::Adb,
    serial: &str,
    package: &str,
    force_stop: bool,
) -> anyhow::Result<Value> {
    if force_stop {
        adb.shell(
            serial,
            &format!("am force-stop {package}"),
            Duration::from_secs(10),
        )?;
    }
    let resolved = adb
        .shell_stdout(
            serial,
            &format!("cmd package resolve-activity --brief {package}"),
            Duration::from_secs(15),
        )
        .unwrap_or_default();
    let mut candidates = resolved
        .lines()
        .map(str::trim)
        .filter(|line| line.contains('/'));
    let component = candidates.next_back().map(str::to_string);

    let output = match &component {
        Some(component) => adb.shell(
            serial,
            &format!("am start -W -n {component}"),
            Duration::from_secs(25),
        )?,
        None => adb.shell(
            serial,
            &format!("monkey -p {package} -c android.intent.category.LAUNCHER 1"),
            Duration::from_secs(25),
        )?,
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        anyhow::bail!(
            "could not launch {package}: {}{}",
            stdout.trim(),
            stderr.trim()
        );
    }
    let sleep_ms = if stdout.contains("TotalTime") {
        300
    } else {
        800
    };
    std::thread::sleep(Duration::from_millis(sleep_ms));
    Ok(json!({
        "package": package,
        "component": component,
        "stdout": stdout.lines().take(4).collect::<Vec<_>>(),
    }))
}
