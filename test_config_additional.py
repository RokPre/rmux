"""Additional real-CLI config integration tests.

Run from this directory after ``make`` with:
    python3 -m unittest -v test_config_additional.py
"""

import os
import socket
import time
import unittest

from test_rmux import SPLIT_VERTICAL, Sessions


class AdditionalConfigSessions(Sessions):
    def test_missing_config_uses_defaults_and_attaches(self):
        config = self.config_dir / 'rmux' / 'config.toml'
        self.assertFalse(config.exists())

        process, _terminal = self.start(
            'new', 'defaults', '--', '/bin/sh', '-c', 'sleep 30'
        )
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if 'defaults' in self.run_rmux('ls').stdout.splitlines():
                break
            if process.poll() is not None:
                self.fail('rmux exited instead of attaching with default config')
            time.sleep(0.05)
        else:
            self.fail('session did not start with missing config')

        self.assertIsNone(process.poll())
        control = self.attach_control('defaults')
        control.close()
        self.assertFalse(config.exists())

    def test_ls_and_kill_work_with_invalid_config(self):
        process, _ = self.start(
            'new', 'management', '--', '/bin/sh', '-c', 'sleep 30'
        )
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if 'management' in self.run_rmux('ls').stdout.splitlines():
                break
            if process.poll() is not None:
                self.fail('session exited before becoming available')
            time.sleep(0.05)
        else:
            self.fail('session did not become available')

        config = self.config_dir / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('this is not valid TOML = [')

        listed = self.run_rmux('ls')
        self.assertEqual(listed.returncode, 0, listed.stderr)
        self.assertIn('management', listed.stdout.splitlines())

        killed = self.run_rmux('kill', 'management')
        self.assertEqual(killed.returncode, 0, killed.stderr)
        self.assertNotIn('management', self.run_rmux('ls').stdout.splitlines())

    def test_custom_binding_creates_buffer_through_attached_pty(self):
        config = self.config_dir / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('[bindings]\nnew_buffer = "ctrl+g"\n')

        process, terminal = self.start(
            'new', 'custom-key', '--', '/bin/sh', '-c', 'sleep 30'
        )
        deadline = time.monotonic() + 5
        while process.poll() is None and time.monotonic() < deadline:
            try:
                os.write(terminal, b'\x07')
                time.sleep(0.1)
                control = self.attach_control('custom-key')
                try:
                    layout = self.read_layout(control)
                finally:
                    control.close()
                if len(layout['buffers']) >= 2:
                    return
            except (AssertionError, OSError, socket.timeout, EOFError):
                time.sleep(0.05)
        self.fail('configured Ctrl-G did not create a buffer from PTY input')

    def test_configured_resize_step_from_attached_client(self):
        config = self.config_dir / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('[resize]\nstep_percent = 12\n')
        self.start('new', 'resize-config', '--', '/bin/sh', '-c', 'sleep 30')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'resize-config\n')
        control = self.attach_control('resize-config')
        try:
            self.read_layout(control)
            self.send_frame(control, SPLIT_VERTICAL)
            split = self.read_layout(control)
            self.assertEqual(split['tree']['ratio'], 50)

            # Attach a real rmux client with the configured resize step.
            _, terminal = self.start('attach', 'resize-config')
            self.wait_for(lambda: self.run_rmux('ls').stdout == 'resize-config\n')
            os.write(terminal, b'\x1b\x0c')
            resized = self.read_layout(control)
            self.assertEqual(resized['tree']['ratio'], 62)
        finally:
            control.close()


if __name__ == '__main__':
    unittest.main()
