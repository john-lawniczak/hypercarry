import importlib.util
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
import subprocess
import tempfile
import os

TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS))
SPEC = importlib.util.spec_from_file_location('private_push', TOOLS / 'check_private_push.py')
push = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(push)


class PrivatePushTests(unittest.TestCase):
    def updates(self):
        return 'refs/heads/main ' + 'a' * 40 + ' refs/heads/main ' + 'b' * 40

    def test_private_history_requires_exact_explicit_destination(self):
        config = subprocess.CompletedProcess([], 0, 'https://example.invalid/private.git\n', '')
        with patch.object(push, 'contains_private_history', return_value=True), patch.object(push.subprocess, 'run', return_value=config):
            push.check_push('https://example.invalid/private.git', self.updates())
            for destination in ['https://example.invalid/public.git', 'https://example.invalid/private.git.evil', 'disabled://use-filtered-export']:
                with self.assertRaisesRegex(ValueError, 'blocked'):
                    push.check_push(destination, self.updates())
        with patch.object(push, 'contains_private_history', return_value=True), patch.object(push.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1, '', '')):
            with self.assertRaisesRegex(ValueError, 'blocked'):
                push.check_push('https://example.invalid/private.git', self.updates())

    def test_clean_history_and_deletions_do_not_need_private_destination(self):
        with patch.object(push, 'contains_private_history', return_value=False), patch.object(push.subprocess, 'run') as config:
            push.check_push('https://example.invalid/public.git', self.updates())
            push.check_push('https://example.invalid/public.git', 'deleted ' + '0' * 40 + ' refs/heads/old ' + 'b' * 40)
            config.assert_not_called()
        with self.assertRaises(ValueError):
            push.check_push('anything', 'malformed input')

    def test_deleted_private_paths_in_reachable_history_are_detected(self):
        with patch.object(push.subprocess, 'check_output', side_effect=['a'*40+'\n', b'README.md\0nested/z-orders.json\0']):
            self.assertTrue(push.contains_private_history('a' * 40))
        with patch.object(push.subprocess, 'check_output', side_effect=['a'*40+'\n', b'README.md\0crates/core.rs\0']):
            self.assertFalse(push.contains_private_history('a' * 40))

    def test_real_git_history_with_newline_paths_and_deleted_private_files(self):
        previous = Path.cwd()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            def git(*args):
                return subprocess.run(['git', '-C', tmp, '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', '-c', 'core.hooksPath=/dev/null', *args], check=True, capture_output=True)
            git('init', '-q')
            file = root / 'notes\nfolder' / 'z-private.json'
            file.parent.mkdir(); file.write_text('synthetic private fixture')
            git('add', '.'); git('commit', '-qm', 'private fixture')
            file.unlink(); git('add', '-A'); git('commit', '-qm', 'remove private fixture')
            try:
                os.chdir(root)
                self.assertTrue(push.contains_private_history('HEAD'))
            finally:
                os.chdir(previous)
