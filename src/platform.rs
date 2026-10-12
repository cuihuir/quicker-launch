//! Fedora GNOME adapter. All desktop mutations are confined to our binding.
use anyhow::{Context, Result, bail};
use gtk::{gdk, gio, prelude::*};
use std::process::Command;

pub const BINDING_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/quick-launcher/";
const MEDIA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";

pub fn available() -> bool {
    gio::SettingsSchemaSource::default().is_some_and(|source| source.lookup(MEDIA, true).is_some())
}

pub fn installed() -> bool {
    available()
        && gio::Settings::new(MEDIA)
            .strv("custom-keybindings")
            .iter()
            .any(|path| path == BINDING_PATH)
}

pub fn canonical_shortcut(value: &str) -> Result<String> {
    let (key, modifiers) = gtk::accelerator_parse(value).context("快捷键格式不正确")?;
    let meaningful = gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK;
    if !modifiers.intersects(meaningful) || !gtk::accelerator_valid(key, modifiers) {
        bail!("请使用 Ctrl、Alt 或 Super 加一个普通按键");
    }
    Ok(gtk::accelerator_name(key, modifiers).to_string())
}

pub fn shortcut_label(value: &str) -> String {
    gtk::accelerator_parse(value)
        .map(|(key, modifiers)| gtk::accelerator_get_label(key, modifiers).to_string())
        .unwrap_or_else(|| value.to_owned())
}

fn same_shortcut(a: &str, b: &str) -> bool {
    matches!((canonical_shortcut(a), canonical_shortcut(b)), (Ok(a), Ok(b)) if a == b)
}

pub fn check_shortcut(value: &str) -> Result<String> {
    let value = canonical_shortcut(value)?;
    if !installed() {
        return Ok(value);
    }
    let media = gio::Settings::new(MEDIA);
    for path in media.strv("custom-keybindings") {
        if path == BINDING_PATH {
            continue;
        }
        let settings = gio::Settings::with_path(CUSTOM, &path);
        if same_shortcut(&settings.string("binding"), &value) {
            bail!(
                "快捷键已被“{}”使用，请选择其他组合",
                settings.string("name")
            );
        }
    }
    for schema in [
        "org.gnome.desktop.wm.keybindings",
        MEDIA,
        "org.gnome.shell.keybindings",
    ] {
        let settings = gio::Settings::new(schema);
        let keys = settings
            .settings_schema()
            .context("缺少 GNOME 设置定义")?
            .list_keys();
        for key in keys {
            let setting = settings.value(&key);
            if setting.is::<Vec<String>>() {
                let values = setting.get::<Vec<String>>().unwrap_or_default();
                if values.iter().any(|binding| same_shortcut(binding, &value)) {
                    bail!("快捷键已被系统动作 {key} 使用，请选择其他组合");
                }
            } else if setting.is::<String>() {
                let binding = setting.get::<String>().unwrap_or_default();
                if same_shortcut(&binding, &value) {
                    bail!("快捷键已被系统动作 {key} 使用");
                }
            }
        }
    }
    Ok(value)
}

pub fn set_shortcut(value: &str) -> Result<()> {
    if installed() {
        gio::Settings::with_path(CUSTOM, BINDING_PATH).set_string("binding", value)?;
        gio::Settings::sync();
    }
    Ok(())
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    if !installed() {
        return Ok(());
    }
    let result = Command::new("systemctl")
        .args([
            "--user",
            if enabled { "enable" } else { "disable" },
            "quick-launcher.service",
        ])
        .output()?;
    if !result.status.success() {
        bail!("无法修改登录自启动，请检查 quick-launcher.service");
    }
    Ok(())
}

pub fn focus_window(title: &str) {
    // GNOME Wayland: the XWayland window is raised through the same WM interface
    // used by the distribution's Ulauncher toggle helper. No virtual key input.
    let _ = Command::new("wmctrl").args(["-F", "-a", title]).output();
}

pub fn system_dark() -> bool {
    gio::Settings::new("org.gnome.desktop.interface").string("color-scheme") == "prefer-dark"
}
