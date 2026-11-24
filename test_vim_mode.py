import fcntl
import json
import os
import pty
import shlex
import shutil
import select
import socket
import subprocess
import struct
import termios
import tempfile
import signal
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
VIM_STATE = 7
NEW_BUFFER = 4
PREVIOUS_BUFFER = 6
SPLIT_VERTICAL = 8
HISTORY_SNAPSHOT = 6
HISTORY_VIEW = 18


class VimModeSessions(unittest.TestCase):
    """Exercise Vim mode through real rmux clients and their PTYs."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='rmux-vim-')
        self.root = Path(self.tmp.name)
        config_dir = self.root / 'config'
        config_dir.mkdir()
        self.env = dict(os.environ, XDG_RUNTIME_DIR=str(self.root),
                        XDG_CONFIG_HOME=str(config_dir), HOME=str(self.root),
                        TERM='xterm')
        self.clients = []
        self.controls = []
        self.vim_states = {}
        self.buffer_output = bytearray()
        self.layout = None
        self.reader_threads = []

    def tearDown(self):
        for sock in self.controls:
            try:
                sock.close()
            except OSError:
                pass
        try:
            names = subprocess.run([str(RMUX), 'ls'], env=self.env, text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   timeout=5).stdout.splitlines()
        except (OSError, subprocess.TimeoutExpired):
            names = []
        for name in names:
            subprocess.run([str(RMUX), 'kill', name], env=self.env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                           timeout=5)
        for process, fd in self.clients:
            if process.poll() is None:
                process.terminate()
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            try:
                os.close(fd)
            except OSError:
                pass
        self.tmp.cleanup()

    def start_client(self, *args, env=None):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
        process = subprocess.Popen([str(RMUX), *args], env=env or self.env,
                                   stdin=slave, stdout=slave, stderr=slave,
                                   start_new_session=True)
        os.close(slave)
        self.clients.append((process, master))
        thread = threading.Thread(target=self._drain_terminal, args=(master,), daemon=True)
        thread.start()
        self.reader_threads.append(thread)
        return process, master

    @staticmethod
    def _drain_terminal(fd):
        while True:
            try:
                os.read(fd, 65536)
            except OSError:
                return
    def run_rmux(self, *args):
        return subprocess.run([str(RMUX), *args], env=self.env, text=True,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)

    def attach_control(self, name, rows=24, cols=80, observe=True):
        sock = socket.socket(socket.AF_UNIX)
        sock.settimeout(8)
        sock.connect(str(self.root / 'rmux-v2' / 'sessions' / f'{name}.sock'))
        request = json.dumps({'op': 'attach', 'name': name, 'rows': rows,
                              'cols': cols, 'resize_step': 5}).encode() + b'\n'
        sock.sendall(request)
        response = bytearray()
        while not response.endswith(b'\n'):
            part = sock.recv(1)
            if not part:
                raise EOFError('session closed before attach response')
            response.extend(part)
        self.assertTrue(json.loads(response)['ok'])
        self.controls.append(sock)
        if observe:
            threading.Thread(target=self._read_control, args=(sock,), daemon=True).start()
        return sock

    def _read_control(self, sock):
        try:
            while True:
                kind, payload = self.read_frame(sock)
                if kind == VIM_STATE:
                    state = json.loads(payload)
                    self.vim_states[state['buffer']] = state
                elif kind == BUFFER_OUTPUT:
                    self.buffer_output.extend(payload[8:])
                elif kind == BUFFER_SNAPSHOT:
                    self.buffer_output.extend(payload[12:])
                elif kind == WINDOW_STATE:
                    self.layout = json.loads(payload)
                elif kind == CLOSE:
                    return
        except (OSError, EOFError):
            return

    @staticmethod
    def send_frame(sock, kind, payload=b''):
        sock.sendall(bytes([kind]) + len(payload).to_bytes(4, 'big') + payload)

    @staticmethod
    def read_exact(sock, size):
        data = bytearray()
        while len(data) < size:
            part = sock.recv(size - len(data))
            if not part:
                raise EOFError('session closed during control frame')
            data.extend(part)
        return bytes(data)

    @classmethod
    def read_frame(cls, sock):
        header = cls.read_exact(sock, 5)
        return header[0], cls.read_exact(sock, int.from_bytes(header[1:], 'big'))

    def wait_for(self, predicate, message='timed out waiting for rmux state', timeout=8):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.02)
        self.fail(message)

    def send_keys(self, terminal, data):
        os.write(terminal, data)
        time.sleep(0.1)

    def start_shell_session(self, name='vim', command='sleep 30', env=None):
        process, terminal = self.start_client('new', name, '--', 'sh', '-c', command, env=env)
        self.wait_for(lambda: self.run_rmux('ls').stdout == f'{name}\n',
                      'rmux CLI did not start the session')
        control = self.attach_control(name)
        return process, terminal, control

    def latest_state(self, buffer=None):
        if buffer is not None:
            return self.vim_states.get(buffer)
        return next(iter(self.vim_states.values()), None)

    def test_insert_mode_forwards_application_input_unchanged(self):
        received = self.root / 'input'
        ready = self.root / 'ready'
        payload = b'insert passthrough\n'
        script = (
            'import os,tty; tty.setraw(0); ' + f'open({str(ready)!r},"w").close(); '
            f'data=bytearray(); '
            f'exec("while len(data) < {len(payload)}:\\n data.extend(os.read(0, {len(payload)}-len(data)))"); '
            f'open({str(received)!r},"wb").write(data)'
        )
        _, terminal = self.start_client('new', 'insert', '--', 'python3', '-u', '-c', script)
        self.wait_for(lambda: self.run_rmux('ls').stdout == 'insert\n')
        self.wait_for(ready.exists, 'application did not enter raw mode')
        self.attach_control('insert')
        self.send_keys(terminal, payload)
        self.wait_for(received.exists, 'application did not receive Insert-mode input')
        self.assertEqual(received.read_bytes(), payload)

    def test_remapped_escape_preserves_following_application_input(self):
        config = self.root / 'config' / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('[bindings]\nvim_normal = "ctrl+q"\nvim_left = "x"\n')
        received = self.root / 'received'
        ready = self.root / 'ready'
        script = ('import os,tty,time; tty.setraw(0); '
                  f'open({str(ready)!r},"w").close(); '
                  'data=os.read(0,1); '
                  f'open({str(received)!r},"wb").write(data); time.sleep(30)')
        _, terminal, _ = self.start_shell_session(command='python3 -u -c ' + shlex.quote(script))
        self.wait_for(ready.exists)
        self.send_keys(terminal, b'\x11')
        self.wait_for(lambda: self.latest_state() and self.latest_state()['mode'] == 'normal')
        os.write(terminal, b'\x11x')
        self.wait_for(lambda: received.exists() and received.stat().st_size == 1)
        self.assertEqual(received.read_bytes(), b'x')

    def test_insert_input_does_not_publish_redundant_snapshots(self):
        ready = self.root / 'ready'
        received = self.root / 'received'
        script = ('import os,tty,time; tty.setraw(0); '
                  f'open({str(ready)!r},"w").close(); '
                  'data=os.read(0,1); '
                  f'open({str(received)!r},"wb").write(data); time.sleep(30)')
        _, terminal, _ = self.start_shell_session(command='python3 -u -c ' + shlex.quote(script))
        self.wait_for(ready.exists)
        observer = self.attach_control('vim', observe=False)
        observer.settimeout(0.2)
        try:
            while True:
                self.read_frame(observer)
        except socket.timeout:
            pass
        os.write(terminal, b'x')
        self.wait_for(lambda: received.exists() and received.stat().st_size == 1)
        self.assertEqual(received.read_bytes(), b'x')
        with self.assertRaises(socket.timeout):
            self.read_frame(observer)

    def test_alt_escape_enters_normal_and_i_or_escape_returns_to_insert(self):
        _, terminal, _ = self.start_shell_session()
        self.wait_for(lambda: self.latest_state() is not None, 'missing initial Vim state')
        buffer = self.latest_state()['buffer']
        self.assertEqual(self.latest_state(buffer)['mode'], 'insert')

        self.send_keys(terminal, b'\x1b\x1b')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'normal',
                      'Alt+Esc did not enter Normal mode')
        self.send_keys(terminal, b'i')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'insert',
                      'i did not enter Insert mode')
        self.send_keys(terminal, b'\x1b\x1b')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'normal')
        self.send_keys(terminal, b'\x1b')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'insert',
                      'Esc did not return to Insert mode')

    def test_alt_scroll_repeats_in_normal_and_visual_modes(self):
        _, terminal, _ = self.start_shell_session(command="seq 1 100; sleep 30")
        self.wait_for(lambda: self.latest_state() and
                      self.latest_state()['history_len'] >= 20)
        buffer = self.latest_state()['buffer']
        for key, expected in [(b'\x1bd', 5), (b'\x1bd', 10), (b'\x1bf', 5)]:
            self.send_keys(terminal, key)
            self.wait_for(lambda: self.latest_state(buffer)['scrollback'] == expected,
                          f'Alt scroll did not reach {expected} rows')
            self.assertEqual(self.latest_state(buffer)['mode'], 'normal')

        self.send_keys(terminal, b'v')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'visual')
        self.send_keys(terminal, b'\x1bd')
        self.wait_for(lambda: self.latest_state(buffer)['scrollback'] == 10)
        self.assertEqual(self.latest_state(buffer)['mode'], 'visual')
        self.assertIsNotNone(self.latest_state(buffer)['selection'])

    def test_visual_shape_switch_preserves_selection(self):
        _, terminal, _ = self.start_shell_session(command="printf 'abcdef\\033[H'; sleep 30")
        self.wait_for(lambda: self.latest_state() is not None)
        buffer = self.latest_state()['buffer']
        self.send_keys(terminal, b'\x1b\x1b')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'normal')
        self.send_keys(terminal, b'\x16l')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'visual-block' and
                      self.latest_state(buffer)['cursor_col'] == 1)
        initial = self.latest_state(buffer)['selection']
        for key, mode in [(b'V', 'visual-line'), (b'v', 'visual'), (b'\x16', 'visual-block')]:
            self.send_keys(terminal, key)
            self.wait_for(lambda: self.latest_state(buffer)['mode'] == mode)
            self.assertEqual(self.latest_state(buffer)['selection'], initial)

    def test_visual_yank_writes_exact_selection_to_fake_xclip(self):
        clipboard = self.root / 'clipboard'
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        xclip = bin_dir / 'xclip'
        xclip.write_text('#!/usr/bin/python3\nimport sys\nsys.stdout.write("")\nopen(' +
                         repr(str(clipboard)) + ', "wb").write(sys.stdin.buffer.read())\n')
        xclip.chmod(0o755)
        env = dict(self.env, PATH=f'{bin_dir}:{self.env.get("PATH", "")}')
        command = "printf 'alpha beta\\033[H'; sleep 30"
        _, terminal, _ = self.start_shell_session(command=command, env=env)
        self.wait_for(lambda: self.latest_state() is not None, 'missing initial Vim state')
        buffer = self.latest_state()['buffer']
        self.send_keys(terminal, b'\x1b\x1b')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'normal')
        self.send_keys(terminal, b'v')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'visual')
        self.send_keys(terminal, b'l')
        self.send_keys(terminal, b'y')
        self.wait_for(clipboard.exists, 'visual yank did not invoke xclip')
        self.assertEqual(clipboard.read_bytes(), b'al')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'insert')

    def test_duplicate_windows_and_clients_share_cursor_but_buffers_do_not(self):
        _, terminal, control = self.start_shell_session(command="printf 'abcdef\\033[H'; sleep 30")
        self.wait_for(lambda: self.latest_state() is not None, 'missing initial Vim state')
        original = self.latest_state()['buffer']
        self.send_frame(control, SPLIT_VERTICAL)
        self.wait_for(lambda: self.layout is not None and
                      len(self._window_buffers(self.layout['tree'])) == 2,
                      'split did not create two windows')
        self.assertEqual(self._window_buffers(self.layout['tree']), [original, original])

        second, second_terminal = self.start_client('attach', 'vim')
        self.wait_for(lambda: second.poll() is None, 'second rmux client exited')
        self.send_keys(terminal, b'\x1b\x1bl')
        self.wait_for(lambda: self.latest_state(original) and
                      self.latest_state(original)['mode'] == 'normal' and
                      self.latest_state(original)['cursor_col'] == 1,
                      'cursor movement was not shared by both windows/clients')
        self.send_keys(second_terminal, b'l')
        self.wait_for(lambda: self.latest_state(original) and
                      self.latest_state(original)['cursor_col'] == 2,
                      'second client did not move the shared cursor')
        shared_col = self.latest_state(original)['cursor_col']

        self.send_frame(control, NEW_BUFFER)
        self.wait_for(lambda: self.layout is not None and
                      any(buffer != original for buffer in self._window_buffers(self.layout['tree'])),
                      'new buffer did not appear')
        self.send_frame(control, INPUT, b'\x1b\x1b0l')
        self.wait_for(lambda: any(buffer != original and state['mode'] == 'normal' and
                                  state['cursor_col'] == 1 for buffer, state in self.vim_states.items()),
                      'new buffer did not receive independent cursor movement')
        self.send_frame(control, PREVIOUS_BUFFER)
        self.wait_for(lambda: self.latest_state(original) is not None and
                      self.latest_state(original)['cursor_col'] == shared_col,
                      'switching buffers changed the original buffer cursor')
        self.assertIsNone(second.poll())
        os.write(second_terminal, b'\x1b')

    @staticmethod
    def _window_buffers(node):
        if node['kind'] == 'window':
            return [node['buffer']]
        return VimModeSessions._window_buffers(node['first']) + VimModeSessions._window_buffers(node['second'])
    @unittest.skipUnless(shutil.which('nvim'), 'Neovim is not installed')
    def test_neovim_crash_restores_app_control_without_affecting_other_buffers(self):
        hook = Path(__file__).with_name('contrib') / 'rmux.nvim.lua'
        pid_file = self.root / 'nvim.pid'
        capture_pid = ('lua vim.fn.writefile({tostring(vim.fn.getpid())}, ' +
                       json.dumps(str(pid_file)) + ')')
        command = ('nvim -u NONE -i NONE --cmd ' +
                   shlex.quote(f'luafile {hook}') + ' --cmd ' +
                   shlex.quote(capture_pid) + '; sleep 30')
        _, terminal, control = self.start_shell_session(command=command)
        self.wait_for(lambda: self.latest_state() and self.latest_state()['app_control'],
                      'Neovim did not disable Vim bindings')
        self.wait_for(pid_file.exists, 'Neovim did not report its own PID')
        controlled_buffer = self.latest_state()['buffer']

        self.send_frame(control, NEW_BUFFER)
        self.wait_for(lambda: any(buffer != controlled_buffer for buffer in self.vim_states),
                      'new rmux buffer did not appear')
        other_buffer = next(buffer for buffer in self.vim_states if buffer != controlled_buffer)
        self.assertFalse(self.latest_state(other_buffer)['app_control'])

        nvim_pid = int(pid_file.read_text().strip())
        proc = Path('/proc') / str(nvim_pid)
        try:
            executable = (proc / 'exe').resolve(strict=True)
            command_line = (proc / 'cmdline').read_bytes().split(b'\0')
            start_time = (proc / 'stat').read_text().split()[21]
        except (FileNotFoundError, ProcessLookupError):
            self.fail('Neovim exited before the crash scenario')
        self.assertEqual(executable.name, 'nvim',
                         'captured PID is not the Neovim executable')
        self.assertIn(f'luafile {hook}'.encode(), command_line,
                      'captured PID did not load the rmux Neovim hook')

        # A pidfd pins the process identity so PID reuse cannot redirect the
        # signal to an unrelated process.
        pidfd = os.pidfd_open(nvim_pid)
        try:
            self.assertEqual((proc / 'stat').read_text().split()[21], start_time,
                             'captured PID was reused before it could be killed')
            self.assertEqual((proc / 'exe').resolve(strict=True), executable)
            signal.pidfd_send_signal(pidfd, signal.SIGKILL)
        finally:
            os.close(pidfd)
        self.wait_for(lambda: not (proc / 'stat').exists(),
                      'Neovim backend did not exit after SIGKILL')
        self.wait_for(lambda: self.latest_state(controlled_buffer) is not None and
                      not self.latest_state(controlled_buffer)['app_control'],
                      'rmux did not restore bindings after Neovim died')
        self.assertFalse(self.latest_state(other_buffer)['app_control'],
                         'Neovim death changed app_control in another buffer')

        self.send_frame(control, PREVIOUS_BUFFER)
        self.wait_for(lambda: self.layout is not None and
                      controlled_buffer in self._window_buffers(self.layout['tree']),
                      'could not return to the recovered Neovim buffer')
        self.send_keys(terminal, b'\x1b\x1b')
        self.wait_for(lambda: self.latest_state(controlled_buffer) and
                      self.latest_state(controlled_buffer)['mode'] == 'normal',
                      'rmux shortcuts did not work after Neovim died')

    @unittest.skipUnless(shutil.which('nvim'), 'Neovim is not installed')
    def test_neovim_and_omp_are_isolated_in_simultaneous_buffers(self):
        if not shutil.which('omp'):
            self.skipTest('OMP is not installed')

        extension = Path(__file__).resolve().parents[3] / 'dotFiles/omp/agent/extensions/rerender.ts'
        if not extension.is_file():
            self.skipTest('OMP rerender extension is not installed')

        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        (bin_dir / 'xclip').write_text('#!/bin/sh\ncat >/dev/null\n')
        (bin_dir / 'xclip').chmod(0o755)
        omp_dir = self.root / 'omp'
        omp_dir.mkdir()
        env = dict(self.env, PI_CODING_AGENT_DIR=str(omp_dir),
                   OMP_SKIP_SETUP='1',
                   OPENAI_API_KEY='rmux-test-not-a-real-key',
                   PATH=f'{bin_dir}:{self.env.get("PATH", "")}')
        command = ['omp', '--no-session', '--no-extensions', '-e', str(extension),
                   '--no-context-files', '--no-skills', '--no-rules',
                   '--model', 'openai/gpt-5.2', '--offline', '--no-title']
        _, terminal, control = self.start_shell_session(
            name='omp-nvim', command='exec ' + shlex.join(command), env=env)
        self.wait_for(lambda: self.latest_state() is not None and
                      self.latest_state().get('mode') == 'insert' and
                      b'state=idle:app=omp' in self.buffer_output,
                      'OMP did not enter its interactive prompt')
        omp_buffer = self.latest_state()['buffer']

        self.send_frame(control, NEW_BUFFER)
        self.wait_for(lambda: self.layout is not None and
                      any(buffer != omp_buffer for buffer in self._window_buffers(self.layout['tree'])),
                      'new buffer did not appear')
        nvim_buffer = next(buffer for buffer in self.vim_states if buffer != omp_buffer)
        hook = Path(__file__).with_name('contrib') / 'rmux.nvim.lua'
        command = ('nvim -u NONE -i NONE --cmd ' +
                   shlex.quote(f'luafile {hook}'))
        self.send_keys(terminal, ('exec ' + command + '\r').encode())
        self.wait_for(lambda: self.latest_state(nvim_buffer) and
                      self.latest_state(nvim_buffer)['app_control'],
                      'Neovim did not acquire application control')

        marker = 'RMUX_NVIM_INPUT_MARKER'
        written = self.root / 'nvim-input'
        self.send_keys(terminal, b'i' + marker.encode() + b'\x1b')
        write_command = (':lua vim.fn.writefile(vim.api.nvim_buf_get_lines(0, 0, -1, false), ' +
                         json.dumps(str(written)) + ')\r')
        self.send_keys(terminal, write_command.encode())
        self.wait_for(written.exists, 'Neovim did not execute typed insert and Lua commands')
        self.assertEqual(written.read_text(), marker + '\n')
        self.assertTrue(self.latest_state(nvim_buffer)['app_control'],
                        'Neovim input unexpectedly released application control')

        self.send_frame(control, PREVIOUS_BUFFER)
        self.wait_for(lambda: self.layout is not None and
                      omp_buffer in self._window_buffers(self.layout['tree']),
                      'could not return to the OMP buffer')
        shell_command = ("!printf '%s\\n' OMP_NVIM_TRANSCRIPT_\"\"MARKER "
                         "OMP_NVIM_SECOND_LINE OMP_NVIM_THIRD_LINE")
        self.send_keys(terminal, shell_command.encode() + b'\r')
        self.wait_for(lambda: b'OMP_NVIM_TRANSCRIPT_MARKER' in self.buffer_output,
                      'OMP did not execute the synthetic transcript command')
        self.send_keys(terminal, b'\x1b\x1b/OMP_NVIM_TRANSCRIPT_MARKER\r')
        self.wait_for(lambda: self.latest_state(omp_buffer) and
                      self.latest_state(omp_buffer)['mode'] == 'normal' and
                      self.latest_state(omp_buffer).get('matches'),
                      'rmux Vim could not search the OMP transcript')
        self.assertTrue(self.latest_state(nvim_buffer)['app_control'],
                        'navigating OMP changed Neovim application control')

        self.send_frame(control, PREVIOUS_BUFFER)
        self.wait_for(lambda: self.layout is not None and
                      nvim_buffer in self._window_buffers(self.layout['tree']),
                      'could not return to the open Neovim buffer')
        self.send_keys(terminal, b':q!\r')
        self.wait_for(lambda: self.latest_state(nvim_buffer) is not None and
                      not self.latest_state(nvim_buffer)['app_control'],
                      'Neovim :q! did not release application control')


    def test_csi_3j_discards_prior_scrollback(self):
        command = ("printf 'OLD_SCROLLBACK_MARKER'; "
                   "i=0; while [ $i -lt 30 ]; do printf '\\n'; i=$((i + 1)); done; "
                   "printf '\\033[3J'; printf 'NEW_SCROLLBACK_MARKER'; sleep 30")
        _, _, _ = self.start_shell_session(command=command)
        time.sleep(0.2)
        self.wait_for(lambda: self.layout is not None and self.latest_state() is not None,
                      'session did not report terminal state')
        buffer = self.latest_state()['buffer']
        history_socket = self.attach_control('vim', observe=False)
        self.send_frame(history_socket, HISTORY_VIEW, buffer.to_bytes(8, 'big') + (100000).to_bytes(4, 'big'))
        live = bytearray()
        history = bytearray()
        got_history = False
        while not got_history or b'NEW_SCROLLBACK_MARKER' not in live:
            kind, payload = self.read_frame(history_socket)
            if kind == BUFFER_SNAPSHOT:
                live.extend(payload[12:])
            elif kind == HISTORY_SNAPSHOT:
                history.extend(payload[16:])
                got_history = True
            elif kind == CLOSE:
                self.fail('session closed before history response')
        self.assertNotIn(b'OLD_SCROLLBACK_MARKER', live + history)

    def test_history_is_bounded_to_2000_rows(self):
        ready = self.root / 'history-ready'
        script = ('import time; [print(f"ROW{i:04}") for i in range(2100)]; '
                  f'open({str(ready)!r}, "w").close(); time.sleep(30)')
        command = 'python3 -u -c ' + shlex.quote(script)
        _, terminal, _ = self.start_shell_session(command=command)
        self.wait_for(lambda: self.latest_state() and
                      self.latest_state().get('history_len') == 2000,
                      'scrollback did not reach its configured bound')
        self.wait_for(ready.exists, 'history producer did not finish')
        buffer = self.latest_state()['buffer']
        self.send_keys(terminal, b'\x1bd')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'normal')
        self.send_keys(terminal, b'9999K')
        self.wait_for(lambda: self.latest_state(buffer).get('scrollback') == 2000,
                      'could not navigate to the oldest retained row')
        history = self.attach_control('vim', observe=False)
        while True:
            kind, payload = self.read_frame(history)
            if kind == HISTORY_SNAPSHOT:
                self.assertNotIn(b'ROW0000', payload[16:])
                self.assertIn(b'ROW0080', payload[16:])
                break
            if kind == CLOSE:
                self.fail('session closed before history snapshot')


    def test_omp_history_search_copy_survives_rerender(self):
        if not shutil.which('omp'):
            self.skipTest('OMP is not installed')

        extension = Path(__file__).resolve().parents[3] / 'dotFiles/omp/agent/extensions/rerender.ts'
        if not extension.is_file():
            self.skipTest('OMP rerender extension is not installed')

        clipboard = self.root / 'clipboard'
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        xclip = bin_dir / 'xclip'
        xclip.write_text('#!/usr/bin/python3\nimport sys\nopen(' +
                         repr(str(clipboard)) + ', "wb").write(sys.stdin.buffer.read())\n')
        xclip.chmod(0o755)

        omp_dir = self.root / 'omp'
        omp_dir.mkdir()
        env = dict(self.env, PI_CODING_AGENT_DIR=str(omp_dir),
                   OMP_SKIP_SETUP='1',
                   OPENAI_API_KEY='rmux-test-not-a-real-key',
                   PATH=f'{bin_dir}:{self.env.get("PATH", "")}')
        command = ['omp', '--no-session', '--no-extensions', '-e', str(extension),
                   '--no-context-files', '--no-skills', '--no-rules',
                   '--model', 'openai/gpt-5.2', '--offline', '--no-title']
        _, terminal, _ = self.start_shell_session(
            name='omp-vim', command='exec ' + shlex.join(command), env=env)
        self.wait_for(lambda: self.latest_state() is not None and
                      self.latest_state().get('mode') == 'insert',
                      'OMP did not enter its interactive prompt')
        buffer = self.latest_state()['buffer']

        marker = 'OMP_VIM_TRANSCRIPT_MARKER'
        self.wait_for(lambda: b'state=idle:app=omp' in self.buffer_output,
                      'OMP did not render its interactive UI')
        shell_command = ("!printf '%s\\n' OMP_VIM_TRANSCRIPT_\"\"MARKER "
                         "OMP_VIM_SECOND_LINE OMP_VIM_THIRD_LINE OMP_VIM_SHELL_\"\"COMPLETE")
        self.assertNotIn(marker, shell_command)
        self.send_keys(terminal, shell_command.encode() + b'\r')
        self.wait_for(lambda: marker.encode() in self.buffer_output and
                      b'OMP_VIM_SHELL_COMPLETE' in self.buffer_output,
                      'OMP did not execute the synthetic shell command')
        self.send_keys(terminal, b'\x1b\x1b/' + marker.encode() + b'\r')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'normal' and
                      self.latest_state(buffer).get('matches'),
                      'Vim search did not find the synthetic OMP transcript')
        self.send_keys(terminal, b'v' + b'l' * (len(marker) - 1) + b'y')
        self.wait_for(clipboard.exists, 'Vim yank did not invoke xclip')
        self.assertEqual(clipboard.read_bytes(), marker.encode())
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'insert',
                      'Vim mode did not exit after copying')

        clears_before = self.buffer_output.count(b'\x1b[3J')
        self.send_keys(terminal, b'/rerender\r')
        self.wait_for(lambda: self.buffer_output.count(b'\x1b[3J') > clears_before,
                      'OMP /rerender did not clear and replay terminal history')
        self.send_keys(terminal, b'\x1b\x1b/' + marker.encode() + b'\r')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'normal' and
                      self.latest_state(buffer).get('matches'),
                      'replayed OMP transcript marker was not searchable')

    def test_escape_clears_search_highlights(self):
        _, terminal, _ = self.start_shell_session(
            command="printf 'needle x needle\\033[H'; sleep 30")
        self.wait_for(lambda: self.latest_state() is not None)
        buffer = self.latest_state()['buffer']
        self.send_keys(terminal, b'\x06needle\r')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'normal' and
                      len(self.latest_state(buffer)['matches']) >= 2)
        self.send_keys(terminal, b'\x1b')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'insert' and
                      not self.latest_state(buffer)['matches'],
                      'Escape did not clear accepted search highlighting')

        self.send_keys(terminal, b'\x06needle')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'search' and
                      self.latest_state(buffer)['matches'])
        self.send_keys(terminal, b'\x1b')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'insert' and
                      not self.latest_state(buffer)['matches'],
                      'Escape did not clear search prompt highlighting')

    def test_yank_flash_keeps_history_view_then_expires(self):
        bin_dir = self.root / 'bin'
        bin_dir.mkdir()
        xclip = bin_dir / 'xclip'
        xclip.write_text('#!/bin/sh\ncat >/dev/null\n')
        xclip.chmod(0o755)
        env = dict(self.env, PATH=f'{bin_dir}:{self.env.get("PATH", "")}')
        _, terminal, _ = self.start_shell_session(command='seq 1 100; sleep 30', env=env)
        self.wait_for(lambda: self.latest_state() and
                      self.latest_state()['history_len'] >= 20)
        buffer = self.latest_state()['buffer']
        self.send_keys(terminal, b'\x1bd')
        self.wait_for(lambda: self.latest_state(buffer)['scrollback'] == 5)
        self.send_keys(terminal, b'vl')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'visual' and
                      self.latest_state(buffer)['selection'] is not None)
        selected = self.latest_state(buffer)['selection']
        self.send_keys(terminal, b'y')
        self.wait_for(lambda: self.latest_state(buffer)['mode'] == 'insert' and
                      self.latest_state(buffer).get('yank_selection') is not None,
                      'missing yank flash')
        state = self.latest_state(buffer)
        self.assertEqual(state['yank_selection'], selected)
        self.assertEqual(state['yank_mode'], 'visual')
        self.assertEqual(state['scrollback'], 5)
        self.wait_for(lambda: self.latest_state(buffer).get('yank_selection') is None and
                      self.latest_state(buffer)['scrollback'] == 0,
                      'yank flash did not expire and return to live output')

    def test_configurable_n_and_N_search_bindings(self):
        config = self.root / 'config' / 'rmux' / 'config.toml'
        config.parent.mkdir()
        config.write_text('[bindings]\nvim_next_match = "n"\n'
                          'vim_previous_match = "N"\n')
        command = "printf 'needle x needle\\033[H'; sleep 30"
        _, terminal, _ = self.start_shell_session(command=command)
        self.wait_for(lambda: self.latest_state() is not None, 'missing initial Vim state')
        buffer = self.latest_state()['buffer']
        self.send_keys(terminal, b'\x1b\x1b/needle\n')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['mode'] == 'normal' and
                      len(self.latest_state(buffer)['matches']) >= 2,
                      'search did not find both occurrences')
        first = self.latest_state(buffer)['cursor_col']
        self.send_keys(terminal, b'n')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['cursor_col'] != first,
                      'configured next-match binding did not advance')
        second = self.latest_state(buffer)['cursor_col']
        self.send_keys(terminal, b'N')
        self.wait_for(lambda: self.latest_state(buffer) and
                      self.latest_state(buffer)['cursor_col'] == first,
                      'configured previous-match binding did not return')
        self.assertNotEqual(first, second)


if __name__ == '__main__':
    unittest.main()
