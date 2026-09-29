#!/usr/bin/env python3
"""Download portable JADX and Android Platform-Tools into this project."""
import hashlib
import json
import subprocess
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ROOT / '.tools'
DOWNLOADS = TOOLS / 'downloads'


def download(url, target, *, resume=False):
    target.parent.mkdir(parents=True, exist_ok=True)
    partial = target.with_suffix(target.suffix + '.part')
    subprocess.run([
        'curl', '--fail', '--location', '--retry', '3', '--retry-all-errors',
        *(['--continue-at', '-'] if resume else []), '--connect-timeout', '30',
        '--max-time', '900', '--silent', '--show-error', url, '-o', str(partial),
    ], check=True)
    partial.replace(target)


def extract(archive, destination):
    destination.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(archive) as z:
        for entry in z.infolist():
            target = (destination / entry.filename).resolve()
            if not target.is_relative_to(destination.resolve()):
                raise ValueError(f'Unsafe archive entry: {entry.filename}')
        z.extractall(destination)
        for entry in z.infolist():
            if entry.external_attr >> 16 & 0o111:
                target = destination / entry.filename
                target.chmod(target.stat().st_mode | 0o111)


if __name__ == '__main__':
    if not (TOOLS / 'jadx/bin/jadx').exists():
        print('Downloading JADX release metadata...', flush=True)
        metadata = DOWNLOADS / 'jadx-release.json'
        download('https://api.github.com/repos/skylot/jadx/releases/tags/v1.5.6', metadata)
        release = json.loads(metadata.read_text())
        asset = next(a for a in release['assets'] if a['name'] == f"jadx-{release['tag_name'].lstrip('v')}.zip")
        archive = DOWNLOADS / asset['name']
        print(f"Downloading {asset['name']}...", flush=True)
        download(asset['browser_download_url'], archive, resume=True)
        if asset.get('digest', '').startswith('sha256:'):
            with archive.open('rb') as stream:
                digest = hashlib.file_digest(stream, 'sha256').hexdigest()
            if digest != asset['digest'].removeprefix('sha256:'):
                raise ValueError('JADX archive checksum mismatch')
        extract(archive, TOOLS / 'jadx')
    if not (TOOLS / 'platform-tools/adb').exists():
        print('Downloading Android Platform-Tools for Linux...', flush=True)
        archive = DOWNLOADS / 'platform-tools-latest-linux.zip'
        download('https://dl.google.com/android/repository/platform-tools-latest-linux.zip', archive, resume=True)
        extract(archive, TOOLS)
    print('Portable Android tools installed under .tools/', flush=True)
