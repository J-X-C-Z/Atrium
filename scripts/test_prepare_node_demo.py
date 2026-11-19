"""Isolated helper contract tests. Never start a node or use real settings."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HELPER = Path(__file__).with_name('prepare-node-demo.py')


class DemoTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / 'fake binaries'
        self.bin.mkdir()
        for name in ('atrium-node', 'mock-codex', 'mock-acp'):
            (self.bin / name).touch()
        self.dest = self.root / 'demo'

    def run_helper(self, port=7382):
        return subprocess.run([sys.executable, str(HELPER), '--output', str(self.dest),
                               '--bin-dir', str(self.bin), '--port', str(port)],
                              capture_output=True, text=True)

    def test_nondefault_port_and_private_consistent_settings(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        config = (self.dest / 'node.toml').read_text()
        self.assertIn('port=7382\n', config)
        self.assertIn('loopback_only=true', config)
        settings = []
        for relative in ('home/Library/Application Support/Atrium/settings.json',
                         'home/.config/Atrium/settings.json'):
            path = self.dest / relative
            settings.append(json.loads(path.read_text()))
            if os.name == 'posix':
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
        self.assertEqual(settings[0], settings[1])
        self.assertEqual(settings[0]['nodes'], ['127.0.0.1:7382'])
        token = settings[0]['node_token']
        self.assertEqual(len(token), 48)
        self.assertIn('token=' + json.dumps(token), config)
        self.assertNotIn(token, result.stdout + result.stderr)
        self.assertEqual(settings[0]['cwd'], str((self.dest / 'work').resolve()))
        if os.name == 'posix':
            self.assertEqual(self.dest.stat().st_mode & 0o777, 0o700)

    @unittest.skipUnless(os.name == 'posix', 'requires symlink fixture')
    def test_symlinked_parent_uses_canonical_working_root(self):
        alias = self.root / 'alias'
        alias.symlink_to(self.root, target_is_directory=True)
        self.dest = alias / 'canonical-demo'
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        settings = json.loads((self.dest / 'home/.config/Atrium/settings.json').read_text())
        self.assertEqual(settings['cwd'], str((self.root / 'canonical-demo/work').resolve()))

    def test_nonempty_destination_is_unchanged(self):
        self.dest.mkdir()
        sentinel = self.dest / 'keep'
        sentinel.write_text('original')
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.assertEqual(list(self.dest.iterdir()), [sentinel])
        self.assertEqual(sentinel.read_text(), 'original')

    def test_invalid_port_and_missing_binary_do_not_create_destination(self):
        for port in (0, 65536, -1):
            self.assertNotEqual(self.run_helper(port).returncode, 0)
            self.assertFalse(self.dest.exists())
        (self.bin / 'mock-acp').unlink()
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.assertFalse(self.dest.exists())


if __name__ == '__main__':
    unittest.main()
