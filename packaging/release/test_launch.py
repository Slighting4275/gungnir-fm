#!/usr/bin/env python3
"""Exercise launchers with isolated socket directories and executable fixtures."""
import json
import os
import shutil
import socket
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class LauncherTests(unittest.TestCase):
    def setUp(self):
        (ROOT / '.build').mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='launcher-', dir=ROOT / '.build')
        self.root = Path(self.temp.name)
        self.prefix = self.root / 'prefix with spaces'
        self.private = self.prefix / 'lib/gungnir-fm'
        for path in ['bin', 'lib/gungnir-fm/bin', 'lib/gungnir-fm/runtime']:
            (self.prefix / path).mkdir(parents=True, exist_ok=True)
        for name in ['gungnir-fm', 'gungnird', 'gio']:
            shutil.copy2(ROOT / 'packaging/release/launch.sh', self.prefix / 'bin' / name)
        loader = self.private / 'runtime/ld-linux-x86-64.so.2'
        loader.write_text('#!/bin/sh\nshift 2\nexec "$@"\n')
        loader.chmod(0o755)
        fixture = '#!/usr/bin/env python3\nimport json,os,sys\nprint(json.dumps({"args":sys.argv[1:],"env":dict(os.environ)}))\n'
        for name in ['gungnir-fm', 'gio']:
            path = self.private / 'bin' / name
            path.write_text(fixture)
            path.chmod(0o755)
        daemon = self.private / 'bin/gungnird'
        daemon.write_text('#!/bin/sh\ntouch "$DAEMON_MARKER"\n')
        daemon.chmod(0o755)
        self.env = {'PATH': os.environ['PATH'], 'HOME': str(self.root),
                    'XDG_RUNTIME_DIR': str(self.root / 'runtime'),
                    'DAEMON_MARKER': str(self.root / 'daemon-started')}

    def tearDown(self):
        self.temp.cleanup()

    def launch(self, *args, name='gungnir-fm', **env):
        result = subprocess.run([str(self.prefix / 'bin' / name), *args],
                                env=self.env | env, text=True, capture_output=True, check=True)
        return json.loads(result.stdout)

    def test_version_has_no_daemon_or_directory_side_effects(self):
        result = self.launch('--version')
        self.assertEqual(result['args'], ['--version'])
        self.assertFalse((self.root / 'runtime').exists())
        self.assertFalse((self.root / 'daemon-started').exists())

    def test_custom_socket_parent_permissions_are_preserved(self):
        parent = self.root / 'shared'
        parent.mkdir(mode=0o755)
        parent.chmod(0o755)
        path = parent / 'daemon.sock'
        with socket.socket(socket.AF_UNIX) as server:
            server.bind(str(path))
            result = self.launch('/folder with spaces', GUNGNIR_SOCKET=str(path))
        self.assertEqual(stat.S_IMODE(parent.stat().st_mode), 0o755)
        self.assertEqual(result['env']['GUNGNIR_SOCKET'], str(path))
        self.assertEqual(result['args'], ['/folder with spaces'])
        self.assertNotIn('LD_LIBRARY_PATH', result['env'])

    def test_new_socket_directory_is_private_and_socket_is_exported(self):
        result = self.launch()
        parent = self.root / 'runtime/gungnir'
        self.assertEqual(stat.S_IMODE(parent.stat().st_mode), 0o700)
        self.assertEqual(result['env']['GUNGNIR_SOCKET'], str(parent / 'gungnird.sock'))

    def test_backend_opt_out_and_appimage_new_window_launcher(self):
        image = str(self.root / 'file manager.AppImage')
        result = self.launch(GUNGNIR_FM_BACKEND='off', APPIMAGE=image)
        self.assertEqual(result['env']['GUNGNIR_FM_BACKEND'], 'off')
        self.assertEqual(result['env']['GUNGNIR_FM_LAUNCHER'], image)
        self.assertFalse((self.root / 'runtime').exists())

    def test_remote_gio_does_not_reenable_gungnir_vfs(self):
        result = self.launch('list', 'network:///', name='gio')
        self.assertNotIn('GIO_USE_VFS', result['env'])
        self.assertEqual(result['args'], ['list', 'network:///'])
        self.assertFalse((self.root / 'runtime').exists())


if __name__ == '__main__':
    unittest.main()
