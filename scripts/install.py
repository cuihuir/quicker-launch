#!/usr/bin/env python3
"""Install a built Rust launcher, GNOME shortcut and one user service.

Dependencies: Python 3/PyGObject, systemd user session, wmctrl, built binary.
Effect: private rollback backup; installs only user files; disables conflicting
launcher startup and the old clipboard recorder after the new service starts.
No package installation, history deletion, or administrator access.
"""

import argparse
import datetime
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import tomllib

from gi.repository import Gio, GLib

PROJECT = Path(__file__).resolve().parents[1]
UNIT = 'quick-launcher.service'
MEDIA = 'org.gnome.settings-daemon.plugins.media-keys'
CUSTOM = MEDIA + '.custom-keybinding'
BINDING = '/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/quick-launcher/'
RECORDER = 'clipboard-indicator@tudmotu.com'


def run(*arguments):
    return subprocess.run(arguments, capture_output=True, text=True, check=True, timeout=30)


def is_state(action, unit):
    return subprocess.run(['systemctl', '--user', action, '--quiet', unit], capture_output=True).returncode == 0


def custom_state(path):
    settings = Gio.Settings.new_with_path(CUSTOM, path)
    return {key: settings.get_string(key) for key in ('name', 'command', 'binding')}


def backup_directory():
    """Keep private rollback files outside the checkout, including standalone clones."""
    configured = os.environ.get('XDG_STATE_HOME', '')
    state_home = Path(configured) if configured and Path(configured).is_absolute() else Path.home() / '.local/state'
    root = state_home / 'quick-launcher/install-backups'
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    root.chmod(0o700)
    directory = root / datetime.datetime.now().strftime('%Y%m%d-%H%M%S-%f')
    directory.mkdir(mode=0o700)
    directory.chmod(0o700)
    return directory


def copy_atomic(source, target):
    """Replace a user file without truncating an executing binary or partial copy."""
    target = Path(target)
    target.parent.mkdir(parents=True, exist_ok=True)
    descriptor, name = tempfile.mkstemp(prefix=target.name + '.', dir=target.parent)
    os.close(descriptor)
    temporary = Path(name)
    try:
        shutil.copy2(source, temporary)
        temporary.replace(target)
    finally:
        temporary.unlink(missing_ok=True)


