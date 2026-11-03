//! GTK4 front end.
//!
//! The layout follows the GNOME HIG: a header bar carries the window title, the
//! connection state and the primary actions; boxed lists hold devices, the MCP
//! server, session properties and settings; and a placeholder page fills the
//! preview while nothing is connected. The app is dark only for now.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gtk4 as gtk;
use gtk4::gio;
use gtk4::gio::prelude::*;
use gtk4::prelude::*;
use gtk4::{
    gdk, glib, pango, AboutDialog, Align, Application, ApplicationWindow, Box as GtkBox, Button,
    ContentFit, CssProvider, Entry, HeaderBar, Image, InterfaceColorScheme, Label, License,
    ListBox, ListBoxRow, MenuButton, Orientation, Overlay, Paned, Picture, PolicyType,
    ScrolledWindow, SelectionMode, Spinner, Stack, StackTransitionType, Switch, TextBuffer,
    TextView, Widget, Window, WrapMode,
};

use crate::android::DeviceEntry;
use crate::app::{AppState, UiEvent};

const CSS: &str = r#"
/* Only what the GTK style classes do not already provide; colours come from
   the theme so the dark variant stays consistent. */

/* The preview is a video surface, so it stays dark in both themes. */
.preview-area {
  background-color: #1b1b1b;
  border-radius: 10px;
}

.preview-placeholder {
  color: rgba(255, 255, 255, 0.95);
}

.osd-chip {
  padding: 4px 10px;
  border-radius: 999px;
  font-size: 0.85em;
}

.empty-icon {
  opacity: 0.45;
}

.state-ready {
  color: @success_color;
}

.state-warning {
  color: @warning_color;
}
"#;

/// Widgets the state pump and the actions keep in sync.
#[derive(Clone)]
struct Ui {
    window: ApplicationWindow,
    header_subtitle: Label,
    spinner: Spinner,
    connect_button: Button,
    disconnect_button: Button,
    appearance_button: Button,
    dark: Rc<Cell<bool>>,
    device_list: ListBox,
    device_stack: Stack,
    devices: Rc<RefCell<Vec<DeviceEntry>>>,
    mcp_switch: Switch,
    mcp_subtitle: Label,
    port_entry: Entry,
    copy_url_row: ListBoxRow,
    copy_dsh_row: ListBoxRow,
    syncing_switch: Rc<Cell<bool>>,
    resolution: Label,
    frame_age: Label,
    foreground: Label,
    battery: Label,
    session_section: GtkBox,
    picture: Picture,
    placeholder: GtkBox,
    hud_left: Label,
    hud_right: Label,
    log_buffer: TextBuffer,
    log_scroll: ScrolledWindow,
}

pub fn run(state: Arc<AppState>) -> glib::ExitCode {
    let app = Application::builder()
        .application_id("dev.neutron42.ScrcpyAgent")
        .build();
    app.connect_activate(move |app| build(app, Arc::clone(&state)));
    // clap already consumed our options; keep GTK from re-parsing them.
    app.run_with_args(&[] as &[&str])
}

fn install_css() {
    let provider = CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Switches the whole interface between the light and dark variants.
///
/// Both settings are written: prefer-dark-theme selects the dark variant of
/// the built-in theme, and gtk-interface-color-scheme (GTK 4.20+) tells themes
/// that style themselves with prefers-color-scheme about the choice.
fn set_appearance(dark: bool) {
    if let Some(settings) = gtk::Settings::default() {
        #[allow(deprecated)]
        settings.set_gtk_application_prefer_dark_theme(dark);
        settings.set_gtk_interface_color_scheme(if dark {
            InterfaceColorScheme::Dark
        } else {
            InterfaceColorScheme::Light
        });
    }
}

fn sync_appearance_button(button: &Button, dark: bool) {
    let (icon, tooltip) = if dark {
        ("weather-clear-symbolic", "Switch to light appearance")
    } else {
        ("weather-clear-night-symbolic", "Switch to dark appearance")
    };
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
}

fn section(title: &str) -> GtkBox {
    let section = GtkBox::new(Orientation::Vertical, 6);
    let label = Label::new(Some(title));
    label.add_css_class("caption-heading");
    label.set_xalign(0.0);
    label.set_margin_start(6);
    section.append(&label);
    section
}

fn boxed_list() -> ListBox {
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::None);
    list.add_css_class("boxed-list");
    list.add_css_class("rich-list");
    list
}

fn icon(name: &str) -> Image {
    let image = Image::from_icon_name(name);
    image.set_pixel_size(16);
    image.set_valign(Align::Center);
    image
}

fn row_title(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label
}

fn row_subtitle(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label
}

fn text_column(title: &str, subtitle: Option<&str>) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 2);
    column.set_hexpand(true);
    column.append(&row_title(title));
    if let Some(subtitle) = subtitle {
        column.append(&row_subtitle(subtitle));
    }
    column
}

