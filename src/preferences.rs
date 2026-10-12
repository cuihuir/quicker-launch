//! Native preferences: validate first, apply desktop effects, then update state.
use crate::{
    config::{self, Mode, Theme},
    platform,
    ui::Ui,
};
use gtk::{gdk, glib, prelude::*};
use std::{cell::RefCell, rc::Rc};

fn row(body: &gtk::Box, title: &str, control: &impl IsA<gtk::Widget>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 14);
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.append(&label);
    row.append(control);
    body.append(&row);
}

fn section(body: &gtk::Box, title: &str) {
    let label = gtk::Label::new(Some(title));
    label.set_xalign(0.0);
    label.set_margin_top(10);
    label.add_css_class("heading");
    body.append(&label);
}

pub fn show(ui: &Rc<Ui>) {
    if let Some(window) = ui.preferences.borrow().as_ref() {
        if ui.window.is_visible() {
            ui.leave_for_preferences();
        }
        window.present();
        glib::idle_add_local_once(|| platform::focus_window("Quick Launcher — 偏好设置"));
        return;
    }
    ui.leave_for_preferences();
    let config = ui.config.borrow().clone();
    let window = gtk::Window::builder()
        .application(&ui.window.application().unwrap())
        .title("Quick Launcher — 偏好设置")
        .default_width(560)
        .default_height(620)
        .build();
    let header = gtk::HeaderBar::new();
    window.set_titlebar(Some(&header));
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.set_margin_top(16);
    body.set_margin_bottom(24);
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .overlay_scrolling(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&body)
        .build();
    outer.append(&scroll);

    section(&body, "快捷键");
    let shortcut = Rc::new(RefCell::new(config.shortcut.clone()));
    let shortcut_button = gtk::Button::with_label(&platform::shortcut_label(&config.shortcut));
    shortcut_button.set_tooltip_text(Some("点击后按下新的组合键"));
    row(&body, "呼出搜索框", &shortcut_button);
    let button = shortcut_button.clone();
    let value = shortcut.clone();
    let parent = window.clone();
    shortcut_button.connect_clicked(move |_| capture_shortcut(&parent, &button, &value));

    section(&body, "搜索与外观");
    let mode = gtk::DropDown::from_strings(&["应用", "剪贴板"]);
    mode.set_selected(if config.default_mode == Mode::Apps {
        0
    } else {
        1
    });
    row(&body, "默认搜索页", &mode);
    let theme = gtk::DropDown::from_strings(&["跟随系统", "浅色", "深色"]);
    theme.set_selected(match config.theme {
        Theme::System => 0,
        Theme::Light => 1,
        Theme::Dark => 2,
    });
    row(&body, "主题", &theme);
    let recent_limit = gtk::SpinButton::with_range(0.0, 20.0, 1.0);
    recent_limit.set_value(config.recent_apps_limit as f64);
    row(&body, "最近使用显示条数", &recent_limit);
    let recent_note = gtk::Label::new(Some(
        "设为 0 可隐藏最近使用；下方完整应用列表不受条数限制。",
    ));
    recent_note.set_xalign(0.0);
    recent_note.set_wrap(true);
    body.append(&recent_note);
    let tip = gtk::Label::new(Some(
        "← 应用，→ 剪贴板；Tab 切换；输入 clip 空格也能进入历史。",
    ));
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    body.append(&tip);

    section(&body, "剪贴板");
    let results = gtk::SpinButton::with_range(4.0, 12.0, 1.0);
    results.set_value(config.max_results as f64);
    row(&body, "剪贴板显示条数", &results);
    let recording = gtk::Switch::builder()
        .active(config.recording)
        .valign(gtk::Align::Center)
        .build();
    row(&body, "记录新复制的内容", &recording);
    let persist = gtk::Switch::builder()
        .active(config.persist_history)
        .valign(gtk::Align::Center)
        .build();
    row(&body, "保存历史到本机", &persist);
    let limit = gtk::SpinButton::with_range(10.0, 500.0, 10.0);
    limit.set_value(config.history_limit as f64);
    row(&body, "历史最多保留条数", &limit);
    let note = gtk::Label::new(Some(
        "文字和图片仅保存在本机。关闭保存后，下次启动不保留历史；暂停记录不会删除现有历史。",
    ));
    note.set_xalign(0.0);
    note.set_wrap(true);
    body.append(&note);
    let history_buttons = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let import = gtk::Button::with_label("导入旧文本历史");
    let clear = gtk::Button::with_label("清空历史…");
    clear.add_css_class("destructive-action");
    history_buttons.append(&import);
    history_buttons.append(&clear);
    body.append(&history_buttons);

    section(&body, "启动");
    let autostart = gtk::Switch::builder()
        .active(config.autostart)
        .valign(gtk::Align::Center)
        .build();
    row(&body, "登录后自动启动", &autostart);
    let status = gtk::Label::new(Some(if platform::installed() {
        "已安装 GNOME 快捷键和用户服务。"
    } else {
        "开发模式：快捷键和自启动将在安装后生效。"
    }));
    status.set_xalign(0.0);
    status.set_wrap(true);
    body.append(&status);
    let feedback = gtk::Label::new(None);
    feedback.set_xalign(0.0);
    feedback.set_wrap(true);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 10);
    footer.set_margin_start(24);
    footer.set_margin_end(24);
    footer.set_margin_top(12);
    footer.set_margin_bottom(18);
    footer.append(&feedback);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("取消");
    let save = gtk::Button::with_label("保存");
    save.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&save);
    footer.append(&actions);
    outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    outer.append(&footer);
    window.set_child(Some(&outer));
    let parent = window.clone();
    cancel.connect_clicked(move |_| parent.close());

    let shared = ui.clone();
    let parent = window.clone();
    let feedback_clone = feedback.clone();
    save.connect_clicked(move |_| {
        let old = shared.config.borrow().clone();
        let mut candidate = old.clone();
        candidate.shortcut = shortcut.borrow().clone();
        candidate.default_mode = if mode.selected() == 0 {
            Mode::Apps
        } else {
            Mode::Clipboard
        };
        candidate.theme = match theme.selected() {
            1 => Theme::Light,
            2 => Theme::Dark,
            _ => Theme::System,
        };
        candidate.max_results = results.value_as_int() as usize;
        candidate.recent_apps_limit = recent_limit.value_as_int() as usize;
        candidate.history_limit = limit.value_as_int() as usize;
        candidate.recording = recording.is_active();
        candidate.persist_history = persist.is_active();
        candidate.autostart = autostart.is_active();
        let result = (|| -> anyhow::Result<()> {
            candidate.shortcut = platform::check_shortcut(&candidate.shortcut)?;
            candidate.validate()?;
            candidate.save(&config::config_path())?;
            let history_result = shared
                .history
                .borrow_mut()
                .set_persistence(candidate.persist_history);
            if let Err(error) = history_result
                .and_then(|_| platform::set_shortcut(&candidate.shortcut))
                .and_then(|_| platform::set_autostart(candidate.autostart))
            {
                let _ = old.save(&config::config_path());
                let _ = platform::set_shortcut(&old.shortcut);
                let _ = platform::set_autostart(old.autostart);
                let _ = shared
                    .history
                    .borrow_mut()
                    .set_persistence(old.persist_history);
                return Err(error);
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                *shared.config.borrow_mut() = candidate.clone();
                if !candidate.recording {
                    shared.cancel_read();
                }
                shared.history.borrow_mut().trim(candidate.history_limit);
                shared.apply_theme();
                shared.history_changed();
                shared.render();
                parent.close();
            }
            Err(error) => feedback_clone.set_text(&error.to_string()),
        }
    });
    let shared = ui.clone();
    let feedback_clone = feedback.clone();
    import.connect_clicked(move |_| {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                    .join(".cache")
            });
        let limit = shared.config.borrow().history_limit;
        let result = shared.history.borrow_mut().import_indicator(
            &cache.join("clipboard-indicator@tudmotu.com/registry.txt"),
            limit,
        );
        match result {
            Ok(count) => {
                shared.history_changed();
                feedback_clone.set_text(&format!("已导入 {count} 条旧文本历史。"));
            }
            Err(_) => feedback_clone.set_text("没有找到可导入的 Clipboard Indicator 历史。"),
        }
    });
    let shared = ui.clone();
    let parent = window.clone();
    clear.connect_clicked(move |_| {
        let dialog = gtk::MessageDialog::builder()
            .transient_for(&parent)
            .modal(true)
            .message_type(gtk::MessageType::Question)
            .text("清空所有剪贴板历史？")
            .secondary_text("会删除本工具保存的文字和图片，无法撤销。当前系统剪贴板仍保持原内容。")
            .build();
        dialog.add_button("取消", gtk::ResponseType::Cancel);
        dialog.add_button("清空", gtk::ResponseType::Accept);
        let shared = shared.clone();
        dialog.connect_response(move |dialog, response| {
            if response == gtk::ResponseType::Accept {
                shared.cancel_read();
                let cleared = shared.history.borrow_mut().clear().is_ok();
                if cleared {
                    shared.history_changed();
                }
            }
            dialog.close();
        });
        dialog.present();
    });
    let weak = Rc::downgrade(ui);
    window.connect_close_request(move |_| {
        if let Some(ui) = weak.upgrade() {
            *ui.preferences.borrow_mut() = None;
            ui.return_from_preferences();
        }
        glib::Propagation::Proceed
    });
    *ui.preferences.borrow_mut() = Some(window.clone());
    window.present();
    glib::idle_add_local_once(|| platform::focus_window("Quick Launcher — 偏好设置"));
}

fn capture_shortcut(parent: &gtk::Window, button: &gtk::Button, value: &Rc<RefCell<String>>) {
    let window = gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title("设置快捷键")
        .default_width(380)
        .default_height(150)
        .resizable(false)
        .build();
    let label = gtk::Label::new(Some("请按下新的组合键\n例如 Alt+Space；Esc 取消"));
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    window.set_child(Some(&label));
    let controller = gtk::EventControllerKey::new();
    let parent = window.clone();
    let button = button.clone();
    let value = value.clone();
    controller.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::Escape {
            parent.close();
            return glib::Propagation::Stop;
        }
        let meaningful = gdk::ModifierType::CONTROL_MASK
            | gdk::ModifierType::ALT_MASK
            | gdk::ModifierType::SUPER_MASK;
        if modifiers.intersects(meaningful) && gtk::accelerator_valid(key, modifiers) {
            let name =
                gtk::accelerator_name(key, modifiers & gtk::accelerator_get_default_mod_mask());
            *value.borrow_mut() = name.to_string();
            button.set_label(&platform::shortcut_label(&name));
            parent.close();
        }
        glib::Propagation::Stop
    });
    window.add_controller(controller);
    window.present();
}
