//! GTK shell. Rc shares UI state on the main thread; RefCell permits updates.
use crate::{
    clipboard,
    config::{self, Config, Mode, Theme},
    history::{self, Content, History},
    platform, preferences, search,
};
use anyhow::{Context, Result};
use gtk::{gdk, gio, glib, pango, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

const APP_ID: &str = "io.github.tope.QuickLauncher";

#[derive(Clone)]
enum Action {
    App(gio::AppInfo),
    Clipboard(Content),
}

impl Action {
    fn same_as(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::App(a), Self::App(b)) => a.equal(b),
            (Self::Clipboard(a), Self::Clipboard(b)) => a == b,
            _ => false,
        }
    }
}

struct LauncherState {
    mode: Mode,
    query: String,
    cursor: i32,
    selection: Option<(i32, i32)>,
    selected: Option<Action>,
    selected_is_recent: bool,
    selection_was_visible: bool,
    scroll: f64,
}

pub struct Ui {
    pub config: RefCell<Config>,
    pub history: RefCell<History>,
    pub window: gtk::ApplicationWindow,
    pub preferences: RefCell<Option<gtk::Window>>,
    preferences_origin: RefCell<Option<LauncherState>>,
    entry: gtk::SearchEntry,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    hint: gtk::Label,
    app_button: gtk::ToggleButton,
    clip_button: gtk::ToggleButton,
    summary: gtk::Label,
    mode: Cell<Mode>,
    composing: Cell<bool>,
    actions: RefCell<Vec<Action>>,
    recent_row_count: Cell<usize>,
    apps: RefCell<Vec<gio::AppInfo>>,
    app_icons: RefCell<HashMap<(PathBuf, i32), gdk::Texture>>,
    icon_theme: gtk::IconTheme,
    generation: Cell<u64>,
    save_pending: Cell<bool>,
    pending_read: RefCell<Option<gio::Cancellable>>,
    _app_monitor: gio::AppInfoMonitor,
    _theme_settings: gio::Settings,
    _hold: gio::ApplicationHoldGuard,
}