fn labeled_column(title: &str, subtitle: &Label) -> GtkBox {
    let column = GtkBox::new(Orientation::Vertical, 2);
    column.set_hexpand(true);
    column.append(&row_title(title));
    column.append(subtitle);
    column
}

fn row(child: &impl IsA<Widget>) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_child(Some(child));
    row
}

fn property_row(title: &str, value: &Label) -> ListBoxRow {
    let content = GtkBox::new(Orientation::Horizontal, 12);
    let name = Label::new(Some(title));
    name.set_xalign(0.0);
    name.set_hexpand(true);
    name.add_css_class("dim-label");
    value.set_xalign(1.0);
    value.set_ellipsize(pango::EllipsizeMode::End);
    content.append(&name);
    content.append(value);
    row(&content)
}

fn action_row(icon_name: &str, title: &str, trailing: Option<&Widget>) -> ListBoxRow {
    let content = GtkBox::new(Orientation::Horizontal, 12);
    let lead = icon(icon_name);
    lead.add_css_class("dim-label");
    content.append(&lead);
    content.append(&text_column(title, None));
    if let Some(trailing) = trailing {
        content.append(trailing);
    }
    row(&content)
}

fn set_action_enabled(ui: &Ui, name: &str, enabled: bool) {
    if let Some(action) = ui.window.lookup_action(name) {
        if let Ok(action) = action.downcast::<gio::SimpleAction>() {
            action.set_enabled(enabled);
        }
    }
}

fn selected_device(ui: &Ui) -> Option<DeviceEntry> {
    let index = ui.device_list.selected_row()?.index() as usize;
    ui.devices.borrow().get(index).cloned()
}

