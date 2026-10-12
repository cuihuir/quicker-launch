//! Entrypoint: models are separate from GTK and GNOME integration.
mod clipboard;
mod config;
mod history;
mod platform;
mod preferences;
mod search;
mod ui;

fn main() -> gtk::glib::ExitCode {
    ui::run()
}