pub fn run() -> glib::ExitCode {
    if std::env::args().any(|arg| arg == "--version") {
        println!("quick-launcher {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    if std::env::args().any(|arg| arg == "--check") {
        let result = Config::load(&config::config_path());
        println!("配置有效：{}", result.is_ok());
        println!("GNOME 快捷键接口：{}", platform::available());
        println!("已安装桌面绑定：{}", platform::installed());
        println!("配置路径：{}", config::config_path().display());
        return if result.is_ok() {
            glib::ExitCode::SUCCESS
        } else {
            glib::ExitCode::FAILURE
        };
    }
    glib::set_prgname(Some("quick-launcher"));
    glib::set_application_name("Quick Launcher");
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    for (name, description) in [
        ("background", "在后台记录剪贴板"),
        ("toggle", "显示或收起窗口"),
        ("preferences", "打开偏好设置"),
        ("quit", "退出运行中的实例"),
    ] {
        app.add_main_option(
            name,
            glib::Char(0),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            description,
            None,
        );
    }
    let state: Rc<RefCell<Option<Rc<Ui>>>> = Rc::new(RefCell::new(None));
    let shared = state.clone();
    app.connect_startup(move |app| match Ui::new(app) {
        Ok(ui) => *shared.borrow_mut() = Some(ui),
        Err(error) => {
            eprintln!("启动失败：{error}");
            app.quit();
        }
    });
    let shared = state.clone();
    app.connect_command_line(move |_app, command| {
        let Some(ui) = shared.borrow().clone() else {
            return glib::ExitCode::FAILURE;
        };
        let options = command.options_dict();
        if options.contains("quit") {
            ui.window.application().unwrap().quit();
        } else if options.contains("preferences") {
            preferences::show(&ui);
        } else if !options.contains("background") {
            ui.toggle();
        }
        glib::ExitCode::SUCCESS
    });
    let shared = state;
    app.connect_shutdown(move |_| {
        if let Some(ui) = shared.borrow().as_ref() {
            let persist = ui.config.borrow().persist_history;
            let _ = ui.history.borrow().save(persist);
            if !persist {
                let _ = ui.history.borrow_mut().clear();
            }
        }
    });
    app.run()
}

impl Ui {
    fn new(app: &gtk::Application) -> Result<Rc<Self>> {
        let config = Config::load(&config::config_path())?;
        let history = History::open(
            config::state_dir(),
            config.persist_history,
            config.history_limit,
        )?;
        history.save(config.persist_history)?;
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Quick Launcher")
            .default_width(620)
            .decorated(false)
            .resizable(false)
            .build();
        window.add_css_class("quick-launcher");
        let settings = gtk::Button::from_icon_name("emblem-system-symbolic");
        settings.set_tooltip_text(Some("偏好设置 · Ctrl+,"));
        settings.add_css_class("flat");
        settings.add_css_class("launcher-settings");
        settings.set_valign(gtk::Align::Center);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
        body.add_css_class("launcher-panel");
        let search_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let entry = gtk::SearchEntry::new();
        entry.set_placeholder_text(Some("搜索应用…"));
        entry.set_hexpand(true);
        entry.add_css_class("launcher-search");
        search_bar.append(&entry);
        search_bar.append(&settings);
        body.append(&search_bar);
        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let app_button = gtk::ToggleButton::with_label("应用");
        let clip_button = gtk::ToggleButton::with_label("剪贴板");
        for button in [&app_button, &clip_button] {
            button.add_css_class("flat");
            button.add_css_class("launcher-tab");
        }
        tabs.append(&app_button);
        tabs.append(&clip_button);
        let summary = gtk::Label::new(None);
        summary.set_hexpand(true);
        summary.set_xalign(1.0);
        summary.add_css_class("launcher-summary");
        tabs.append(&summary);
        body.append(&tabs);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.add_css_class("launcher-results");
        let scroll = gtk::ScrolledWindow::builder()
            .propagate_natural_height(true)
            .min_content_height(68)
            .max_content_height(340)
            .overlay_scrolling(false)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .build();
        body.append(&scroll);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let hint = gtk::Label::new(Some("↑↓ 选择    Enter 打开    Tab 切换    Esc 收起"));
        hint.set_xalign(0.0);
        hint.add_css_class("launcher-hint");
        hint.set_wrap(true);
        body.append(&hint);
        window.set_child(Some(&body));
        let css = gtk::CssProvider::new();
        css.load_from_data(include_str!("style.css"));
        gtk::style_context_add_provider_for_display(
            &gdk::Display::default().context("无法连接桌面显示")?,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let icon_theme = gtk::IconTheme::for_display(&WidgetExt::display(&window));
        let ui = Rc::new(Self {
            config: RefCell::new(config),
            history: RefCell::new(history),
            window,
            preferences: RefCell::new(None),
            preferences_origin: RefCell::new(None),
            entry,
            list,
            scroll,
            hint,
            app_button,
            clip_button,
            summary,
            mode: Cell::new(Mode::Apps),
            composing: Cell::new(false),
            actions: RefCell::new(vec![]),
            recent_row_count: Cell::new(0),
            apps: RefCell::new(vec![]),
            app_icons: RefCell::new(HashMap::new()),
            icon_theme,
            generation: Cell::new(0),
            save_pending: Cell::new(false),
            pending_read: RefCell::new(None),
            _app_monitor: gio::AppInfoMonitor::get(),
            _theme_settings: gio::Settings::new("org.gnome.desktop.interface"),
            _hold: app.hold(),
        });
        ui.apply_theme();
        ui.reload_apps();

        let shared = ui.clone();
        ui.entry.connect_changed(move |_| shared.render());
        if let Some(text) = ui.entry.delegate().and_downcast::<gtk::Text>() {
            let weak = Rc::downgrade(&ui);
            text.connect_preedit_changed(move |_, preedit| {
                if let Some(ui) = weak.upgrade() {
                    ui.composing.set(!preedit.is_empty());
                }
            });
        }
        let shared = ui.clone();
        ui.app_button
            .connect_clicked(move |_| shared.set_mode(Mode::Apps, true));
        let shared = ui.clone();
        ui.clip_button
            .connect_clicked(move |_| shared.set_mode(Mode::Clipboard, true));
        let shared = ui.clone();
        settings.connect_clicked(move |_| preferences::show(&shared));
        let shared = ui.clone();
        ui.list
            .connect_row_activated(move |_, row| shared.activate(row.index() as usize));
        let shared = ui.clone();
        ui.window.connect_close_request(move |_| {
            shared.window.hide();
            glib::Propagation::Stop
        });
        let controller = gtk::EventControllerKey::new();
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        let shared = ui.clone();
        controller.connect_key_pressed(move |_, key, _, modifiers| {
            // Candidate selection and confirmation belong to the input method.
            if shared.composing.get() {
                return glib::Propagation::Proceed;
            }
            if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == gdk::Key::comma {
                preferences::show(&shared);
                return glib::Propagation::Stop;
            }
            match key {
                gdk::Key::Escape => shared.window.hide(),
                gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                    let mode = if shared.effective_mode() == Mode::Apps {
                        Mode::Clipboard
                    } else {
                        Mode::Apps
                    };
                    shared.set_mode(mode, true);
                }
                gdk::Key::Left | gdk::Key::Right
                    if !modifiers.intersects(
                        gdk::ModifierType::CONTROL_MASK
                            | gdk::ModifierType::ALT_MASK
                            | gdk::ModifierType::SUPER_MASK
                            | gdk::ModifierType::SHIFT_MASK,
                    ) =>
                {
                    let mode = if key == gdk::Key::Left {
                        Mode::Apps
                    } else {
                        Mode::Clipboard
                    };
                    if shared.effective_mode() != mode {
                        shared.set_mode(mode, true);
                    }
                }
                gdk::Key::Down | gdk::Key::Up => {
                    shared.navigate(if key == gdk::Key::Down { 1 } else { -1 })
                }
                gdk::Key::Return | gdk::Key::KP_Enter => {
                    if let Some(row) = shared.list.selected_row() {
                        shared.activate(row.index() as usize);
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        ui.window.add_controller(controller);
        let weak = Rc::downgrade(&ui);
        ui._app_monitor.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.reload_apps();
                if ui.window.is_visible() {
                    ui.render();
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        ui._theme_settings
            .connect_changed(Some("color-scheme"), move |_, _| {
                if let Some(ui) = weak.upgrade() {
                    ui.apply_theme();
                }
            });
        let weak = Rc::downgrade(&ui);
        ui.icon_theme.connect_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.app_icons.borrow_mut().clear();
                if ui.window.is_visible() {
                    ui.render();
                }
            }
        });
        let clipboard = gdk::Display::default().unwrap().clipboard();
        let weak = Rc::downgrade(&ui);
        clipboard.connect_changed(move |clipboard| {
            if let Some(ui) = weak.upgrade() {
                ui.read_clipboard(clipboard);
            }
        });
        ui.read_clipboard(&clipboard);
        ui.set_mode(ui.config.borrow().default_mode, false);
        Ok(ui)
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.window.is_visible() && self.window.is_active() {
            self.window.hide();
            return;
        }
        if !self.window.is_visible() {
            self.entry.set_text("");
            self.set_mode(self.config.borrow().default_mode, false);
        }
        self.present_launcher();
    }

    fn present_launcher(&self) {
        self.window.present();
        self.entry.grab_focus();
        glib::idle_add_local_once(|| platform::focus_window("Quick Launcher"));
    }

    pub fn leave_for_preferences(&self) {
        let index = self.list.selected_row().map(|row| row.index() as usize);
        let actions = self.actions.borrow();
        let selected = index.and_then(|index| actions.get(index)).cloned();
        let adjustment = self.scroll.vadjustment();
        let selection_was_visible = self
            .list
            .selected_row()
            .and_then(|row| row.compute_bounds(&self.list))
            .is_some_and(|bounds| {
                bounds.y() as f64 >= adjustment.value()
                    && (bounds.y() + bounds.height()) as f64
                        <= adjustment.value() + adjustment.page_size()
            });
        *self.preferences_origin.borrow_mut() = Some(LauncherState {
            mode: self.mode.get(),
            query: self.entry.text().to_string(),
            cursor: self.entry.position(),
            selection: self.entry.selection_bounds(),
            selected,
            selected_is_recent: index.is_some_and(|index| index < self.recent_row_count.get()),
            selection_was_visible,
            scroll: self.scroll.vadjustment().value(),
        });
        self.window.hide();
    }

    pub fn return_from_preferences(self: &Rc<Self>) {
        let origin = self.preferences_origin.borrow_mut().take();
        if let Some(origin) = origin {
            self.mode.set(origin.mode);
            self.entry.set_text(&origin.query);
            self.set_mode(origin.mode, false);
            if let Some(selected) = origin.selected {
                let actions = self.actions.borrow();
                let matches: Vec<_> = actions
                    .iter()
                    .enumerate()
                    .filter(|(_, action)| action.same_as(&selected))
                    .map(|(index, _)| index)
                    .collect();
                let index = matches
                    .iter()
                    .copied()
                    .find(|index| {
                        (*index < self.recent_row_count.get()) == origin.selected_is_recent
                    })
                    .or_else(|| matches.first().copied());
                if let Some(index) = index {
                    self.list
                        .select_row(self.list.row_at_index(index as i32).as_ref());
                }
            }
            self.present_launcher();
            if let Some((start, end)) = origin.selection {
                self.entry.select_region(start, end);
            } else {
                self.entry.set_position(origin.cursor);
            }
            let weak = Rc::downgrade(self);
            glib::timeout_add_local_once(Duration::from_millis(50), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.scroll.vadjustment().set_value(origin.scroll);
                    if origin.selection_was_visible {
                        ui.scroll_selection_into_view();
                    }
                }
            });
        } else {
            self.present_launcher();
        }
    }

    pub fn apply_theme(&self) {
        let dark = match self.config.borrow().theme {
            Theme::System => platform::system_dark(),
            Theme::Light => false,
            Theme::Dark => true,
        };
        if let Some(settings) = gtk::Settings::default() {
            settings.set_gtk_application_prefer_dark_theme(dark);
        }
    }

    fn reload_apps(&self) {
        let mut apps: Vec<_> = gio::AppInfo::all()
            .into_iter()
            .filter(|app| app.should_show())
            .collect();
        apps.sort_by_key(|app| app.display_name().to_lowercase());
        *self.apps.borrow_mut() = apps;
        self.app_icons.borrow_mut().clear();
    }

    fn effective_mode(&self) -> Mode {
        if self.entry.text().starts_with("clip ") {
            Mode::Clipboard
        } else {
            self.mode.get()
        }
    }

    fn set_mode(&self, mode: Mode, clear: bool) {
        self.mode.set(mode);
        if clear {
            self.entry.set_text("");
        }
        self.entry.set_placeholder_text(Some(if mode == Mode::Apps {
            "搜索应用…"
        } else {
            "搜索剪贴板历史…"
        }));
        self.render();
        self.entry.grab_focus();
    }

    pub fn render(&self) {
        self.list.unset_header_func();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let text = self.entry.text();
        let mode = self.effective_mode();
        let query = if mode == Mode::Clipboard {
            text.strip_prefix("clip ").unwrap_or(&text)
        } else {
            &text
        };
        self.app_button.set_active(mode == Mode::Apps);
        self.clip_button.set_active(mode == Mode::Clipboard);
        let config = self.config.borrow();
        let mut actions = vec![];
        self.recent_row_count.set(0);
        if mode == Mode::Apps {
            let mut matches = vec![];
            for app in self.apps.borrow().iter() {
                let extra = format!(
                    "{} {}",
                    app.id().unwrap_or_default(),
                    app.executable().display()
                );
                if let Some(rank) = search::score(query, &app.display_name(), &extra) {
                    matches.push((rank, app.clone()));
                }
            }
            matches.sort_by_key(|item| std::cmp::Reverse(item.0));
            let ids: Vec<_> = matches
                .iter()
                .map(|(_, app)| app.id().map(|id| id.to_string()))
                .collect();
            let recent =
                search::recent_indices(&config.recent_apps, &ids, config.recent_apps_limit);
            let recent_count = recent.len();
            self.recent_row_count.set(recent_count);
            let all_count = matches.len();
            let show_empty_recent =
                config.recent_apps_limit > 0 && query.trim().is_empty() && recent.is_empty();
            let all_heading = if query.trim().is_empty() {
                "全部应用"
            } else {
                "匹配应用"
            };
            self.list.set_header_func(move |row, _| {
                if row.index() == 0 && recent_count > 0 {
                    row.set_header(Some(&Self::section_heading(&format!(
                        "最近使用 · {recent_count}"
                    ))));
                } else if row.index() == recent_count as i32 {
                    let header = gtk::Box::new(gtk::Orientation::Vertical, 4);
                    if show_empty_recent {
                        header.append(&Self::section_heading("最近使用"));
                        let empty =
                            gtk::Label::new(Some("暂无记录，通过启动器打开应用后会显示在这里。"));
                        empty.set_xalign(0.0);
                        empty.add_css_class("launcher-recent-empty");
                        header.append(&empty);
                    }
                    header.append(&Self::section_heading(&format!(
                        "{all_heading} · {all_count}"
                    )));
                    row.set_header(Some(&header));
                } else {
                    row.set_header(None::<&gtk::Widget>);
                }
            });
            for index in recent {
                let app = &matches[index].1;
                self.append_app(app);
                actions.push(Action::App(app.clone()));
            }
            for (_, app) in matches {
                self.append_app(&app);
                actions.push(Action::App(app));
            }
            self.summary.set_text(&format!("{all_count} 个应用"));
            self.hint
                .set_text("↑↓ 选择   Enter 打开   ←→ / Tab 切换   Ctrl+, 设置   Esc 收起");
        } else {
            for content in self
                .history
                .borrow()
                .entries
                .iter()
                .filter(|entry| entry.matches(query))
                .take(config.max_results)
            {
                let icon =
                    gtk::Image::from_icon_name(if matches!(content, Content::Image { .. }) {
                        "insert-image-symbolic"
                    } else {
                        "edit-paste-symbolic"
                    });
                self.append_row(
                    &content.preview(),
                    if matches!(content, Content::Image { .. }) {
                        "回车复制图片，然后 Ctrl+V 粘贴"
                    } else {
                        "回车复制原文，然后 Ctrl+V 粘贴"
                    },
                    icon,
                );
                actions.push(Action::Clipboard(content.clone()));
            }
            self.hint.set_text(if config.recording {
                "↑↓ 选择   Enter 复制   ←→ / Tab 切换   Ctrl+, 设置   Esc 收起"
            } else {
                "记录已暂停 · ↑↓ 选择   Enter 复制   ←→ / Tab 切换   Ctrl+, 设置"
            });
            self.summary.set_text(&format!("{} 项结果", actions.len()));
        }
        if actions.is_empty() {
            self.append_row(
                if !query.trim().is_empty() {
                    "没有匹配的结果"
                } else if mode == Mode::Clipboard {
                    "暂无剪贴板历史"
                } else {
                    "暂无可用应用"
                },
                if mode == Mode::Clipboard && !config.recording {
                    "记录已暂停，可在设置中恢复记录或导入旧文本历史。"
                } else if mode == Mode::Clipboard {
                    "复制文字或图片后会出现记录，也可在设置中导入旧文本历史。"
                } else {
                    "试试应用名称、英文名或程序名称。"
                },
                gtk::Image::from_icon_name("system-search-symbolic"),
            );
            if let Some(row) = self.list.row_at_index(0) {
                row.set_selectable(false);
                row.set_activatable(false);
            }
        }
        *self.actions.borrow_mut() = actions;
        if let Some(row) = self.list.row_at_index(0) {
            if row.is_selectable() {
                self.list.select_row(Some(&row));
            }
        }
        // GTK keeps the mapped toplevel's previous size. Request the new
        // natural height explicitly so shorter searches also shrink it.
        if let Some(body) = self.window.child() {
            let (_, height, _, _) = body.measure(gtk::Orientation::Vertical, 620);
            self.window.set_default_size(620, height);
        }
    }

    fn section_heading(title: &str) -> gtk::Label {
        let label = gtk::Label::new(Some(title));
        label.set_xalign(0.0);
        label.add_css_class("launcher-section");
        label
    }

    fn append_app(&self, app: &gio::AppInfo) {
        let icon = gtk::Image::new();
        if let Some(gicon) = app.icon() {
            let scale = self.window.scale_factor().max(1);
            let paintable = self.icon_theme.lookup_by_gicon(
                &gicon,
                26,
                scale,
                self.window.direction(),
                gtk::IconLookupFlags::empty(),
            );
            let path = if paintable.is_symbolic() {
                None // GTK recolors symbolic icons using the current theme.
            } else {
                paintable.file().and_then(|file| file.path())
            };
            if let Some(path) = path {
                // File and themed application icons may contain large source
                // images. Decode at row size and share across both sections.
                let key = (path.clone(), scale);
                let cached = self.app_icons.borrow().get(&key).cloned();
                let texture = cached.or_else(|| {
                    let pixbuf = gtk::gdk_pixbuf::Pixbuf::from_file_at_scale(
                        &path,
                        26 * scale,
                        26 * scale,
                        true,
                    )
                    .ok()?;
                    let texture = gdk::Texture::for_pixbuf(&pixbuf);
                    self.app_icons.borrow_mut().insert(key, texture.clone());
                    Some(texture)
                });
                if let Some(texture) = texture {
                    icon.set_paintable(Some(&texture));
                } else {
                    icon.set_icon_name(Some("application-x-executable-symbolic"));
                }
            } else {
                icon.set_from_gicon(&gicon);
            }
        } else {
            icon.set_icon_name(Some("application-x-executable-symbolic"));
        }
        self.append_row(
            &app.display_name(),
            app.description().as_deref().unwrap_or("回车打开应用"),
            icon,
        );
    }

    fn append_row(&self, name: &str, description: &str, icon: gtk::Image) {
        icon.set_pixel_size(26);
        let row = gtk::ListBoxRow::new();
        row.set_focusable(false);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        body.add_css_class("launcher-row");
        body.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
        text.set_hexpand(true);
        for (value, class) in [
            (name, "launcher-name"),
            (description, "launcher-description"),
        ] {
            let label = gtk::Label::new(Some(value));
            label.set_xalign(0.0);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_max_width_chars(64);
            label.add_css_class(class);
            text.append(&label);
        }
        body.append(&text);
        row.set_child(Some(&body));
        self.list.append(&row);
    }

    fn navigate(&self, delta: i32) {
        let count = self.actions.borrow().len() as i32;
        if count == 0 {
            return;
        }
        let current = self.list.selected_row().map(|row| row.index()).unwrap_or(0);
        let next = (current + delta).rem_euclid(count);
        self.list.select_row(self.list.row_at_index(next).as_ref());
        self.scroll_selection_into_view();
    }

    fn scroll_selection_into_view(&self) {
        if let Some(row) = self.list.selected_row() {
            if let Some(bounds) = row.compute_bounds(&self.list) {
                let adjustment = self.scroll.vadjustment();
                let top = bounds.y() as f64;
                let bottom = top + bounds.height() as f64;
                if top < adjustment.value() {
                    adjustment.set_value(top);
                } else if bottom > adjustment.value() + adjustment.page_size() {
                    adjustment.set_value(bottom - adjustment.page_size());
                }
            }
        }
    }

    fn activate(self: &Rc<Self>, index: usize) {
        let action = self.actions.borrow().get(index).cloned();
        let result: Result<()> = (|| {
            let display = gdk::Display::default().context("显示连接不可用")?;
            match action {
                Some(Action::App(app)) => {
                    app.launch(&[], Some(&display.app_launch_context()))?;
                    if let Some(id) = app.id() {
                        let mut config = self.config.borrow_mut();
                        config.recent_apps.retain(|old| old != &id);
                        config.recent_apps.insert(0, id.to_string());
                        config.recent_apps.truncate(20);
                        config.save(&config::config_path())?;
                    }
                }
                Some(Action::Clipboard(Content::Text { text })) => {
                    display.clipboard().set_text(&text)
                }
                Some(Action::Clipboard(Content::Image { hash, .. })) => {
                    let png = self.history.borrow().image_bytes(&hash)?;
                    history::png_dimensions(&png).context("图片大小或格式不受支持")?;
                    let texture = gdk::Texture::from_bytes(&glib::Bytes::from_owned(png))
                        .context("无法读取图片")?;
                    display.clipboard().set_texture(&texture);
                }
                None => return Ok(()),
            }
            self.window.hide();
            Ok(())
        })();
        if let Err(error) = result {
            self.error(&error.to_string());
        }
    }

    pub fn error(&self, text: &str) {
        self.hint.set_text(text);
    }

    pub fn cancel_read(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        if let Some(cancel) = self.pending_read.borrow_mut().take() {
            cancel.cancel();
        }
    }

    fn read_clipboard(self: &Rc<Self>, clipboard: &gdk::Clipboard) {
        self.cancel_read();
        if !self.config.borrow().recording {
            return;
        }
        let generation = self.generation.get();
        let formats = clipboard.formats();
        let image = formats.contain_mime_type("image/png");
        let cancel = gio::Cancellable::new();
        *self.pending_read.borrow_mut() = Some(cancel.clone());
        let weak = Rc::downgrade(self);
        let mime_types = if image {
            vec!["image/png"]
        } else {
            vec![
                "text/plain;charset=utf-8",
                "text/plain;charset=UTF-8",
                "text/plain",
                "UTF8_STRING",
            ]
        };
        let cancellable = cancel.clone();
        clipboard.read_async(
            &mime_types,
            glib::Priority::DEFAULT,
            Some(&cancel),
            move |result| {
                let Ok((stream, _)) = result else {
                    return;
                };
                clipboard::read_limited(
                    stream,
                    cancellable,
                    if image {
                        history::MAX_IMAGE
                    } else {
                        history::MAX_TEXT
                    },
                    move |bytes| {
                        let Some(ui) = weak.upgrade() else {
                            return;
                        };
                        if ui.generation.get() != generation || !ui.config.borrow().recording {
                            return;
                        }
                        let Some(bytes) = bytes else {
                            return;
                        };
                        let limit = ui.config.borrow().history_limit;
                        let changed = if image {
                            let Some((width, height)) = history::png_dimensions(&bytes) else {
                                return;
                            };
                            ui.history
                                .borrow_mut()
                                .add_image(&bytes, width, height, limit)
                                .unwrap_or(false)
                        } else {
                            let Ok(text) = String::from_utf8(bytes) else {
                                return;
                            };
                            ui.history.borrow_mut().add_text(text, limit)
                        };
                        if changed {
                            ui.history_changed();
                        }
                    },
                );
            },
        );
    }

    pub fn history_changed(self: &Rc<Self>) {
        if self.window.is_visible() && self.effective_mode() == Mode::Clipboard {
            self.render();
        }
        if self.save_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            if let Some(ui) = weak.upgrade() {
                ui.save_pending.set(false);
                if ui
                    .history
                    .borrow()
                    .save(ui.config.borrow().persist_history)
                    .is_err()
                {
                    ui.error("无法保存历史，请检查本机存储空间。当前记录仍在内存中。");
                }
            }
        });
    }
}