fn build(app: &Application, state: Arc<AppState>) {
    set_appearance(true);
    install_css();

    let window = ApplicationWindow::builder()
        .application(app)
        .title("scrcpy-agent")
        .default_width(1120)
        .default_height(760)
        .build();
    window.set_size_request(760, 560);

    let (tx, rx) = mpsc::channel::<UiEvent>();
    state.set_ui(Some(tx));

    // ---- header bar -----------------------------------------------------
    let header = HeaderBar::new();

    let title_box = GtkBox::new(Orientation::Vertical, 0);
    let title = Label::new(Some("scrcpy-agent"));
    title.add_css_class("heading");
    title.set_xalign(0.0);
    let header_subtitle = Label::new(Some("No device connected"));
    header_subtitle.add_css_class("caption");
    header_subtitle.add_css_class("dim-label");
    header_subtitle.set_xalign(0.0);
    title_box.append(&title);
    title_box.append(&header_subtitle);
    header.set_title_widget(Some(&title_box));

    let spinner = Spinner::new();
    spinner.set_visible(false);
    spinner.set_valign(Align::Center);

    let refresh_button = Button::from_icon_name("view-refresh-symbolic");
    refresh_button.set_tooltip_text(Some("Refresh Devices (Ctrl+R)"));
    refresh_button.set_action_name(Some("win.refresh"));

    let appearance_button = Button::from_icon_name("weather-clear-symbolic");
    appearance_button.set_tooltip_text(Some("Switch to light appearance"));
    appearance_button.set_action_name(Some("win.toggle-appearance"));

    let connect_button = Button::with_label("Connect");
    connect_button.add_css_class("suggested-action");
    connect_button.set_tooltip_text(Some("Connect the selected device (Ctrl+Enter)"));
    connect_button.set_action_name(Some("win.connect"));

    let disconnect_button = Button::with_label("Disconnect");
    disconnect_button.set_tooltip_text(Some("Close the current session"));
    disconnect_button.set_action_name(Some("win.disconnect"));
    disconnect_button.set_visible(false);

    let menu = gio::Menu::new();
    menu.append(Some("Keyboard Shortcuts"), Some("win.shortcuts"));
    menu.append(Some("About scrcpy-agent"), Some("app.about"));
    menu.append(Some("Quit"), Some("app.quit"));
    let menu_button = MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Main Menu")
        .build();

    header.pack_end(&menu_button);
    header.pack_end(&disconnect_button);
    header.pack_end(&connect_button);
    header.pack_end(&appearance_button);
    header.pack_end(&refresh_button);
    header.pack_end(&spinner);
    window.set_titlebar(Some(&header));

    // ---- sidebar: devices ----------------------------------------------
    let sidebar = GtkBox::new(Orientation::Vertical, 18);
    sidebar.set_margin_top(18);
    sidebar.set_margin_bottom(18);
    sidebar.set_margin_start(18);
    sidebar.set_margin_end(18);
    sidebar.set_width_request(330);

    let device_list = ListBox::new();
    device_list.set_selection_mode(SelectionMode::Single);
    device_list.set_activate_on_single_click(false);
    device_list.add_css_class("boxed-list");
    device_list.add_css_class("rich-list");
    let devices = Rc::new(RefCell::new(Vec::<DeviceEntry>::new()));

    let empty_frame = GtkBox::new(Orientation::Vertical, 0);
    empty_frame.add_css_class("boxed-list");
    let empty_page = GtkBox::new(Orientation::Vertical, 6);
    empty_page.set_margin_top(18);
    empty_page.set_margin_bottom(18);
    empty_page.set_margin_start(18);
    empty_page.set_margin_end(18);
    let empty_icon = Image::from_icon_name("phone-symbolic");
    empty_icon.set_pixel_size(32);
    empty_icon.set_halign(Align::Center);
    empty_icon.add_css_class("empty-icon");
    let empty_title = Label::new(Some("No Devices"));
    empty_title.add_css_class("heading");
    let empty_hint = Label::new(Some("Enable USB debugging and reconnect the device."));
    empty_hint.add_css_class("caption");
    empty_hint.add_css_class("dim-label");
    empty_hint.set_wrap(true);
    empty_hint.set_justify(gtk4::Justification::Center);
    empty_hint.set_max_width_chars(28);
    let empty_refresh = Button::with_label("Refresh");
    empty_refresh.add_css_class("flat");
    empty_refresh.set_halign(Align::Center);
    empty_refresh.set_margin_top(6);
    empty_refresh.set_action_name(Some("win.refresh"));
    empty_page.append(&empty_icon);
    empty_page.append(&empty_title);
    empty_page.append(&empty_hint);
    empty_page.append(&empty_refresh);
    empty_frame.append(&empty_page);

    let device_stack = Stack::builder()
        .transition_type(StackTransitionType::Crossfade)
        .build();
    device_stack.add_named(&device_list, Some("list"));
    device_stack.add_named(&empty_frame, Some("empty"));

    let devices_section = section("Devices");
    devices_section.append(&device_stack);

    // ---- sidebar: MCP server -------------------------------------------
    let mcp_list = boxed_list();

    let mcp_switch = Switch::new();
    mcp_switch.set_valign(Align::Center);
    mcp_switch.set_tooltip_text(Some("Start or stop the MCP server (Ctrl+M)"));
    let mcp_subtitle = row_subtitle("Not running");
    let mcp_row_box = GtkBox::new(Orientation::Horizontal, 12);
    mcp_row_box.append(&labeled_column("MCP Server", &mcp_subtitle));
    mcp_row_box.append(&mcp_switch);
    let mcp_row = row(&mcp_row_box);
    mcp_list.append(&mcp_row);

    let cfg = state.cfg.lock().unwrap().clone();
    let port_entry = Entry::new();
    port_entry.set_text(&cfg.mcp_port.to_string());
    port_entry.set_width_chars(6);
    port_entry.set_max_width_chars(6);
    port_entry.set_valign(Align::Center);
    port_entry.set_input_purpose(gtk4::InputPurpose::Digits);
    let port_row_box = GtkBox::new(Orientation::Horizontal, 12);
    port_row_box.append(&text_column("Port", None));
    port_row_box.append(&port_entry);
    let port_row = row(&port_row_box);
    port_row.set_activatable(false);
    mcp_list.append(&port_row);

    let copy_url_row = action_row("edit-copy-symbolic", "Copy Server URL", None);
    mcp_list.append(&copy_url_row);
    let copy_dsh_row = action_row("edit-copy-symbolic", "Copy DSH Config", None);
    mcp_list.append(&copy_dsh_row);

    let mcp_section = section("Server");
    mcp_section.append(&mcp_list);

    // ---- sidebar: session properties -----------------------------------
    let resolution = Label::new(Some("—"));
    let frame_age = Label::new(Some("—"));
    let foreground = Label::new(Some("—"));
    let battery = Label::new(Some("—"));
    let session_list = boxed_list();
    session_list.append(&property_row("Resolution", &resolution));
    session_list.append(&property_row("Last Frame", &frame_age));
    session_list.append(&property_row("Foreground App", &foreground));
    session_list.append(&property_row("Battery", &battery));
    let session_section = section("Session");
    session_section.append(&session_list);

    // ---- sidebar: settings ---------------------------------------------
    let settings_list = boxed_list();
    let manual_switch = Switch::new();
    manual_switch.set_valign(Align::Center);
    manual_switch.set_active(cfg.manual_input);
    let manual_row_box = GtkBox::new(Orientation::Horizontal, 12);
    manual_row_box.append(&text_column(
        "Manual Input",
        Some("Tap, drag and scroll in the preview"),
    ));
    manual_row_box.append(&manual_switch);
    let manual_row = row(&manual_row_box);
    settings_list.append(&manual_row);

    let shortcut_hint = Label::new(Some("Ctrl+S"));
    shortcut_hint.add_css_class("caption");
    shortcut_hint.add_css_class("dim-label");
    shortcut_hint.set_valign(Align::Center);
    let save_row = action_row(
        "document-save-symbolic",
        "Save Current Frame",
        Some(shortcut_hint.upcast_ref::<Widget>()),
    );
    settings_list.append(&save_row);

    let settings_section = section("Settings");
    settings_section.append(&settings_list);

    // Actions first, reference data (session properties) last, so the most
    // useful controls stay above the fold on small windows.
    sidebar.append(&devices_section);
    sidebar.append(&mcp_section);
    sidebar.append(&settings_section);
    sidebar.append(&session_section);
    session_section.set_visible(false);

    let sidebar_scroll = ScrolledWindow::new();
    sidebar_scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    sidebar_scroll.set_child(Some(&sidebar));
    sidebar_scroll.add_css_class("sidebar");

    // ---- preview -------------------------------------------------------
    let picture = Picture::new();
    picture.set_content_fit(ContentFit::Contain);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);

    let placeholder = GtkBox::new(Orientation::Vertical, 12);
    placeholder.add_css_class("preview-placeholder");
    placeholder.set_halign(Align::Center);
    placeholder.set_valign(Align::Center);
    let placeholder_icon = Image::from_icon_name("video-display-symbolic");
    placeholder_icon.set_pixel_size(64);
    placeholder_icon.add_css_class("empty-icon");
    let placeholder_title = Label::new(Some("No Preview"));
    placeholder_title.add_css_class("title-2");
    let placeholder_hint = Label::new(Some("Select a device and press Connect to see its screen."));
    placeholder_hint.add_css_class("dim-label");
    placeholder_hint.set_wrap(true);
    placeholder_hint.set_justify(gtk4::Justification::Center);
    placeholder_hint.set_max_width_chars(36);
    let placeholder_connect = Button::with_label("Connect");
    placeholder_connect.add_css_class("suggested-action");
    placeholder_connect.set_halign(Align::Center);
    placeholder_connect.set_action_name(Some("win.connect"));
    placeholder.append(&placeholder_icon);
    placeholder.append(&placeholder_title);
    placeholder.append(&placeholder_hint);
    placeholder.append(&placeholder_connect);

    let hud_left = Label::new(Some(""));
    hud_left.add_css_class("osd");
    hud_left.add_css_class("osd-chip");
    hud_left.set_halign(Align::Start);
    hud_left.set_valign(Align::Start);
    hud_left.set_margin_top(12);
    hud_left.set_margin_start(12);
    hud_left.set_can_target(false);
    hud_left.set_visible(false);

    let hud_right = Label::new(Some(""));
    hud_right.add_css_class("osd");
    hud_right.add_css_class("osd-chip");
    hud_right.set_halign(Align::End);
    hud_right.set_valign(Align::Start);
    hud_right.set_margin_top(12);
    hud_right.set_margin_end(12);
    hud_right.set_can_target(false);
    hud_right.set_visible(false);

    let overlay = Overlay::new();
    overlay.set_child(Some(&picture));
    overlay.add_overlay(&placeholder);
    overlay.add_overlay(&hud_left);
    overlay.add_overlay(&hud_right);

    let preview = GtkBox::new(Orientation::Vertical, 0);
    preview.add_css_class("preview-area");
    preview.set_vexpand(true);
    preview.append(&overlay);

    // ---- log -----------------------------------------------------------
    let log_header = GtkBox::new(Orientation::Horizontal, 6);
    let log_title = Label::new(Some("Log"));
    log_title.add_css_class("caption-heading");
    log_title.set_xalign(0.0);
    log_title.set_hexpand(true);
    let clear_log_button = Button::from_icon_name("user-trash-symbolic");
    clear_log_button.set_tooltip_text(Some("Clear the log"));
    clear_log_button.add_css_class("flat");
    log_header.append(&log_title);
    log_header.append(&clear_log_button);

    let log_view = TextView::new();
    log_view.set_editable(false);
    log_view.set_cursor_visible(false);
    log_view.set_monospace(true);
    log_view.set_wrap_mode(WrapMode::WordChar);
    log_view.set_top_margin(6);
    log_view.set_bottom_margin(6);
    log_view.set_left_margin(8);
    log_view.set_right_margin(8);
    let log_buffer = log_view.buffer();

    let log_scroll = ScrolledWindow::new();
    log_scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    log_scroll.set_min_content_height(170);
    log_scroll.set_has_frame(true);
    log_scroll.set_child(Some(&log_view));

    let log_section = GtkBox::new(Orientation::Vertical, 6);
    log_section.append(&log_header);
    log_section.append(&log_scroll);

    let main_column = GtkBox::new(Orientation::Vertical, 18);
    main_column.set_margin_top(18);
    main_column.set_margin_bottom(18);
    main_column.set_margin_start(18);
    main_column.set_margin_end(18);
    main_column.append(&preview);
    main_column.append(&log_section);

    let paned = Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&sidebar_scroll));
    paned.set_end_child(Some(&main_column));
    paned.set_position(366);
    paned.set_resize_start_child(false);
    paned.set_shrink_start_child(false);
    window.set_child(Some(&paned));

    // ---- ui handle ------------------------------------------------------
    let ui = Ui {
        window: window.clone(),
        header_subtitle,
        spinner,
        connect_button,
        disconnect_button,
        appearance_button: appearance_button.clone(),
        dark: Rc::new(Cell::new(true)),
        device_list: device_list.clone(),
        device_stack: device_stack.clone(),
        devices: Rc::clone(&devices),
        mcp_switch: mcp_switch.clone(),
        mcp_subtitle,
        port_entry: port_entry.clone(),
        copy_url_row: copy_url_row.clone(),
        copy_dsh_row: copy_dsh_row.clone(),
        syncing_switch: Rc::new(Cell::new(false)),
        resolution,
        frame_age,
        foreground,
        battery,
        session_section: session_section.clone(),
        picture: picture.clone(),
        placeholder: placeholder.clone(),
        hud_left,
        hud_right,
        log_buffer: log_buffer.clone(),
        log_scroll: log_scroll.clone(),
    };

    // ---- actions --------------------------------------------------------
    install_actions(app, &state, &ui);

    // ---- device list ----------------------------------------------------
    device_list.connect_row_selected({
        let state = Arc::clone(&state);
        let ui = ui.clone();
        move |_, _| sync_controls(&state, &ui)
    });
    device_list.connect_row_activated({
        let state = Arc::clone(&state);
        let ui = ui.clone();
        move |_, _| connect_selected(&state, &ui)
    });

    // ---- MCP switch and rows --------------------------------------------
    mcp_switch.connect_state_set({
        let state = Arc::clone(&state);
        let ui = ui.clone();
        move |_, want| {
            if !ui.syncing_switch.get() {
                toggle_mcp(&state, &ui, want);
                sync_mcp(&state, &ui);
            }
            glib::Propagation::Stop
        }
    });
    mcp_list.connect_row_activated({
        let state = Arc::clone(&state);
        let ui = ui.clone();
        move |_, row| match row.index() {
            0 => ui.mcp_switch.set_active(!ui.mcp_switch.is_active()),
            2 => copy_server_url(&state, &ui),
            3 => copy_dsh_config(&state, &ui),
            _ => {}
        }
    });

    // ---- settings rows ---------------------------------------------------
    settings_list.connect_row_activated({
        let state = Arc::clone(&state);
        let manual_switch = manual_switch.clone();
        move |_, row| match row.index() {
            0 => manual_switch.set_active(!manual_switch.is_active()),
            1 => save_screenshot(&state),
            _ => {}
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

    clear_log_button.connect_clicked({
        let buffer = log_buffer.clone();
        move |_| buffer.set_text("")
    });

    // ---- manual input in the preview ------------------------------------
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
            let Some((sx, sy)) = map_to_frame(&picture, &state, start_x, start_y) else {
                return;
            };
            let Some((ex, ey)) =
                map_to_frame(&picture, &state, start_x + offset_x, start_y + offset_y)
            else {
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

    let scroll = gtk4::EventControllerScroll::new(
        gtk4::EventControllerScrollFlags::VERTICAL | gtk4::EventControllerScrollFlags::BOTH_AXES,
    );
    scroll.connect_scroll({
        let state = Arc::clone(&state);
        let picture = picture.clone();
        move |_, dx, dy| {
            if !state.manual_input_enabled() {
                return glib::Propagation::Proceed;
            }
            let Some((px, py)) = map_to_frame(&picture, &state, 0.0, 0.0) else {
                return glib::Propagation::Proceed;
            };
            if let Some(session) = state.session() {
                let settle = state.cfg.lock().unwrap().settle_ms;
                let dx = dx as f32 * 3.0;
                let dy = dy as f32 * 3.0;
                std::thread::spawn(move || {
                    let _ = session.scroll(px, py, dx, dy, settle);
                });
            }
            glib::Propagation::Stop
        }
    });
    picture.add_controller(scroll);

    // ---- lifecycle ------------------------------------------------------
    window.connect_close_request({
        let state = Arc::clone(&state);
        move |_| {
            state.close_session();
            state.stop_mcp();
            glib::Propagation::Proceed
        }
    });

    refresh_devices(&state, &ui);
    sync_mcp(&state, &ui);
    update_session(&state, &ui);
    append_log(
        &log_buffer,
        Instant::now(),
        "Ready. Select a device, press Connect, then start the MCP server.",
    );
    window.present();

    start_pump(state, ui, rx);
}

fn install_actions(app: &Application, state: &Arc<AppState>, ui: &Ui) {
    let refresh = gio::SimpleAction::new("refresh", None);
    refresh.connect_activate({
        let state = Arc::clone(state);
        let ui = ui.clone();
        move |_, _| refresh_devices(&state, &ui)
    });
    ui.window.add_action(&refresh);

    let connect = gio::SimpleAction::new("connect", None);
    connect.connect_activate({
        let state = Arc::clone(state);
        let ui = ui.clone();
        move |_, _| connect_selected(&state, &ui)
    });
    ui.window.add_action(&connect);

    let disconnect = gio::SimpleAction::new("disconnect", None);
    disconnect.connect_activate({
        let state = Arc::clone(state);
        move |_, _| state.close_session()
    });
    ui.window.add_action(&disconnect);

    let toggle = gio::SimpleAction::new("toggle-mcp", None);
    toggle.connect_activate({
        let ui = ui.clone();
        move |_, _| ui.mcp_switch.set_active(!ui.mcp_switch.is_active())
    });
    ui.window.add_action(&toggle);

    let save = gio::SimpleAction::new("save-frame", None);
    save.connect_activate({
        let state = Arc::clone(state);
        move |_, _| save_screenshot(&state)
    });
    ui.window.add_action(&save);

    let appearance = gio::SimpleAction::new("toggle-appearance", None);
    appearance.connect_activate({
        let ui = ui.clone();
        move |_, _| {
            let dark = !ui.dark.get();
            ui.dark.set(dark);
            set_appearance(dark);
            sync_appearance_button(&ui.appearance_button, dark);
        }
    });
    ui.window.add_action(&appearance);

    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    shortcuts.connect_activate({
        let window = ui.window.clone();
        move |_, _| show_shortcuts(&window)
    });
    ui.window.add_action(&shortcuts);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate({
        let window = ui.window.clone();
        move |_, _| show_about(&window)
    });
    app.add_action(&about);

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate({
        let app = app.clone();
        move |_, _| app.quit()
    });
    app.add_action(&quit);

    app.set_accels_for_action("win.refresh", &["<Control>r"]);
    app.set_accels_for_action("win.connect", &["<Control>Return"]);
    app.set_accels_for_action("win.toggle-mcp", &["<Control>m"]);
    app.set_accels_for_action("win.save-frame", &["<Control>s"]);
    app.set_accels_for_action("win.shortcuts", &["<Control>question"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

fn refresh_devices(state: &Arc<AppState>, ui: &Ui) {
    while let Some(child) = ui.device_list.first_child() {
        ui.device_list.remove(&child);
    }
    match state.adb.devices() {
        Ok(devices) => {
            for device in &devices {
                let content = GtkBox::new(Orientation::Horizontal, 12);
                let lead = icon("phone-symbolic");
                lead.add_css_class("dim-label");
                content.append(&lead);
                let model = device
                    .model
                    .clone()
                    .unwrap_or_else(|| device.serial.clone());
                content.append(&text_column(&model, Some(&device.serial)));

                let (text, class) = if device.state.is_usable() {
                    ("Ready", "state-ready")
                } else {
                    (device.state.as_str(), "state-warning")
                };
                let state_label = Label::new(Some(&capitalize(text)));
                state_label.add_css_class("caption");
                state_label.add_css_class(class);
                state_label.set_valign(Align::Center);
                content.append(&state_label);
                ui.device_list.append(&row(&content));
            }
            if devices.is_empty() {
                ui.device_stack.set_visible_child_name("empty");
            } else {
                ui.device_stack.set_visible_child_name("list");
                let first_usable = devices.iter().position(|device| device.state.is_usable());
                ui.device_list.select_row(
                    ui.device_list
                        .row_at_index(first_usable.unwrap_or(0) as i32)
                        .as_ref(),
                );
            }
            *ui.devices.borrow_mut() = devices;
        }
        Err(err) => {
            state.log(format!("adb error: {err:#}"));
            ui.device_stack.set_visible_child_name("empty");
            *ui.devices.borrow_mut() = Vec::new();
        }
    }
    sync_controls(state, ui);
}

fn sync_controls(state: &Arc<AppState>, ui: &Ui) {
    let connected = state.session().is_some();
    let selected = selected_device(ui);
    let usable = selected
        .map(|device| device.state.is_usable())
        .unwrap_or(false);
    ui.connect_button.set_visible(!connected);
    ui.disconnect_button.set_visible(connected);
    set_action_enabled(ui, "connect", !connected && usable);
    set_action_enabled(ui, "disconnect", connected);
    ui.placeholder.set_visible(!connected);
}

fn connect_selected(state: &Arc<AppState>, ui: &Ui) {
    if state.session().is_some() {
        return;
    }
    let Some(device) = selected_device(ui) else {
        state.log("Select a device first");
        return;
    };
    if !device.state.is_usable() {
        state.log(format!(
            "{} is {}. Accept the USB debugging prompt on the device.",
            device.serial,
            device.state.as_str()
        ));
        return;
    }
    ui.spinner.set_visible(true);
    ui.spinner.start();
    set_action_enabled(ui, "connect", false);
    state.log(format!("Connecting: {}", device.serial));
    let state = Arc::clone(state);
    let serial = device.serial;
    std::thread::spawn(move || {
        if let Err(err) = state.open_session(&serial) {
            state.log(format!("Connect failed: {err:#}"));
            state.emit(UiEvent::SessionChanged);
        }
    });
}

fn toggle_mcp(state: &Arc<AppState>, ui: &Ui, want: bool) {
    if want {
        match ui.port_entry.text().trim().parse::<u16>() {
            Ok(port) => state.cfg.lock().unwrap().mcp_port = port,
            Err(_) => {
                state.log("Invalid port; use a number between 1024 and 65535");
                return;
            }
        }
        match state.start_mcp() {
            Ok(url) => state.log(format!("MCP server started: {url}")),
            Err(err) => state.log(format!("MCP start failed: {err:#}")),
        }
    } else if state.mcp_url().is_some() {
        state.stop_mcp();
        state.log("MCP server stopped");
    }
}

fn sync_mcp(state: &Arc<AppState>, ui: &Ui) {
    let url = state.mcp_url();
    let running = url.is_some();
    ui.syncing_switch.set(true);
    ui.mcp_switch.set_active(running);
    ui.mcp_switch.set_state(running);
    ui.syncing_switch.set(false);
    ui.mcp_subtitle
        .set_text(url.as_deref().unwrap_or("Not running"));
    ui.port_entry.set_sensitive(!running);
    ui.copy_url_row.set_sensitive(running);
    ui.copy_dsh_row.set_sensitive(running);
    set_action_enabled(ui, "toggle-mcp", true);
}

fn copy_server_url(state: &Arc<AppState>, ui: &Ui) {
    match state.mcp_url() {
        Some(url) => {
            ui.window.clipboard().set_text(&url);
            state.log(format!("Copied: {url}"));
        }
        None => state.log("The MCP server is not running"),
    }
}

fn copy_dsh_config(state: &Arc<AppState>, ui: &Ui) {
    let port = state.cfg.lock().unwrap().mcp_port;
    let snippet = format!(
        "- insert:\n    - id: mcp-android\n      name: \"@deepseek-ai/dsh-mcp-client\"\n      config:\n        serverName: android\n        transport: streamable-http\n        url: http://127.0.0.1:{port}/mcp\n"
    );
    ui.window.clipboard().set_text(&snippet);
    state.log("Copied the DSH config snippet");
}

fn update_session(state: &Arc<AppState>, ui: &Ui) {
    match state.session() {
        Some(session) => {
            let status = session.status();
            let dims = status
                .dims
                .map(|dims| format!("{}×{}", dims.width, dims.height));
            ui.resolution.set_text(dims.as_deref().unwrap_or("—"));
            ui.frame_age.set_text(
                &status
                    .frame_age_ms
                    .map(|age| format!("{age} ms ago"))
                    .unwrap_or_else(|| "—".to_string()),
            );
            ui.header_subtitle.set_text(&match &dims {
                Some(dims) => format!("{} · {dims}", status.device_name),
                None => status.device_name.clone(),
            });
            ui.hud_left.set_text(&match &dims {
                Some(dims) => format!("{}  {dims}", status.device_name),
                None => status.device_name.clone(),
            });
            ui.hud_right
                .set_text(&format!("{} · {}s", status.state, status.uptime_s));
            ui.hud_left.set_visible(true);
            ui.hud_right.set_visible(true);
            ui.session_section.set_visible(true);
            ui.placeholder.set_visible(false);
        }
        None => {
            ui.header_subtitle.set_text("No device connected");
            ui.resolution.set_text("—");
            ui.frame_age.set_text("—");
            ui.foreground.set_text("—");
            ui.battery.set_text("—");
            ui.hud_left.set_visible(false);
            ui.hud_right.set_visible(false);
            ui.session_section.set_visible(false);
            ui.placeholder.set_visible(true);
        }
    }
}

fn start_pump(state: Arc<AppState>, ui: Ui, rx: mpsc::Receiver<UiEvent>) {
    let started = Instant::now();
    let rx = Rc::new(RefCell::new(rx));
    let last_seq = Rc::new(Cell::new(0u64));
    let last_stats = Rc::new(Cell::new(Instant::now()));
    let last_labels = Rc::new(Cell::new(Instant::now()));
    let spinner_since: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));

    glib::timeout_add_local(Duration::from_millis(16), move || {
        while let Ok(event) = rx.borrow().try_recv() {
            match event {
                UiEvent::Log(line) => {
                    append_log(&ui.log_buffer, started, &line);
                    scroll_log(&ui.log_scroll);
                }
                UiEvent::SessionChanged => {
                    ui.spinner.stop();
                    ui.spinner.set_visible(false);
                    sync_controls(&state, &ui);
                    update_session(&state, &ui);
                }
                UiEvent::McpChanged => sync_mcp(&state, &ui),
                UiEvent::ToolCall { name, ok, detail } => {
                    let mark = if ok { "ok" } else { "error" };
                    append_log(
                        &ui.log_buffer,
                        started,
                        &format!("[tool] {name} {mark}: {detail}"),
                    );
                    scroll_log(&ui.log_scroll);
                }
                UiEvent::Stats {
                    battery,
                    charging,
                    app,
                } => {
                    ui.battery.set_text(&match battery {
                        Some(level) if charging => format!("{level}% (charging)"),
                        Some(level) => format!("{level}%"),
                        None => "—".to_string(),
                    });
                    ui.foreground.set_text(app.as_deref().unwrap_or("—"));
                }
            }
        }

        if ui.spinner.is_visible() {
            match spinner_since.get() {
                None => spinner_since.set(Some(Instant::now())),
                Some(since) if since.elapsed() > Duration::from_secs(20) => {
                    ui.spinner.stop();
                    ui.spinner.set_visible(false);
                    spinner_since.set(None);
                    sync_controls(&state, &ui);
                }
                Some(_) => {}
            }
        } else {
            spinner_since.set(None);
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
                    ui.picture.set_paintable(Some(&texture));
                }
            }
        }

        if last_labels.get().elapsed() >= Duration::from_secs(1) {
            last_labels.set(Instant::now());
            update_session(&state, &ui);
        }
        if last_stats.get().elapsed() >= Duration::from_secs(5) {
            last_stats.set(Instant::now());
            state.refresh_stats();
        }

        glib::ControlFlow::Continue
    });
}

fn show_about(window: &ApplicationWindow) {
    let dialog = AboutDialog::builder()
        .modal(true)
        .transient_for(window)
        .program_name("scrcpy-agent")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Android device control for agents, backed by scrcpy.")
        .website("https://github.com/neutron42-jp/scrcpy-agent")
        .license_type(License::MitX11)
        .build();
    dialog.present();
}

fn show_shortcuts(parent: &ApplicationWindow) {
    // GtkShortcutsWindow is deprecated since 4.18 in favour of libadwaita, so
    // this uses the same boxed-list rows as the main window.
    let window = Window::builder()
        .modal(true)
        .transient_for(parent)
        .title("Keyboard Shortcuts")
        .default_width(400)
        .build();

    let header = HeaderBar::new();
    let title = Label::new(Some("Keyboard Shortcuts"));
    title.add_css_class("heading");
    header.set_title_widget(Some(&title));
    window.set_titlebar(Some(&header));

    let content = GtkBox::new(Orientation::Vertical, 18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let groups: &[(&str, &[(&str, &str)])] = &[
        (
            "Session",
            &[
                ("Refresh devices", "Ctrl+R"),
                ("Connect the selected device", "Ctrl+Enter"),
                ("Start or stop the MCP server", "Ctrl+M"),
                ("Save the current frame", "Ctrl+S"),
            ],
        ),
        (
            "Application",
            &[("Keyboard shortcuts", "Ctrl+?"), ("Quit", "Ctrl+Q")],
        ),
    ];

    for &(heading, shortcuts) in groups {
        let group = section(heading);
        let list = boxed_list();
        for &(name, accelerator) in shortcuts {
            let row_box = GtkBox::new(Orientation::Horizontal, 12);
            row_box.append(&text_column(name, None));
            let hint = Label::new(Some(accelerator));
            hint.add_css_class("monospace");
            hint.add_css_class("dim-label");
            hint.set_valign(Align::Center);
            row_box.append(&hint);
            let shortcut_row = row(&row_box);
            shortcut_row.set_activatable(false);
            list.append(&shortcut_row);
        }
        group.append(&list);
        content.append(&group);
    }

    let scroll = ScrolledWindow::new();
    scroll.set_policy(PolicyType::Never, PolicyType::Automatic);
    scroll.set_child(Some(&content));
    window.set_child(Some(&scroll));
    window.present();
}

fn append_log(buffer: &TextBuffer, started: Instant, line: &str) {
    let seconds = started.elapsed().as_secs();
    let stamp = format!("[{:02}:{:02}] ", seconds / 60, seconds % 60);
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, &format!("{stamp}{line}\n"));
    if buffer.line_count() > 3000 {
        let mut start = buffer.start_iter();
        let mut cut = buffer.start_iter();
        cut.forward_lines(1000);
        buffer.delete(&mut start, &mut cut);
    }
}

fn scroll_log(scroll: &ScrolledWindow) {
    let adjustment = scroll.vadjustment();
    adjustment.set_value(adjustment.upper());
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn save_screenshot(state: &Arc<AppState>) {
    let Some(session) = state.session() else {
        state.log("No active session");
        return;
    };
    let Some(frame) = session.frames().latest() else {
        state.log("No frame received yet");
        return;
    };
    match crate::video::encode_png(&frame) {
        Ok(png) => {
            let dir = dirs::picture_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or(0);
            let path = dir.join(format!("scrcpy-agent-{stamp}.png"));
            match std::fs::write(&path, png) {
                Ok(()) => state.log(format!("Saved: {}", path.display())),
                Err(err) => state.log(format!("Save failed: {err}")),
            }
        }
        Err(err) => state.log(format!("PNG encoding failed: {err:#}")),
    }
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
