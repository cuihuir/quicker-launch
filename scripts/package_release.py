#!/usr/bin/env python3
"""Package a built launcher with its installer, README, license and screenshots.

Dependencies: Python 3.11+, a release binary built with Cargo.
Effect: writes a tar.gz and SHA256SUMS under dist/; no build or system changes.
"""

import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import platform
import re
import subprocess
import tarfile
import tempfile
import tomllib

PROJECT = Path(__file__).resolve().parents[1]


def package(binary, output, label):
    binary = Path(binary).resolve()
    output = Path(output).resolve()
    version = tomllib.loads((PROJECT / 'Cargo.toml').read_text())['package']['version']
    if not re.fullmatch(r'[A-Za-z0-9._-]+', label):
        raise SystemExit('Target label may contain letters, numbers, dots, underscores and hyphens.')
    actual = subprocess.run([str(binary), '--version'], capture_output=True, text=True,
                            check=True, timeout=10).stdout.strip()
    if actual != f'quick-launcher {version}':
        raise SystemExit('Binary version does not match Cargo.toml; rebuild before packaging.')
    files = [(binary, 'quick-launcher', 0o755)]
    for name in ('scripts/install.py', 'README.md', 'LICENSE', 'config.example.toml',
                 'assets/screenshots/launcher.png', 'assets/screenshots/preferences.png'):
        source = PROJECT / name
        if not source.is_file():
            raise SystemExit(f'Missing release file: {name}')
        files.append((source, name, 0o755 if name.endswith('.py') else 0o644))
    output.mkdir(parents=True, exist_ok=True)
    root = f'quick-launcher-v{version}-{label}'
    archive = output / f'{root}.tar.gz'
    descriptor, temporary_name = tempfile.mkstemp(dir=output, prefix=archive.name+'.')
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, 'wb') as stream:
            with gzip.GzipFile(fileobj=stream, mode='wb', filename='', mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode='w') as tar:
                    for source, name, mode in files:
                        content = source.read_bytes()
                        member = tarfile.TarInfo(f'{root}/{name}')
                        member.size = len(content)
                        member.mode = mode
                        tar.addfile(member, io.BytesIO(content))
        temporary.replace(archive)
    finally:
        temporary.unlink(missing_ok=True)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / 'SHA256SUMS').write_text(f'{checksum}  {archive.name}\n')
    print(archive)
    print(output / 'SHA256SUMS')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default=PROJECT / 'target/release/quick-launcher')
    parser.add_argument('--output', default=PROJECT / 'dist')
    parser.add_argument('--target-label', default=f'linux-{platform.machine()}')
    arguments = parser.parse_args()
    package(arguments.binary, arguments.output, arguments.target_label)
