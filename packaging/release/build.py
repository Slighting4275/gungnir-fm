#!/usr/bin/env python3
"""Build real Linux packages locally; never succeed with metadata checks alone."""
import argparse
import hashlib
import json
import os
import platform
import shutil
import socket
import subprocess
import tarfile
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VERSION = tomllib.loads((ROOT / 'Cargo.toml').read_text())['workspace']['package']['version']
OUT = ROOT / '.build' / ('release-' + VERSION)
DOWNLOADS = ROOT / 'packages'
IMAGE = 'gungnir-release:' + VERSION
APP_ID = 'org.gungnir.GungnirFM'
ZIG_VERSION = '0.18.0-dev.624+b7f2c6d8f'
ZIG_SHA = '9f8233edf3995101108e39c2de44b38347750d6c169e6879d1fcfaf4e1c0c2f2'
RUNTIME_SHA = '2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d'


def run(*args, **kwargs):
    print('+', ' '.join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), check=True, **kwargs)


def download(url, target, checksum):
    if not target.exists() or hashlib.sha256(target.read_bytes()).hexdigest() != checksum:
        run('curl', '--fail', '--location', '--retry', '3', '--output', target, url)
    if hashlib.sha256(target.read_bytes()).hexdigest() != checksum:
        raise RuntimeError(f'Checksum mismatch: {target.name}')


def docker(*args, network='none'):
    return ['docker', 'run', '--rm', '--network', network, '--user', f'{os.getuid()}:{os.getgid()}',
            '-e', 'HOME=/out', '-v', f'{ROOT}:/source/gungnir-fm:ro', '-v', f'{OUT}:/out', *map(str, args)]


def prepare():
    vfs = Path(os.environ.get('GUNGNIR_VFS_DIR', ROOT.parent / 'gungnir-vfs')).resolve()
    if not (vfs / 'Cargo.toml').is_file():
        raise RuntimeError('Clone the companion gungnir-vfs repository next to gungnir-fm, or set GUNGNIR_VFS_DIR')
    if subprocess.check_output(['git', '-C', str(vfs), 'status', '--porcelain']):
        raise RuntimeError('Companion VFS checkout must be clean for a reproducible package')
    run('docker', 'build', '--network', 'host', '--add-host', 'deb.debian.org:' + socket.gethostbyname('deb.debian.org'),
        '-t', IMAGE, ROOT / 'packaging/release')
    zig = OUT / ('zig-x86_64-linux-' + ZIG_VERSION)
    archive = OUT / 'zig.tar.xz'
    download(f'https://ziglang.org/builds/zig-x86_64-linux-{ZIG_VERSION}.tar.xz', archive, ZIG_SHA)
    if not zig.exists():
        with tarfile.open(archive) as source:
            source.extractall(OUT, filter='data')
    for directory in ['vfs-build', 'cargo-home']:
        (OUT / directory).mkdir(exist_ok=True)
    run(*docker('-e', 'CARGO_HOME=/cargo', '-e', 'PATH=/zig:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin',
                '-e', 'ZIG_GLOBAL_CACHE_DIR=/out/zig-cache', '-v', f'{vfs}:/source/gungnir-vfs:ro',
                '-v', f'{OUT}/vfs-build:/source/gungnir-vfs/.build', '-v', f'{OUT}/cargo-home:/cargo',
                '-v', f'{Path.home()}/.cargo/registry:/cargo/registry:ro', '-v', f'{zig}:/zig:ro',
                IMAGE, '/source/gungnir-fm/packaging/release/build-container.sh'))
    # Stable staging path shared by the existing Snapcraft/Flatpak manifests.
    link = ROOT / '.build/release'
    if link.is_symlink():
        link.unlink()
    elif link.exists():
        raise RuntimeError(f'Refusing to replace existing directory: {link}')
    link.symlink_to(OUT.name, target_is_directory=True)
    provenance = {
        'version': VERSION,
        'fm_commit': subprocess.check_output(['git', '-C', str(ROOT), 'rev-parse', 'HEAD'], text=True).strip(),
        'fm_dirty': bool(subprocess.check_output(['git', '-C', str(ROOT), 'status', '--porcelain', '--untracked-files=no', '--', '.', ':(exclude)packages/**'])),
        'vfs_commit': subprocess.check_output(['git', '-C', str(vfs), 'rev-parse', 'HEAD'], text=True).strip(),
        'builder': 'rust:1.99-slim-bookworm@sha256:86234e857a413004348b471d11a4ebe1aa92d2eae2b6b143cac68fd1ac3c48be',
        'zig': ZIG_VERSION,
        'runtime': 'AppImage/type2-runtime 20251108',
    }
    (DOWNLOADS / 'BUILD-INFO.json').write_text(json.dumps(provenance, indent=2) + '\n')


