import fcntl
import json
import os
import pty
import select
import socket
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

RMUX = Path(__file__).with_name('rmux')
INPUT = 1
CLOSE = 2
WINDOW_STATE = 3
BUFFER_SNAPSHOT = 4
BUFFER_OUTPUT = 5
CLIENT_RESIZE = 2
NEW_BUFFER = 4
CLOSE_BUFFER = 5
PREVIOUS_BUFFER = 6
NEXT_BUFFER = 7
SPLIT_VERTICAL = 8
SPLIT_HORIZONTAL = 9
CLOSE_WINDOW = 10
FOCUS_WINDOW = 11
MOVE_WINDOW = 12
RESIZE_WINDOW = 13
REORDER_BUFFER_LEFT = 14
REORDER_BUFFER_RIGHT = 15
RENAME_BUFFER = 16


class Sessions(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='rmux-')
        self.config_dir = Path(self.tmp.name, 'config')
        self.config_dir.mkdir()
        self.env = dict(os.environ, XDG_RUNTIME_DIR=self.tmp.name,
                        XDG_CONFIG_HOME=str(self.config_dir), HOME=self.tmp.name,
                        TERM='xterm')
        self.clients = []

    def tearDown(self):
        for name in self.run_rmux('ls').stdout.splitlines():
            subprocess.run([str(RMUX), 'kill', name], env=self.env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for process, fd in self.clients:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            os.close(fd)
        self.tmp.cleanup()

    def run_rmux(self, *args):
        return subprocess.run([str(RMUX), *args], env=self.env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)

    def test_invalid_config_fails_before_session_start(self):
        config = self.config_dir / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('[resize]\nstep_percent = 0\n')
        result = self.run_rmux('new', 'invalid')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('resize.step_percent', result.stderr)
        self.assertEqual(self.run_rmux('ls').stdout, '')

    def test_custom_binding_works_when_attaching_to_new_session(self):
        config = self.config_dir / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('[bindings]\nnew_buffer = "ctrl+g"\n')
        process, terminal = self.start('new', 'custom-binding', '--', '/bin/sh', '-c', 'sleep 30')
        self.addCleanup(lambda: os.write(terminal, b'\x03') if process.poll() is None else None)
        deadline = time.monotonic() + 5
        while process.poll() is None and time.monotonic() < deadline:
            try:
                os.write(terminal, b'\x07')
                time.sleep(0.1)
                control = self.attach_control('custom-binding')
                layout = self.read_layout(control)
                control.close()
                self.assertGreaterEqual(len(layout['buffers']), 2)
                return
            except (AssertionError, OSError, socket.timeout, EOFError):
                continue
        self.fail('custom Ctrl-G binding did not create a second buffer')

    def drain_terminal_output(self, fd):
        while True:
            try:
                os.read(fd, 4096)
            except OSError:
                return

    def start(self, *args):
        master, slave = pty.openpty()
        process = subprocess.Popen([str(RMUX), *args], env=self.env, stdin=slave,
                                   stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.clients.append((process, master))
        threading.Thread(
            target=self.drain_terminal_output,
            args=(master,),
            daemon=True,
        ).start()
        return process, master

    def test_opt_in_resize_diagnostics_record_lifecycle_without_input(self):
        log = Path(self.tmp.name, 'debug.log')
        self.env['RMUX_DEBUG_LOG'] = str(log)
        self.start('new', 'diagnostic', '--', 'sh', '-c', 'sleep 1; exit 7')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'diagnostic\n')
        control = self.attach_control('diagnostic')
        try:
            self.read_layout(control)
            self.send_frame(control, CLIENT_RESIZE, (20).to_bytes(2, 'big') + (70).to_bytes(2, 'big'))
            self.wait_for(lambda: log.exists() and 'size=70x20' in log.read_text())
        finally:
            control.close()
        self.wait_for(lambda: 'buffer exited session=diagnostic' in log.read_text())
        contents = log.read_text()
        self.assertIn('server started session=diagnostic', contents)
        self.assertIn('client attached session=diagnostic', contents)
        self.assertIn('status=', contents)
        self.assertNotIn('sleep 1; exit 7', contents)

    def test_slow_client_recovers_without_blocking_other_clients(self):
        log = Path(self.tmp.name, 'debug.log')
        trigger = Path(self.tmp.name, 'start-output')
        self.env['RMUX_DEBUG_LOG'] = str(log)
        command = (
            'import os,time\n'
            f'while not os.path.exists({str(trigger)!r}): time.sleep(0.01)\n'
            'os.write(1, b"x" * 8000000)\n'
            'os.write(1, b"\\x1b[2J\\x1b[H\\x1b[31")\n'
            'time.sleep(0.2)\n'
            'os.write(1, b"mRECOVEREDMARK\\x1b[0m")\n'
            'time.sleep(30)\n'
        )
        self.start('new', 'slow-client', '--', 'python3', '-c', command)
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'slow-client\n')
        slow = self.attach_control('slow-client')
        healthy = None
        try:
            self.read_layout(slow)
            healthy = self.attach_control('slow-client')
            self.read_layout(healthy)
            trigger.touch()
            deadline = time.monotonic() + 12
            while time.monotonic() < deadline:
                if 'client switched to snapshot-only client=2' in log.read_text():
                    break
                time.sleep(0.05)
            self.assertIn('client switched to snapshot-only client=2', log.read_text())
            self.send_frame(slow, CLIENT_RESIZE, (20).to_bytes(2, 'big') + (70).to_bytes(2, 'big'))
            self.wait_for(lambda: 'client resize client=2 size=70x20' in log.read_text())

            self.send_frame(healthy, RENAME_BUFFER, (1).to_bytes(8, 'big') + b'responsive')
            while self.read_layout(healthy)['buffer_names']['1'] != 'responsive':
                pass

            while True:
                kind, payload = self.read_frame(slow)
                self.assertNotEqual(kind, CLOSE)
                if kind == BUFFER_SNAPSHOT and b'RECOVEREDMARK' in payload[12:]:
                    self.assertEqual(int.from_bytes(payload[8:10], 'big'), 19)
                    self.assertEqual(int.from_bytes(payload[10:12], 'big'), 70)
                    break
            self.send_frame(healthy, RENAME_BUFFER, (1).to_bytes(8, 'big') + b'after-recovery')
            while True:
                kind, payload = self.read_frame(slow)
                self.assertNotEqual(kind, CLOSE)
                if kind == WINDOW_STATE and json.loads(payload)['buffer_names']['1'] == 'after-recovery':
                    break
        finally:
            slow.close()
            if healthy is not None:
                healthy.close()

    def test_bracketed_paste_reaches_application_unchanged(self):
        payload = b'\x1b[200~first line\nsecond\x14 line\n\x1b[201~'
        received = Path(self.tmp.name, 'paste')
        program = (
            'import os, tty, time\n'
            'tty.setraw(0)\n'
            'os.write(1, b"\\x1b[?2004h")\n'
            'data = b""\n'
            f'while len(data) < {len(payload)}: data += os.read(0, 4096)\n'
            f'open({str(received)!r}, "wb").write(data)\n'
            'time.sleep(30)\n'
        )
        master, slave = pty.openpty()
        process = subprocess.Popen(
            [str(RMUX), 'new', 'paste', '--', 'python3', '-u', '-c', program],
            env=self.env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
        )
        os.close(slave)
        self.clients.append((process, master))
        output = bytearray()
        deadline = time.monotonic() + 5
        while b'\x1b[?2004h' not in output and time.monotonic() < deadline:
            if select.select([master], [], [], 0.1)[0]:
                output.extend(os.read(master, 65536))
        self.assertIn(b'\x1b[?2004h', output)
        control = self.attach_control('paste')
        try:
            while True:
                kind, snapshot = self.read_frame(control)
                if kind == BUFFER_SNAPSHOT:
                    self.assertIn(b'\x1b[?2004h', snapshot[12:])
                    break
                self.assertNotEqual(kind, CLOSE)
        finally:
            control.close()
        for chunk in (payload[:3], payload[3:13], payload[13:27], payload[27:]):
            os.write(master, chunk)
            time.sleep(0.03)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if received.exists() and received.read_bytes() == payload:
                break
            if select.select([master], [], [], 0.1)[0]:
                os.read(master, 65536)
        self.assertTrue(received.exists(), 'application did not receive the entire paste')
        self.assertEqual(received.read_bytes(), payload)

    def attach_control(self, name, rows=24, cols=80, resize_step=5):
        sock = socket.socket(socket.AF_UNIX)
        sock.settimeout(8)
        sock.connect(str(Path(self.tmp.name, 'rmux-v2', 'sessions', f'{name}.sock')))
        request = json.dumps({
            'op': 'attach', 'name': name, 'rows': rows, 'cols': cols,
            'resize_step': resize_step,
        }).encode() + b'\n'
        sock.sendall(request)
        response = bytearray()
        while not response.endswith(b'\n'):
            chunk = sock.recv(1)
            if not chunk:
                raise EOFError('session closed before attach response')
            response.extend(chunk)
        self.assertTrue(json.loads(response)['ok'])
        return sock

    def send_frame(self, sock, kind, payload=b''):
        sock.sendall(bytes([kind]) + len(payload).to_bytes(4, 'big') + payload)

    def read_exact(self, sock, length):
        data = bytearray()
        while len(data) < length:
            chunk = sock.recv(length - len(data))
            if not chunk:
                raise EOFError('session closed during frame')
            data.extend(chunk)
        return bytes(data)

    def read_frame(self, sock):
        header = self.read_exact(sock, 5)
        size = int.from_bytes(header[1:], 'big')
        payload = self.read_exact(sock, size)
        return header[0], payload
    def read_layout(self, sock):
        while True:
            kind, payload = self.read_frame(sock)
            if kind == WINDOW_STATE:
                return json.loads(payload)
            if kind == CLOSE:
                self.fail('session closed while waiting for layout state')
            self.assertIn(kind, (BUFFER_SNAPSHOT, BUFFER_OUTPUT))

    def read_buffer_size(self, sock, buffer):
        while True:
            kind, payload = self.read_frame(sock)
            if kind == CLOSE:
                self.fail('session closed while waiting for buffer snapshot')
            if kind == BUFFER_SNAPSHOT:
                self.assertGreaterEqual(len(payload), 12)
                if int.from_bytes(payload[:8], 'big') == buffer:
                    rows = int.from_bytes(payload[8:10], 'big')
                    cols = int.from_bytes(payload[10:12], 'big')
                    return rows, cols
            else:
                self.assertIn(kind, (WINDOW_STATE, BUFFER_OUTPUT))

    def window_leaves(self, node):
        if node['kind'] == 'window':
            return [node]
        return (self.window_leaves(node['first']) +
                self.window_leaves(node['second']))

    def receive_until(self, sock, text):
        output = bytearray()
        while text not in output:
            kind, payload = self.read_frame(sock)
            if kind == WINDOW_STATE:
                continue
            if kind == CLOSE:
                self.fail('session closed while waiting for buffer output')
            if kind == BUFFER_SNAPSHOT:
                self.assertGreaterEqual(len(payload), 12)
                output.extend(payload[12:])
            elif kind == BUFFER_OUTPUT:
                self.assertGreaterEqual(len(payload), 8)
                output.extend(payload[8:])
            else:
                self.fail(f'unexpected server frame {kind}')
        return bytes(output)


    def wait_for(self, predicate):
        for _ in range(100):
            if predicate():
                return
            time.sleep(0.03)
        self.fail('timed out waiting for session state')

    def test_version_flags(self):
        import tomllib

        with Path(__file__).with_name('Cargo.toml').open('rb') as file:
            version = tomllib.load(file)['package']['version']
        for flag in ('--version', '-v'):
            with self.subTest(flag=flag):
                result = self.run_rmux(flag)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(result.stdout, f'rmux {version}\n')
                self.assertEqual(result.stderr, '')

    def test_lifecycle_and_up(self):
        marker = Path(self.tmp.name, 'marker')
        command = f'printf "%s\\n" "$$:$PWD:$1" >> {marker}; exec sh -i'
        first, fd = self.start('new', 'demo', '--cwd', self.tmp.name, '--',
                               'sh', '-c', command, 'sh', 'two words')
        self.wait_for(marker.exists)
        self.assertEqual(marker.read_text().strip().split(':')[1:], [self.tmp.name, 'two words'])
        self.assertEqual(self.run_rmux('ls').stdout, 'demo\n')
        self.assertNotEqual(self.run_rmux('new', 'demo').returncode, 0)
        first.terminate()
        first.wait(timeout=5)
        second, fd2 = self.start('up', 'demo', '--cwd', '/nonexistent', '--', 'false')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'demo\n')
        time.sleep(0.2)
        self.assertIsNone(second.poll())
        self.assertEqual(len(marker.read_text().splitlines()), 1)
        os.write(fd2, b'exit\n')
        self.wait_for(lambda: self.run_rmux('ls').stdout == '')
        third, fd3 = self.start('new', 'demo', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'demo\n')
        os.write(fd3, b'exit\n')
        self.wait_for(lambda: self.run_rmux('ls').stdout == '')

    def test_cursor_position_query_receives_buffer_coordinates(self):
        marker = Path(self.tmp.name, 'cursor-replies')
        script = (
            'import os,select,time,tty; '
            'tty.setraw(0); '
            'os.write(1,b"\\x1b[4;7H\\x1b["); '
            'time.sleep(0.1); '
            'os.write(1,b"6n"); '
            'r,_,_=select.select([0],[],[],2); '
            'response=os.read(0,128) if r else b"TIMEOUT"; '
            f'open({str(marker)!r},"wb").write(response)'
        )
        self.start('new', 'cursor-query', '--', 'python3', '-c', script)
        self.wait_for(marker.exists)
        self.assertEqual(marker.read_bytes(), b'\x1b[4;7R')

    def test_cursor_query_works_without_attached_clients(self):
        ready = Path(self.tmp.name, 'cursor-ready')
        trigger = Path(self.tmp.name, 'cursor-trigger')
        result = Path(self.tmp.name, 'cursor-detached')
        script = (
            'import os,select,time,tty; '
            'tty.setraw(0); '
            f'open({str(ready)!r},"w").close(); '
            f'exec("while not os.path.exists({str(trigger)!r}):\\n time.sleep(0.01)"); '
            'os.write(1,b"\\x1b[3;5H\\x1b[6n"); '
            'r,_,_=select.select([0],[],[],2); '
            'response=os.read(0,128) if r else b"TIMEOUT"; '
            f'open({str(result)!r},"wb").write(response)'
        )
        client, _ = self.start('new', 'cursor-detached', '--', 'python3', '-c', script)
        self.wait_for(ready.exists)
        client.terminate()
        client.wait(timeout=5)
        trigger.touch()
        self.wait_for(result.exists)
        self.assertEqual(result.read_bytes(), b'\x1b[3;5R')

    def test_client_ctrl_t_and_ctrl_w_control_buffers(self):
        process, fd = self.start('new', 'keybindings', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'keybindings\n')
        control = self.attach_control('keybindings')
        try:
            initial = self.read_layout(control)
            self.assertEqual(initial['active_buffer'], 1)

            os.write(fd, b'\x14')
            created = self.read_layout(control)
            self.assertEqual(created['active_buffer'], 2)

            os.write(fd, b'\x17')
            closed = self.read_layout(control)
            self.assertEqual(closed['active_buffer'], 1)

            os.write(fd, b'\x17')
            self.wait_for(lambda: self.run_rmux('ls').stdout == '')
            self.wait_for(lambda: process.poll() is not None)
        finally:
            control.close()

    def test_alt_split_keys_create_shared_windows(self):
        _, fd = self.start('new', 'alt-splits', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'alt-splits\n')
        control = self.attach_control('alt-splits')
        try:
            initial = self.read_layout(control)
            self.assertEqual(initial['focused_window'], 1)
            os.write(fd, b'\x1bv')
            vertical = self.read_layout(control)
            self.assertEqual(vertical['tree']['axis'], 'vertical')
            self.assertEqual(len(self.window_leaves(vertical['tree'])), 2)

            os.write(fd, b'\x1bb')
            horizontal = self.read_layout(control)
            self.assertEqual(horizontal['tree']['second']['axis'], 'horizontal')
            self.assertEqual(len(self.window_leaves(horizontal['tree'])), 3)
        finally:
            control.close()


    def test_rename_word_editing_and_escape(self):
        _, fd = self.start('new', 'word-rename', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'word-rename\n')
        control = self.attach_control('word-rename')
        try:
            self.read_layout(control)
            time.sleep(0.1)
            os.write(fd, b'\x12')
            time.sleep(0.1)
            os.write(fd, '\x15one 雪 three'.encode())
            os.write(fd, b'\x1b[1;5D')
            os.write(fd, b'\x1b\x7f')
            os.write(fd, b'new ')
            os.write(fd, b'\r')
            self.assertEqual(self.read_layout(control)['buffer_names']['1'], 'one new three')

            os.write(fd, b'\x12')
            time.sleep(0.1)
            os.write(fd, b'\x15discard\x1b')
            time.sleep(0.15)
            os.write(fd, b'\x12')
            time.sleep(0.1)
            os.write(fd, b'\x15kept\r')
            self.assertEqual(self.read_layout(control)['buffer_names']['1'], 'kept')
        finally:
            control.close()

    def test_buffer_rename_is_shared_without_changing_buffer_identity_or_order(self):
        self.start('new', 'rename', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'rename\n')
        first = self.attach_control('rename')
        second = None
        third = None
        try:
            initial = self.read_layout(first)
            self.assertEqual(initial['buffer_names'], {'1': '1'})

            def buffer_views(node):
                if node['kind'] == 'window':
                    return [node['buffer']]
                return buffer_views(node['first']) + buffer_views(node['second'])

            self.send_frame(first, SPLIT_VERTICAL)
            split_state = self.read_layout(first)
            self.assertEqual(buffer_views(split_state['tree']), [1, 1])
            self.send_frame(first, NEW_BUFFER)
            created = self.read_layout(first)
            self.assertEqual(created['buffers'], [1, 2])
            self.assertEqual(created['buffer_names'], {'1': '1', '2': '2'})
            self.assertEqual(buffer_views(created['tree']), [1, 2])

            renamed_name = 'renamed 雪'
            self.send_frame(first, RENAME_BUFFER,
                            (1).to_bytes(8, 'big') + renamed_name.encode('utf-8'))
            renamed = self.read_layout(first)
            self.assertEqual(renamed['buffer_names']['1'], renamed_name)
            self.assertEqual(renamed['buffers'], created['buffers'])
            self.assertEqual(renamed['tree'], created['tree'])
            self.assertEqual(renamed['focused_window'], created['focused_window'])
            self.assertEqual(renamed['active_buffer'], created['active_buffer'])

            second = self.attach_control('rename')
            self.assertEqual(self.read_layout(second), renamed)
            for action, expected_views in (
                (PREVIOUS_BUFFER, [1, 1]),
                (NEXT_BUFFER, [1, 2]),
                (PREVIOUS_BUFFER, [1, 1]),
            ):
                self.send_frame(first, action)
                first_state = self.read_layout(first)
                second_state = self.read_layout(second)
                self.assertEqual(first_state, second_state)
                self.assertEqual(buffer_views(first_state['tree']), expected_views)
                self.assertEqual(first_state['buffer_names']['1'], renamed_name)

            self.send_frame(first, RENAME_BUFFER, (1).to_bytes(8, 'big') + b'\xff')
            self.send_frame(first, RENAME_BUFFER,
                            (1).to_bytes(8, 'big') + b'bad' + bytes([10]) + b'name')
            third = self.attach_control('rename')
            invalid_names = self.read_layout(third)
            self.assertEqual(invalid_names['buffer_names']['1'], renamed_name)

            self.send_frame(first, RENAME_BUFFER, (1).to_bytes(8, 'big'))
            reset = self.read_layout(first)
            self.assertEqual(reset['buffer_names']['1'], '1')
            self.assertEqual(reset['buffers'], created['buffers'])
            self.assertEqual(self.read_layout(second), reset)
            self.assertEqual(self.read_layout(third), reset)
        finally:
            first.close()
            if second is not None:
                second.close()
            if third is not None:
                third.close()

    def test_buffers_keep_independent_screen_and_process_state(self):
        self.env['SHELL'] = '/bin/sh'
        process, fd = self.start('new', 'buffers', '--', 'sh', '-i')
        self.wait_for(lambda: 'buffers' in self.run_rmux('ls').stdout.splitlines())
        os.write(fd, b'echo FIRST_BUFFER_MARKER\n')
        control = self.attach_control('buffers')
        try:
            self.assertIn(b'FIRST_BUFFER_MARKER', self.receive_until(
                control, b'FIRST_BUFFER_MARKER'))
            self.send_frame(control, 4)
            pidfile = Path(self.tmp.name, 'buffer-child.pid')
            command = (
                b'echo SECOND_BUFFER_VISIBLE; '
                b'(sleep 2; echo SECOND_BUFFER_HIDDEN) & '
                b'sleep 60 & echo $! > ' + str(pidfile).encode() + b'; wait\n'
            )
            self.send_frame(control, 1, command)
            second_visible = self.receive_until(control, b'SECOND_BUFFER_VISIBLE')
            self.assertIn(b'SECOND_BUFFER_VISIBLE', second_visible)
            self.send_frame(control, 6)
            first_screen = self.receive_until(control, b'FIRST_BUFFER_MARKER')
            current_screen = first_screen.rsplit(b'\x1b[H\x1b[J', 1)[-1]
            self.assertNotIn(b'SECOND_BUFFER_VISIBLE', current_screen)
            time.sleep(2.2)
            self.send_frame(control, 7)
            second_screen = self.receive_until(control, b'SECOND_BUFFER_HIDDEN')
            self.assertIn(b'SECOND_BUFFER_VISIBLE', second_screen)
            self.wait_for(pidfile.exists)
            child_pid = int(pidfile.read_text())

            self.send_frame(control, 5)
            self.assertIn(b'FIRST_BUFFER_MARKER', self.receive_until(
                control, b'FIRST_BUFFER_MARKER'))

            def stopped():
                try:
                    state = Path(f'/proc/{child_pid}/stat').read_text().split(') ')[1][0]
                    return state == 'Z'
                except FileNotFoundError:
                    return True
            self.wait_for(stopped)
            self.assertEqual(self.run_rmux('ls').stdout, 'buffers\n')

            self.send_frame(control, 4)
            self.send_frame(control, 1, b'exit\n')
            self.assertIn(b'FIRST_BUFFER_MARKER', self.receive_until(
                control, b'FIRST_BUFFER_MARKER'))
            self.assertEqual(self.run_rmux('ls').stdout, 'buffers\n')

            self.send_frame(control, 5)
            self.wait_for(lambda: self.run_rmux('ls').stdout == '')
            self.wait_for(lambda: process.poll() is not None)
        finally:
            control.close()

    def test_shared_window_tree_actions(self):
        self.start('new', 'windows', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'windows\n')
        first = self.attach_control('windows')
        second = self.attach_control('windows', resize_step=12)
        last = None
        try:
            initial = self.read_layout(first)
            self.assertEqual(initial, self.read_layout(second))
            self.assertEqual(initial['focused_window'], 1)
            self.assertEqual(initial['active_buffer'], 1)

            self.send_frame(first, SPLIT_VERTICAL)
            vertical = self.read_layout(first)
            self.assertEqual(vertical, self.read_layout(second))
            self.assertEqual(vertical['tree']['axis'], 'vertical')
            self.assertEqual(
                [(window['id'], window['buffer'])
                 for window in self.window_leaves(vertical['tree'])],
                [(1, 1), (2, 1)],
            )
            self.assertEqual(vertical['focused_window'], 2)

            self.send_frame(second, SPLIT_HORIZONTAL)
            split = self.read_layout(first)
            self.assertEqual(split, self.read_layout(second))
            self.assertEqual(split['tree']['second']['axis'], 'horizontal')
            self.assertEqual(
                [window['id'] for window in self.window_leaves(split['tree'])],
                [1, 2, 3],
            )
            self.assertEqual(split['focused_window'], 3)

            self.send_frame(first, FOCUS_WINDOW, b'h')
            left = self.read_layout(first)
            self.assertEqual(left, self.read_layout(second))
            self.assertEqual(left['focused_window'], 1)

            self.send_frame(first, FOCUS_WINDOW, b'l')
            right = self.read_layout(first)
            self.assertEqual(right, self.read_layout(second))
            self.assertIn(right['focused_window'], [2, 3])
            if right['focused_window'] == 3:
                self.send_frame(first, FOCUS_WINDOW, b'k')
                self.assertEqual(
                    self.read_layout(first),
                    self.read_layout(second),
                )

            self.send_frame(first, FOCUS_WINDOW, b'j')
            down = self.read_layout(first)
            self.assertEqual(down, self.read_layout(second))
            self.assertEqual(down['focused_window'], 3)
            self.send_frame(first, FOCUS_WINDOW, b'k')
            focused = self.read_layout(first)
            self.assertEqual(focused, self.read_layout(second))
            self.assertEqual(focused['focused_window'], 2)

            self.send_frame(first, MOVE_WINDOW, b'h')
            moved_first = self.read_layout(first)
            moved = self.read_layout(second)
            self.assertEqual(moved_first, moved)
            self.assertEqual(moved['focused_window'], 2)
            self.assertEqual(
                [window['id'] for window in self.window_leaves(moved['tree'])],
                [2, 1, 3],
            )

            self.send_frame(second, RESIZE_WINDOW, b'l')
            resized = self.read_layout(first)
            self.assertEqual(resized, self.read_layout(second))
            self.assertEqual(resized['tree']['ratio'], 62)

            self.send_frame(first, CLOSE_WINDOW)
            closed = self.read_layout(first)
            self.assertEqual(closed, self.read_layout(second))
            self.assertEqual(
                [window['id'] for window in self.window_leaves(closed['tree'])],
                [1, 3],
            )
            self.assertEqual(closed['focused_window'], 1)

            self.send_frame(first, CLOSE_WINDOW)
            one_window = self.read_layout(first)
            self.assertEqual(one_window, self.read_layout(second))
            self.assertEqual(
                [window['id'] for window in self.window_leaves(one_window['tree'])],
                [3],
            )
            self.send_frame(first, CLOSE_WINDOW)
            last = self.attach_control('windows')
            remaining = self.read_layout(last)
            self.assertEqual(
                [window['id'] for window in self.window_leaves(remaining['tree'])],
                [3],
            )
            self.assertEqual(self.run_rmux('ls').stdout, 'windows\n')
        finally:
            first.close()
            second.close()
            if last is not None:
                last.close()


    def test_equalize_balances_layout_and_broadcasts_to_attachments(self):
        _, terminal = self.start('new', 'equalize', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'equalize\n')
        first = self.attach_control('equalize')
        second = self.attach_control('equalize')
        try:
            initial = self.read_layout(first)
            self.assertEqual(initial, self.read_layout(second))

            self.send_frame(first, SPLIT_VERTICAL)
            self.read_layout(first)
            self.read_layout(second)
            self.send_frame(second, SPLIT_HORIZONTAL)
            uneven = self.read_layout(first)
            self.assertEqual(uneven, self.read_layout(second))
            self.assertEqual(uneven['tree']['ratio'], 50)
            self.assertEqual(uneven['tree']['second']['ratio'], 50)

            os.write(terminal, b'\x1b=')
            balanced = self.read_layout(first)
            self.assertEqual(balanced, self.read_layout(second))
            self.assertEqual(balanced['tree']['ratio'], 33)
            self.assertEqual(balanced['tree']['second']['ratio'], 50)
            self.assertEqual(balanced['tree']['axis'], 'vertical')
            self.assertEqual(balanced['tree']['second']['axis'], 'horizontal')
            self.assertEqual(balanced['focused_window'], uneven['focused_window'])
            self.assertEqual(
                [(window['id'], window['buffer'])
                 for window in self.window_leaves(balanced['tree'])],
                [(window['id'], window['buffer'])
                 for window in self.window_leaves(uneven['tree'])],
            )
        finally:
            first.close()
            second.close()
    def test_buffer_size_uses_smallest_pane_across_clients(self):
        self.start('new', 'geometry', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'geometry\n')
        large = self.attach_control('geometry', rows=24, cols=80)
        small = None
        try:
            self.assertEqual(self.read_buffer_size(large, 1), (23, 80))

            small = self.attach_control('geometry', rows=12, cols=50)
            self.assertEqual(self.read_buffer_size(small, 1), (11, 50))
            self.assertEqual(self.read_buffer_size(large, 1), (11, 50))

            self.send_frame(large, SPLIT_VERTICAL)
            split = self.read_layout(large)
            self.assertEqual(split, self.read_layout(small))
            self.assertEqual(self.read_buffer_size(large, 1), (11, 24))
            self.assertEqual(self.read_buffer_size(small, 1), (11, 24))

            resize = (10).to_bytes(2, 'big') + (30).to_bytes(2, 'big')
            self.send_frame(small, CLIENT_RESIZE, resize)
            self.assertEqual(self.read_buffer_size(large, 1), (9, 14))
            self.assertEqual(self.read_buffer_size(small, 1), (9, 14))

            self.send_frame(small, CLOSE_WINDOW)
            single = self.read_layout(small)
            self.assertEqual(single, self.read_layout(large))
            self.assertEqual(self.read_buffer_size(large, 1), (9, 30))
            self.assertEqual(self.read_buffer_size(small, 1), (9, 30))
            small.close()
            small = None
            self.assertEqual(self.read_buffer_size(large, 1), (23, 80))
        finally:
            large.close()
            if small is not None:
                small.close()

    def test_buffer_views_switch_close_and_exit_independently(self):
        self.env['SHELL'] = '/bin/sh'
        self.start('new', 'window-buffers', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'window-buffers\n')
        control = self.attach_control('window-buffers')
        try:
            self.read_layout(control)
            self.send_frame(control, NEW_BUFFER)
            created = self.read_layout(control)
            self.assertEqual(created['buffers'], [1, 2])
            self.assertEqual(created['active_buffer'], 2)
            self.send_frame(
                control,
                INPUT,
                b'echo HIDDEN_WINDOW_BUFFER_MARKER\n',
            )
            self.assertIn(
                b'HIDDEN_WINDOW_BUFFER_MARKER',
                self.receive_until(control, b'HIDDEN_WINDOW_BUFFER_MARKER'),
            )

            self.send_frame(control, SPLIT_VERTICAL)
            split = self.read_layout(control)
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(split['tree'])],
                [2, 2],
            )

            self.send_frame(control, FOCUS_WINDOW, b'h')
            self.assertEqual(self.read_layout(control)['focused_window'], 1)
            self.send_frame(control, PREVIOUS_BUFFER)
            switched = self.read_layout(control)
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(switched['tree'])],
                [1, 2],
            )

            self.send_frame(control, FOCUS_WINDOW, b'l')
            self.assertEqual(self.read_layout(control)['focused_window'], 2)
            self.send_frame(control, CLOSE_WINDOW)
            closed_window = self.read_layout(control)
            self.assertEqual(closed_window['buffers'], [1, 2])
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(closed_window['tree'])],
                [1],
            )
            self.send_frame(control, NEXT_BUFFER)
            restored = self.read_layout(control)
            self.assertEqual(restored['active_buffer'], 2)
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(restored['tree'])],
                [2],
            )
            self.assertIn(
                b'HIDDEN_WINDOW_BUFFER_MARKER',
                self.receive_until(control, b'HIDDEN_WINDOW_BUFFER_MARKER'),
            )

            self.send_frame(control, SPLIT_VERTICAL)
            split = self.read_layout(control)
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(split['tree'])],
                [2, 2],
            )
            self.send_frame(control, PREVIOUS_BUFFER)
            switched = self.read_layout(control)
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(switched['tree'])],
                [2, 1],
            )
            self.send_frame(control, REORDER_BUFFER_RIGHT)
            reordered = self.read_layout(control)
            self.assertEqual(reordered['buffers'], [2, 1])

            self.send_frame(control, NEXT_BUFFER)
            self.assertEqual(self.read_layout(control)['active_buffer'], 2)
            self.send_frame(control, CLOSE_BUFFER)
            closed = self.read_layout(control)
            self.assertEqual(closed['buffers'], [1])
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(closed['tree'])],
                [1, 1],
            )

            self.send_frame(control, NEW_BUFFER)
            self.assertEqual(self.read_layout(control)['active_buffer'], 3)
            self.send_frame(control, INPUT, b'exit\n')
            exited = self.read_layout(control)
            self.assertEqual(exited['buffers'], [1])
            self.assertEqual(
                [window['buffer'] for window in self.window_leaves(exited['tree'])],
                [1, 1],
            )
            self.assertEqual(self.run_rmux('ls').stdout, 'window-buffers\n')
        finally:
            control.close()

    def test_stale_hidden_and_invalid_names(self):
        self.run_rmux('ls')
        stale = Path(self.tmp.name, 'rmux-v2', 'sessions', '.hidden.sock')
        stale.parent.mkdir(parents=True, exist_ok=True)
        sock = socket.socket(socket.AF_UNIX)
        sock.bind(str(stale))
        sock.close()
        self.assertEqual(self.run_rmux('ls').stdout, '')
        process, fd = self.start('new', '.hidden', '--', 'sh', '-i')
        self.wait_for(lambda: self.run_rmux('ls').stdout == '.hidden\n')
        self.assertNotEqual(self.run_rmux('attach', '../bad').returncode, 0)
        self.assertNotEqual(self.run_rmux('attach', 'missing').returncode, 0)
        os.write(fd, b'exit\n')
        self.wait_for(lambda: self.run_rmux('ls').stdout == '')

    def test_default_shell_and_creation_errors(self):
        process, fd = self.start('new', 'shell')
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'shell\n')
        os.write(fd, b'exit\n')
        self.wait_for(lambda: self.run_rmux('ls').stdout == '')
        process, fd = self.start('new', 'bad-cwd', '--cwd', '/does-not-exist')
        self.assertNotEqual(process.wait(timeout=5), 0)
        self.assertEqual(self.run_rmux('ls').stdout, '')
        self.assertNotEqual(self.run_rmux('new', 'x/y').returncode, 0)

    def test_concurrent_up_creates_one_process(self):
        marker = Path(self.tmp.name, 'count')
        command = f'echo started >> {marker}; exec sh -i'
        first, fd1 = self.start('up', 'race', '--', 'sh', '-c', command)
        second, fd2 = self.start('up', 'race', '--', 'sh', '-c', command)
        self.wait_for(marker.exists)
        time.sleep(0.3)
        self.assertEqual(marker.read_text().splitlines(), ['started'])
        self.assertIsNone(first.poll())
        self.assertIsNone(second.poll())
        os.write(fd1, b'exit\n')
        self.wait_for(lambda: self.run_rmux('ls').stdout == '')


    def test_kill_stops_process_tree_and_attached_clients(self):
        marker = Path(self.tmp.name, 'pids')
        script = ('import os,signal,subprocess,time; '
                  'signal.signal(signal.SIGHUP, signal.SIG_IGN); '
                  'child=subprocess.Popen(["sleep","60"]); '
                  f'open({str(marker)!r},"w").write(str(os.getpid())+" "+str(child.pid)); '
                  'time.sleep(60)')
        first, fd1 = self.start('new', 'target', '--', 'python3', '-c', script)
        other, fd2 = self.start('new', 'other', '--', 'sh', '-i')
        self.wait_for(marker.exists)
        self.wait_for(lambda: 'other' in self.run_rmux('ls').stdout.splitlines())
        attached, fd3 = self.start('attach', 'target')
        time.sleep(0.1)
        self.assertIsNone(attached.poll())
        pids = [int(pid) for pid in marker.read_text().split()]
        result = self.run_rmux('kill', 'target')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.wait_for(lambda: first.poll() is not None and attached.poll() is not None)
        def stopped(pid):
            try:
                return Path(f'/proc/{pid}/stat').read_text().split(') ')[1][0] == 'Z'
            except FileNotFoundError:
                return True
        self.wait_for(lambda: all(stopped(pid) for pid in pids))
        self.assertEqual(self.run_rmux('ls').stdout, 'other\n')
        self.assertIsNone(other.poll())
        replacement, fd4 = self.start('new', 'target', '--', 'sh', '-i')
        self.wait_for(lambda: 'target' in self.run_rmux('ls').stdout.splitlines())
        os.write(fd4, b'exit\n')

    def test_kill_rejects_untracked_and_bad_identity(self):
        self.run_rmux('ls')
        socket_path = Path(self.tmp.name, 'rmux-v2', 'sessions', 'old.sock')
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(socket_path))
        listener.listen()
        listener.settimeout(0.1)
        stopping = threading.Event()

        def answer_probes():
            while not stopping.is_set():
                try:
                    connection, _ = listener.accept()
                except TimeoutError:
                    continue
                except OSError:
                    return
                with connection:
                    request = b''
                    while not request.endswith(b'\n'):
                        chunk = connection.recv(4096)
                        if not chunk:
                            break
                        request += chunk
                    response = {
                        'ok': True, 'name': 'old',
                        'pid': os.getpid(), 'start_time': 1,
                    }
                    connection.sendall(json.dumps(response).encode() + b'\n')

        probe_server = threading.Thread(target=answer_probes)
        probe_server.start()
        try:
            result = self.run_rmux('kill', 'old')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('old', self.run_rmux('ls').stdout.splitlines())
        finally:
            stopping.set()
            listener.close()
            probe_server.join(timeout=2)
            socket_path.unlink(missing_ok=True)
        process, fd = self.start('new', 'tracked', '--', 'sh', '-i')
        self.wait_for(lambda: 'tracked' in self.run_rmux('ls').stdout.splitlines())
        pidfile = Path(self.tmp.name, 'rmux-v2', 'pids', 'tracked')
        pidfile.write_text(f'{os.getpid()} 0\n')
        self.assertNotEqual(self.run_rmux('kill', 'tracked').returncode, 0)
        self.assertIsNone(process.poll())
        self.assertNotEqual(self.run_rmux('kill', 'missing').returncode, 0)
        self.assertNotEqual(self.run_rmux('kill', '../bad').returncode, 0)
        self.assertIn('kill', self.run_rmux('--help').stdout)
        self.assertIn('kill', self.run_rmux().stderr)
        os.write(fd, b'exit\n')

    def test_kill_rejects_other_sessions_identity(self):
        first, fd1 = self.start('new', 'first', '--', 'sh', '-i')
        second, fd2 = self.start('new', 'second', '--', 'sh', '-i')
        first_id = Path(self.tmp.name, 'rmux-v2', 'pids', 'first')
        second_id = Path(self.tmp.name, 'rmux-v2', 'pids', 'second')
        self.wait_for(lambda: first_id.exists() and second_id.exists())
        first_id.write_text(second_id.read_text())
        result = self.run_rmux('kill', 'first')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('session identity no longer matches', result.stderr)
        self.assertIsNone(first.poll())
        self.assertIsNone(second.poll())
        os.write(fd1, b'exit\n')
        os.write(fd2, b'exit\n')

    def test_kill_and_up_do_not_kill_replacement(self):
        first, fd = self.start('new', 'race', '--', 'sh', '-i')
        self.wait_for(lambda: 'race' in self.run_rmux('ls').stdout.splitlines())
        lock_path = Path(self.tmp.name, 'rmux-v2', 'locks', 'race')
        with lock_path.open('r') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            killer = subprocess.Popen([str(RMUX), 'kill', 'race'], env=self.env,
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            time.sleep(0.1)
            replacement, fd2 = self.start('up', 'race', '--', 'sh', '-i')
            time.sleep(0.1)
            fcntl.flock(lock, fcntl.LOCK_UN)
        stdout, stderr = killer.communicate(timeout=5)
        self.assertEqual(killer.returncode, 0, stderr.decode())
        self.wait_for(lambda: first.poll() is not None)
        self.wait_for(lambda: 'race' in self.run_rmux('ls').stdout.splitlines())
        self.assertIsNone(replacement.poll())
        os.write(fd2, b'exit\n')

if __name__ == '__main__':
    unittest.main()
