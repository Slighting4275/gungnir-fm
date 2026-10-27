#!/usr/bin/env python3
"""Stage the Debian-built executables and their private runtime inside Docker."""
import glob
import re
import shutil
import subprocess
import sys
from pathlib import Path

root = Path('/source/gungnir-fm')
vfs = Path('/source/gungnir-vfs')
prefix = Path(sys.argv[1])
if prefix.exists():
    shutil.rmtree(prefix)
private = prefix / 'lib/gungnir-fm'


def copy(source, destination):
    destination = Path(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, destination)


programs = {
    'gungnir-fm': '/out/cargo-fm/release/gungnir-fm',
    'gungnird': '/out/cargo-vfs/release/gungnird',
    'gio': '/usr/bin/gio',
}
elfs = []
for name, source in programs.items():
    copy(source, private / 'bin' / name)
    copy(root / 'packaging/release/launch.sh', prefix / 'bin' / name)
    elfs.append(source)
copy(root / 'packaging/release/launch.sh', prefix / 'bin/gungnir-fm-wrapper')
module = vfs / '.build/zig-out/lib/libgiogungnir.so'
copy(module, prefix / 'lib/gio/modules/libgiogungnir.so')
elfs.append(str(module))
(prefix / 'lib/gio/modules/giomodule.cache').write_text('libgiogungnir.so: gio-vfs\n')
for source in glob.glob('/usr/lib/x86_64-linux-gnu/gio/modules/*.so'):
    copy(source, private / 'gio-modules' / Path(source).name)
    elfs.append(source)
subprocess.run(['gio-querymodules', str(private / 'gio-modules')], check=True)
qt = Path('/usr/lib/x86_64-linux-gnu/qt6/plugins')
for source in qt.rglob('*.so'):
    copy(source, private / 'qt-plugins' / source.relative_to(qt))
    elfs.append(str(source))
# Winit and Qt can dlopen these rather than recording DT_NEEDED entries.
dlopen_libraries = set()
for pattern in ['libwayland-*.so.*', 'libxkbcommon*.so.*', 'libX*.so.*', 'libxcb*.so.*', 'libfontconfig.so.*', 'libfreetype.so.*', 'libEGL.so.*', 'libGL*.so.*']:
    dlopen_libraries.update(glob.glob('/usr/lib/x86_64-linux-gnu/' + pattern))
elfs.extend(sorted(dlopen_libraries))
libs = set(dlopen_libraries)
for source in elfs:
    result = subprocess.run(['ldd', source], check=True, text=True, capture_output=True)
    if 'not found' in result.stdout:
        raise RuntimeError(result.stdout)
    for line in result.stdout.splitlines():
        match = re.search(r'(?:=>\s+)?(/\S+)\s+\(', line)
        if match:
            libs.add(match.group(1))
# Include the loader explicitly; it is an invocation target, not an inherited env var.
libs.add('/lib64/ld-linux-x86-64.so.2')
for source in sorted(libs):
    copy(source, private / 'runtime' / Path(source).name)
for source, target in [
    ('packaging/flatpak/org.gungnir.GungnirFM.desktop', 'share/applications/org.gungnir.GungnirFM.desktop'),
    ('packaging/flatpak/org.gungnir.GungnirFM.metainfo.xml', 'share/metainfo/org.gungnir.GungnirFM.metainfo.xml'),
    ('crates/ui/assets/gungnir-fm.svg', 'share/icons/hicolor/scalable/apps/org.gungnir.GungnirFM.svg'),
]:
    copy(root / source, prefix / target)
for repository in [root, vfs]:
    for source in (repository / 'LICENSES').glob('*'):
        copy(source, prefix / 'share/licenses' / repository.name / source.name)
# Carry all installed Debian copyright notices alongside bundled shared libraries.
for source in Path('/usr/share/doc').glob('*/copyright'):
    copy(source, prefix / 'share/licenses/runtime' / source.parent.name / 'copyright')
shutil.copytree('/etc/fonts', prefix / 'share/fontconfig', symlinks=False)
for source in Path('/usr/share/fonts/truetype/dejavu').glob('*.ttf'):
    copy(source, prefix / 'share/fonts/truetype/dejavu' / source.name)
packages = subprocess.check_output(['dpkg-query', '-W', '-f=${Package} ${Version}\n'], text=True)
(prefix / 'share/licenses/runtime/packages.txt').write_text(packages)
# Package ownership is normalized later; all runtime files must remain readable
# to the installing user even when the builder has a restrictive umask.
for path in [prefix, *prefix.rglob('*')]:
    path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
print(f'Staged {len(programs)} programs, {len(libs)} runtime libraries and GIO/Qt plugins')