def appimage():
    appdir = OUT / 'AppDir'
    if appdir.exists():
        shutil.rmtree(appdir)
    shutil.copytree(OUT / 'payload', appdir / 'usr')
    shutil.copy2(ROOT / 'packaging/appimage/AppRun', appdir / 'AppRun')
    shutil.copy2(ROOT / 'packaging/flatpak' / (APP_ID + '.desktop'), appdir / (APP_ID + '.desktop'))
    icon = ROOT / 'crates/ui/assets/gungnir-fm.svg'
    shutil.copy2(icon, appdir / (APP_ID + '.svg'))
    shutil.copy2(icon, appdir / '.DirIcon')
    runtime = OUT / 'runtime-x86_64'
    download('https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64', runtime, RUNTIME_SHA)
    runtime.chmod(0o755)
    run(*docker(IMAGE, 'mksquashfs', '/out/AppDir', '/out/appimage.squashfs', '-root-owned', '-noappend', '-comp', 'gzip', '-no-progress'))
    destination = DOWNLOADS / 'Gungnir_FM-x86_64.AppImage'
    with destination.open('wb') as target:
        for source in [runtime, OUT / 'appimage.squashfs']:
            with source.open('rb') as stream:
                shutil.copyfileobj(stream, target)
    destination.chmod(0o755)
    run(destination, '--appimage-version')
    run('appstreamcli', 'validate', '--no-net', appdir / 'usr/share/metainfo' / (APP_ID + '.metainfo.xml'))
    run('desktop-file-validate', appdir / (APP_ID + '.desktop'))


def snap():
    stage = OUT / 'snap'
    if stage.exists():
        shutil.rmtree(stage)
    shutil.copytree(OUT / 'payload', stage)
    meta = stage / 'meta'
    (meta / 'gui').mkdir(parents=True)
    data = {'name': 'gungnir-fm', 'version': VERSION, 'summary': 'Dual-pane file manager with Gungnir VFS',
            'description': 'Nemo-style file management with document templates, folder creation and a bundled VFS daemon.',
            'base': 'core24', 'grade': 'stable', 'confinement': 'classic', 'architectures': ['amd64'],
            'apps': {'gungnir-fm': {'command': 'bin/gungnir-fm-wrapper',
                                  'environment': {'GUNGNIR_PACKAGE_FORMAT': 'snap'}}}}
    (meta / 'snap.yaml').write_text(json.dumps(data, indent=2) + '\n')
    desktop = (stage / 'share/applications' / (APP_ID + '.desktop')).read_text()
    desktop = desktop.replace('Exec=gungnir-fm-wrapper', 'Exec=gungnir-fm')
    (meta / 'gui' / (APP_ID + '.desktop')).write_text(desktop)
    shutil.copy2(ROOT / 'crates/ui/assets/gungnir-fm.svg', meta / 'gui/icon.svg')
    for path in [stage, *stage.rglob('*')]:
        path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
    run(*docker(IMAGE, 'snap', 'pack', '/out/snap', '/out', '--filename=gungnir-fm.snap'))
    shutil.copy2(OUT / 'gungnir-fm.snap', DOWNLOADS / f'gungnir-fm_{VERSION}_amd64.snap')


def flatpak():
    stage = OUT / 'flatpak-build'
    if stage.exists():
        shutil.rmtree(stage)
    # The runtime is sufficient: compilation already happened locally in Docker.
    run('flatpak', 'build-init', '--arch=x86_64', stage, APP_ID,
        'org.freedesktop.Platform', 'org.freedesktop.Platform', '26.08')
    shutil.copytree(OUT / 'payload', stage / 'files', dirs_exist_ok=True)
    finish = json.loads((ROOT / 'packaging/release/flatpak-finish.json').read_text())
    run('flatpak', 'build-finish', '--command=gungnir-fm-wrapper', *finish, stage)
    repository = OUT / 'flatpak-repo'
    run('flatpak', 'build-export', '--arch=x86_64', repository, stage, 'stable')
    run('flatpak', 'build-bundle', '--arch=x86_64', '--runtime-repo=https://flathub.org/repo/flathub.flatpakrepo',
        repository, DOWNLOADS / (APP_ID + '.flatpak'), APP_ID, 'stable')


def checksums():
    assets = sorted(p for p in DOWNLOADS.iterdir() if p.suffix in ['.AppImage', '.snap', '.flatpak', '.json'])
    (DOWNLOADS / 'SHA256SUMS').write_text(''.join(f'{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n' for p in assets))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('format', choices=['all', 'prepare', 'appimage', 'snap', 'flatpak'])
    parser.add_argument('--skip-build', action='store_true', help='Reuse an already-built payload (developer checks only)')
    args = parser.parse_args()
    if platform.machine() != 'x86_64':
        parser.error('These release packages currently target x86_64 only')
    OUT.mkdir(parents=True, exist_ok=True)
    DOWNLOADS.mkdir(exist_ok=True)
    if not args.skip_build:
        prepare()
    if args.format != 'prepare':
        for name, build in [('appimage', appimage), ('snap', snap), ('flatpak', flatpak)]:
            if args.format in ['all', name]:
                build()
        checksums()
    print(f'Packages: {DOWNLOADS}')


if __name__ == '__main__':
    main()
