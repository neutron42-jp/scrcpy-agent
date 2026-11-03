//! GTK4 front-end: pick a device, connect, watch the screen, start/stop MCP.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{
    gdk, glib, Application, ApplicationWindow, Box as GtkBox, Button, DropDown, Entry, HeaderBar,
    Label, Orientation, Picture, ScrolledWindow, Separator, Switch, TextView,
};

use crate::android::DeviceEntry;
use crate::app::{AppState, UiEvent};

pub fn run(state: Arc<AppState>) -> glib::ExitCode {
    let app = Application::builder()
        .application_id("dev.neutron42.ScrcpyAgent")
        .build();
    app.connect_activate(move |app| build(app, Arc::clone(&state)));
    // clap already consumed our options; keep GTK from re-parsing them.
    app.run_with_args(&[] as &[&str])
}

fn build(app: &Application, state: Arc<AppState>) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("scrcpy-agent")
        .default_width(1180)
        .default_height(840)
        .build();

    let header = HeaderBar::new();
    window.set_titlebar(Some(&header));

    let (tx, rx) = mpsc::channel::<UiEvent>();
    state.set_ui(Some(tx));

    let refresh_button = Button::with_label("端末更新");
    let connect_button = Button::with_label("接続");
    let disconnect_button = Button::with_label("切断");
    header.pack_start(&refresh_button);
    header.pack_end(&disconnect_button);
    header.pack_end(&connect_button);

    let root = GtkBox::new(Orientation::Horizontal, 0);

    // ---- left panel -----------------------------------------------------
    let side = GtkBox::new(Orientation::Vertical, 8);
    side.set_width_request(330);
    side.set_margin_top(10);
    side.set_margin_bottom(10);
    side.set_margin_start(10);
    side.set_margin_end(10);

    let device_label = Label::new(Some("端末"));
    device_label.set_xalign(0.0);
    let devices = DropDown::from_strings(&["(端末なし)"]);
    let known_devices = Rc::new(RefCell::new(Vec::<DeviceEntry>::new()));

    side.append(&device_label);
    side.append(&devices);

    let mcp_separator = Separator::new(Orientation::Horizontal);
    side.append(&mcp_separator);

    let mcp_label = Label::new(Some("MCPサーバー"));
    mcp_label.set_xalign(0.0);
    side.append(&mcp_label);

    let port_row = GtkBox::new(Orientation::Horizontal, 6);
    let port_label = Label::new(Some("ポート"));
    let port_entry = Entry::new();
    let cfg = state.cfg.lock().unwrap().clone();
    port_entry.set_text(&cfg.mcp_port.to_string());
    port_entry.set_width_chars(6);
    port_row.append(&port_label);
    port_row.append(&port_entry);
    side.append(&port_row);

    let mcp_buttons = GtkBox::new(Orientation::Horizontal, 6);
    let start_mcp_button = Button::with_label("MCPサーバー起動");
    let stop_mcp_button = Button::with_label("停止");
    stop_mcp_button.set_sensitive(false);
    mcp_buttons.append(&start_mcp_button);
    mcp_buttons.append(&stop_mcp_button);
    side.append(&mcp_buttons);

    let url_label = Label::new(Some("MCP: (停止中)"));
    url_label.set_xalign(0.0);
    url_label.set_selectable(true);
    url_label.set_wrap(true);
    side.append(&url_label);

    let copy_button = Button::with_label("URLをコピー");
    side.append(&copy_button);

    let manual_switch = Switch::new();
    manual_switch.set_active(cfg.manual_input);
    let manual_row = GtkBox::new(Orientation::Horizontal, 6);
    let manual_label = Label::new(Some("プレビューの手動操作"));
    manual_label.set_xalign(0.0);
    manual_label.set_hexpand(true);
    manual_row.append(&manual_label);
    manual_row.append(&manual_switch);
    side.append(&manual_row);

    let status_label = Label::new(Some("未接続"));
    status_label.set_xalign(0.0);
    status_label.set_wrap(true);
    side.append(&status_label);

    // ---- right panel ----------------------------------------------------
    let picture = Picture::new();
    picture.set_content_fit(gtk4::ContentFit::Contain);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_size_request(480, 320);

    let text_view = TextView::new();
    text_view.set_editable(false);
    text_view.set_monospace(true);
    let log_buffer = text_view.buffer();
    let scrolled = ScrolledWindow::new();
    scrolled.set_min_content_height(180);
    scrolled.set_child(Some(&text_view));

    let right = GtkBox::new(Orientation::Vertical, 6);
    right.set_margin_top(10);
    right.set_margin_bottom(10);
    right.set_margin_start(10);
    right.set_margin_end(10);
    right.append(&picture);
    right.append(&scrolled);

    root.append(&side);
    root.append(&Separator::new(Orientation::Vertical));
    root.append(&right);
    window.set_child(Some(&root));

    // ---- device list ----------------------------------------------------
    let refresh = {
        let state = Arc::clone(&state);
        let devices = devices.clone();
        let known = Rc::clone(&known_devices);
        let log_buffer = log_buffer.clone();
        move || match state.adb.devices() {
            Ok(list) => {
                let labels: Vec<String> = if list.is_empty() {
                    vec!["(端末なし)".to_string()]
                } else {
                    list.iter().map(|device| device.label()).collect()
                };
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                devices.set_model(Some(&gtk4::StringList::new(&refs)));
                devices.set_selected(0);
                *known.borrow_mut() = list;
            }
            Err(err) => append_log(&log_buffer, &format!("adb error: {err:#}")),
        }
    };
    refresh();
    refresh_button.connect_clicked({
        let refresh = Rc::new(refresh);
        move |_| refresh()
    });

    connect_button.connect_clicked({
        let state = Arc::clone(&state);
        let devices = devices.clone();
        let known = Rc::clone(&known_devices);
        move |_| {
            let list = known.borrow();
            let index = devices.selected() as usize;
            let Some(device) = list.get(index) else {
                state.log("接続する端末を選んでください");
                return;
            };
            if !device.state.is_usable() {
                state.log(format!(
                    "{} は {} です。端末側の USB デバッグ許可を確認してください",
                    device.serial,
                    device.state.as_str()
                ));
                return;
            }
            let serial = device.serial.clone();
            let state_for_thread = Arc::clone(&state);
            state.log(format!("接続中: {serial}"));
            std::thread::spawn(move || {
                if let Err(err) = state_for_thread.open_session(&serial) {
                    state_for_thread.log(format!("接続失敗: {err:#}"));
                }
            });
        }
    });

    disconnect_button.connect_clicked({
        let state = Arc::clone(&state);
        move |_| state.close_session()
    });

    // ---- MCP buttons ----------------------------------------------------
    start_mcp_button.connect_clicked({
        let state = Arc::clone(&state);
        let port_entry = port_entry.clone();
        let url_label = url_label.clone();
        let start = start_mcp_button.clone();
        let stop = stop_mcp_button.clone();
        move |_| {
            if let Ok(port) = port_entry.text().parse::<u16>() {
                state.cfg.lock().unwrap().mcp_port = port;
            }
            match state.start_mcp() {
                Ok(url) => {
                    url_label.set_text(&format!("MCP: {url}"));
                    state.log(format!("MCPサーバー起動: {url}"));
                    start.set_sensitive(false);
                    stop.set_sensitive(true);
                }
                Err(err) => state.log(format!("MCP起動失敗: {err:#}")),
            }
        }
    });

    stop_mcp_button.connect_clicked({
        let state = Arc::clone(&state);
        let url_label = url_label.clone();
        let start = start_mcp_button.clone();
        let stop = stop_mcp_button.clone();
        move |_| {
            state.stop_mcp();
            url_label.set_text("MCP: (停止中)");
            state.log("MCPサーバー停止");
            start.set_sensitive(true);
            stop.set_sensitive(false);
        }
    });

    copy_button.connect_clicked({
        let state = Arc::clone(&state);
        let window = window.clone();
        move |_| {
            if let Some(url) = state.mcp_url() {
                window.clipboard().set_text(&url);
                state.log(format!("コピーしました: {url}"));
            } else {
                state.log("MCPサーバーは停止中です");
            }
        }
    });

    manual_switch.connect_active_notify({
        let state = Arc::clone(&state);
        move |switch| {
            state
                .manual_input
                .store(switch.is_active(), Ordering::Relaxed)
        }
    });

    // ---- preview input --------------------------------------------------
    let click = gtk4::GestureClick::new();
    click.connect_pressed({
        let state = Arc::clone(&state);
        let picture = picture.clone();
        move |_, _presses, x, y| {
            if !state.manual_input_enabled() {
                return;
            }
            let Some((px, py)) = map_to_frame(&picture, &state, x, y) else {
                return;
            };
            if let Some(session) = state.session() {
                let settle = state.cfg.lock().unwrap().settle_ms;
                std::thread::spawn(move || {
                    let _ = session.tap(px, py, settle);
                });
            }
        }
    });
    picture.add_controller(click);

    let drag_start = Rc::new(Cell::new((0.0f64, 0.0f64)));
    let drag = gtk4::GestureDrag::new();
    drag.connect_drag_begin({
        let drag_start = Rc::clone(&drag_start);
        move |_, x, y| drag_start.set((x, y))
    });
    drag.connect_drag_end({
        let state = Arc::clone(&state);
        let picture = picture.clone();
        let drag_start = Rc::clone(&drag_start);
        move |_, offset_x, offset_y| {
            if !state.manual_input_enabled() {
                return;
            }
            let (start_x, start_y) = drag_start.get();
            let end_x = start_x + offset_x;
            let end_y = start_y + offset_y;
            let Some((sx, sy)) = map_to_frame(&picture, &state, start_x, start_y) else {
                return;
            };
            let Some((ex, ey)) = map_to_frame(&picture, &state, end_x, end_y) else {
                return;
            };
            if let Some(session) = state.session() {
                let settle = state.cfg.lock().unwrap().settle_ms;
                std::thread::spawn(move || {
                    let _ = session.swipe(sx, sy, ex, ey, 260, settle);
                });
            }
        }
    });
    picture.add_controller(drag);

    // ---- frame and event pump ------------------------------------------
    let last_seq = Rc::new(Cell::new(0u64));
    let rx = Rc::new(RefCell::new(rx));
    glib::timeout_add_local(Duration::from_millis(16), {
        let state = Arc::clone(&state);
        let picture = picture.clone();
        let status_label = status_label.clone();
        let url_label = url_label.clone();
        let log_buffer = log_buffer.clone();
        let last_seq = Rc::clone(&last_seq);
        move || {
            while let Ok(event) = rx.borrow_mut().try_recv() {
                match event {
                    UiEvent::Log(line) => append_log(&log_buffer, &line),
                    UiEvent::SessionChanged => status_label.set_text(&state.status_line()),
                    UiEvent::McpChanged => match state.mcp_url() {
                        Some(url) => url_label.set_text(&format!("MCP: {url}")),
                        None => url_label.set_text("MCP: (停止中)"),
                    },
                    UiEvent::ToolCall { name, ok, detail } => {
                        let mark = if ok { "ok" } else { "error" };
                        append_log(&log_buffer, &format!("[tool] {name} {mark}: {detail}"));
                    }
                }
            }
            if let Some(session) = state.session() {
                let seq = session.frames().seq();
                if seq != last_seq.get() {
                    if let Some(frame) = session.frames().latest() {
                        last_seq.set(seq);
                        let bytes = glib::Bytes::from(&frame.rgba[..]);
                        let texture = gdk::MemoryTexture::new(
                            frame.width as i32,
                            frame.height as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &bytes,
                            frame.width as usize * 4,
                        );
                        picture.set_paintable(Some(&texture));
                    }
                }
            }
            glib::ControlFlow::Continue
        }
    });

    // ---- window lifecycle ----------------------------------------------
    window.connect_close_request({
        let state = Arc::clone(&state);
        move |_| {
            state.close_session();
            state.stop_mcp();
            glib::Propagation::Proceed
        }
    });

    append_log(
        &log_buffer,
        "scrcpy-agent 起動。端末を選んで「接続」、次に「MCPサーバー起動」を押してください。",
    );
    window.present();
}

fn append_log(buffer: &gtk4::TextBuffer, line: &str) {
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, line);
    buffer.insert(&mut end, "\n");
}

fn map_to_frame(picture: &Picture, state: &AppState, x: f64, y: f64) -> Option<(u32, u32)> {
    let session = state.session()?;
    let dims = session.dims()?;
    let widget_width = picture.width() as f64;
    let widget_height = picture.height() as f64;
    if widget_width <= 1.0 || widget_height <= 1.0 {
        return None;
    }
    let scale = (widget_width / dims.width as f64).min(widget_height / dims.height as f64);
    if scale <= 0.0 {
        return None;
    }
    let drawn_width = dims.width as f64 * scale;
    let drawn_height = dims.height as f64 * scale;
    let offset_x = (widget_width - drawn_width) / 2.0;
    let offset_y = (widget_height - drawn_height) / 2.0;
    let frame_x = (x - offset_x) / scale;
    let frame_y = (y - offset_y) / scale;
    if frame_x < 0.0 || frame_y < 0.0 {
        return None;
    }
    if frame_x >= dims.width as f64 || frame_y >= dims.height as f64 {
        return None;
    }
    Some((frame_x as u32, frame_y as u32))
}