def input_method_environment():
    """Use an available native Fcitx5 module instead of the IBus bridge.

    Keep explicit session preferences; otherwise detect the running input
    method and its GTK4 module. Do not change desktop-wide input settings.
    """
    selected = os.environ.get('GTK_IM_MODULE', '')
    if selected:
        return selected if selected in ('ibus', 'fcitx', 'xim', 'simple') else None
    module_available = any(
        list(Path(directory).glob('gtk-4.0/*/immodules/libim-fcitx5.so'))
        for directory in ('/usr/lib64', '/usr/lib', '/usr/lib/x86_64-linux-gnu'))
    if not module_available:
        return None
    try:
        bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        running = bus.call_sync(
            'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
            'NameHasOwner', GLib.Variant('(s)', ('org.fcitx.Fcitx5',)),
            GLib.VariantType.new('(b)'), Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
    except GLib.Error:
        return None
    return 'fcitx' if running else None


def wait_ready():
    """Wait for the service itself to own GApplication's name before forwarding.

    Starting a CLI client too early could make it the resident primary process.
    Checking the owner PID also detects a separately running development instance.
    """
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if not is_state('is-active', UNIT):
            raise RuntimeError('Launcher service exited before becoming ready.')
        pid = int(run('systemctl', '--user', 'show', UNIT, '-p', 'MainPID', '--value').stdout)
        try:
            owner = bus.call_sync(
                'org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
                'GetConnectionUnixProcessID', GLib.Variant('(s)', ('io.github.tope.QuickLauncher',)),
                GLib.VariantType.new('(u)'), Gio.DBusCallFlags.NONE, 1000, None).unpack()[0]
        except GLib.Error:
            time.sleep(.1)
            continue
        if owner != pid:
            raise RuntimeError('Quit the separately running development instance before installing.')
        return
    raise RuntimeError('Launcher did not register its single-instance interface in time.')


def restore(filename, stop_launcher=True):
    backup = Path(filename).resolve()
    state = json.loads(backup.read_text())
    if stop_launcher:
        subprocess.run(['systemctl', '--user', 'disable', '--now', UNIT], capture_output=True)
    for name, saved in state['files'].items():
        target = Path(name)
        if saved is None:
            target.unlink(missing_ok=True)
        else:
            copy_atomic(backup.parent / saved, target)
    settings = Gio.Settings.new(MEDIA)
    current = list(settings.get_strv('custom-keybindings'))
    if not state['binding_was_listed']:
        current = [path for path in current if path != BINDING]
    settings.set_strv('custom-keybindings', current)
    Gio.Settings.new('org.gnome.desktop.wm.keybindings').set_strv('activate-window-menu', state['window_menu'])
    for path, values in state['bindings'].items():
        binding = Gio.Settings.new_with_path(CUSTOM, path)
        for key, value in values.items():
            binding.set_string(key, value)
    shell = Gio.Settings.new('org.gnome.shell')
    extensions = list(shell.get_strv('enabled-extensions'))
    if state['recorder_enabled'] and RECORDER not in extensions:
        run('gnome-extensions', 'enable', RECORDER)
    Gio.Settings.sync()
    run('systemctl', '--user', 'daemon-reload')
    for unit, status in state['units'].items():
        if status['enabled']:
            run('systemctl', '--user', 'enable', unit)
        if status['active']:
            run('systemctl', '--user', 'start', unit)
    print('Restored launcher files, shortcut, services and clipboard recorder.')


def install(binary):
    binary = Path(binary).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise SystemExit('Build first: cargo build --release --locked')
    if not shutil.which('wmctrl'):
        raise SystemExit('Install Fedora wmctrl first.')
    config = Path(os.environ.get('XDG_CONFIG_HOME') or Path.home() / '.config')
    data = Path(os.environ.get('XDG_DATA_HOME') or Path.home() / '.local/share')
    target = Path.home() / '.local/bin/quick-launcher'
    unit = config / 'systemd/user' / UNIT
    desktop = data / 'applications/io.github.tope.QuickLauncher.desktop'
    settings = Gio.Settings.new(MEDIA)
    preferences = config / 'quick-launcher/config.toml'
    values = tomllib.loads(preferences.read_text()) if preferences.exists() else {}
    shortcut = values.get('shortcut', '<Alt>space')
    autostart = values.get('autostart', True)
    paths = list(settings.get_strv('custom-keybindings'))
    selected = [path for path in paths if path == BINDING or custom_state(path)['binding'].lower().replace('mod1', 'alt') == shortcut.lower()]
    # Preserve the only desktop settings this installation will change.
    selected = list(dict.fromkeys(selected + [BINDING]))
    backup_dir = backup_directory()
    files = {}
    for index, file in enumerate((target, unit, desktop)):
        saved = f'file-{index}' if file.exists() else None
        files[str(file)] = saved
        if saved is not None:
            shutil.copy2(file, backup_dir / saved)
    state = {'files': files, 'binding_was_listed': BINDING in paths,
             'bindings': {path: custom_state(path) for path in selected},
             'window_menu': list(Gio.Settings.new('org.gnome.desktop.wm.keybindings').get_strv('activate-window-menu')),
             'recorder_enabled': RECORDER in Gio.Settings.new('org.gnome.shell').get_strv('enabled-extensions'),
             'units': {name: {'enabled': is_state('is-enabled', name), 'active': is_state('is-active', name)}
                       for name in (UNIT, 'ulauncher.service')}}
    backup = backup_dir / 'state.json'
    with backup.open('x') as stream:
        os.chmod(backup, 0o600)
        json.dump(state, stream, indent=2)
    print('Backup:', backup, flush=True)
    service_changed = False
    try:
        input_method = input_method_environment()
        input_environment = f'Environment=GTK_IM_MODULE={input_method}\n' if input_method else ''
        unit.parent.mkdir(parents=True, exist_ok=True)
        unit.write_text(f'''[Unit]
Description=Quick Launcher application search and clipboard history
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
Environment=GDK_BACKEND=x11
Environment=GSK_RENDERER=cairo
{input_environment}ExecStart="{target}" --background
KillMode=process
Restart=on-failure
RestartSec=2

[Install]
WantedBy=graphical-session.target
''')
        # Desktop applications launched through Gio can inherit this cgroup.
        # Reload the main-process-only stop policy before stopping an old unit,
        # so upgrading the launcher does not also close those applications.
        run('systemctl', '--user', 'daemon-reload')
        if state['units'][UNIT]['active']:
            service_changed = True
            run('systemctl', '--user', 'stop', UNIT)
        target.parent.mkdir(parents=True, exist_ok=True)
        copy_atomic(binary, target)
        target.chmod(0o755)
        desktop.parent.mkdir(parents=True, exist_ok=True)
        desktop.write_text(f'''[Desktop Entry]
Type=Application
Name=Quick Launcher
Comment=应用搜索与剪贴板历史
Exec="{target}" --toggle
Icon=system-search
Terminal=false
Categories=Utility;
StartupWMClass=quick-launcher
''')
        run('systemctl', '--user', 'daemon-reload')
        service_changed = True
        run('systemctl', '--user', 'enable' if autostart else 'disable', UNIT)
        run('systemctl', '--user', 'start', UNIT)
        run('systemctl', '--user', 'is-active', '--quiet', UNIT)
        wait_ready()
        # Single-instance command confirms initialization and registration.
        run(str(target), '--background')
        run('systemctl', '--user', 'is-active', '--quiet', UNIT)
        for path in selected:
            if path != BINDING:
                Gio.Settings.new_with_path(CUSTOM, path).set_string('binding', '')
        binding = Gio.Settings.new_with_path(CUSTOM, BINDING)
        binding.set_string('name', 'Quick Launcher')
        binding.set_string('command', str(target) + ' --toggle')
        binding.set_string('binding', shortcut)
        Gio.Settings.new('org.gnome.desktop.wm.keybindings').set_strv('activate-window-menu',
            [key for key in state['window_menu'] if key.lower().replace('mod1', 'alt') != shortcut.lower()])
        if BINDING not in paths:
            settings.set_strv('custom-keybindings', paths + [BINDING])
        Gio.Settings.sync()
        if state['units']['ulauncher.service']['active'] or state['units']['ulauncher.service']['enabled']:
            run('systemctl', '--user', 'disable', '--now', 'ulauncher.service')
        if state['recorder_enabled']:
            run('gnome-extensions', 'disable', RECORDER)
    except Exception:
        restore(backup, stop_launcher=service_changed)
        raise
    print(f'Installed shortcut: {shortcut}; Left/Right / Tab / clip Space; Ctrl+, opens preferences.')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--install', action='store_true')
    action.add_argument('--restore', metavar='BACKUP_JSON')
    parser.add_argument('--binary', default=PROJECT / 'target/release/quick-launcher')
    args = parser.parse_args()
    install(args.binary) if args.install else restore(args.restore)
