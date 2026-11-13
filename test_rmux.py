import fcntl
import os
import pty
import select
import signal
import socket
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

RMUX = Path(__file__).with_name('rmux')


class Sessions(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='rmux-')
        self.env = dict(os.environ, XDG_RUNTIME_DIR=self.tmp.name, TERM='xterm')
        self.clients = []

    def tearDown(self):
        for name in self.run_rmux('ls').stdout.splitlines():
            subprocess.run(['dtach', '-p', f'{self.tmp.name}/rmux/{name}'],
                           input=b'exit\n', stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for process, fd in self.clients:
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

    def start(self, *args):
        master, slave = pty.openpty()
        process = subprocess.Popen([str(RMUX), *args], env=self.env, stdin=slave,
                                   stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.clients.append((process, master))
        return process, master

    def wait_for(self, predicate):
        for _ in range(100):
            if predicate():
                return
            time.sleep(0.03)
        self.fail('timed out waiting for session state')

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

    def test_stale_hidden_and_invalid_names(self):
        self.run_rmux('ls')
        stale = f'{self.tmp.name}/rmux/.hidden'
        sock = socket.socket(socket.AF_UNIX)
        sock.bind(stale)
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
        self.assertEqual(self.run_rmux('kill', 'target').returncode, 0)
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
        socket_path = f'{self.tmp.name}/rmux/old'
        subprocess.run(['dtach', '-n', socket_path, 'sh', '-i'], check=True)
        try:
            result = self.run_rmux('kill', 'old')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('not tracked', result.stderr)
            self.assertIn('old', self.run_rmux('ls').stdout.splitlines())
        finally:
            subprocess.run(['dtach', '-p', socket_path], input=b'exit\n', check=True)
        process, fd = self.start('new', 'tracked', '--', 'sh', '-i')
        self.wait_for(lambda: 'tracked' in self.run_rmux('ls').stdout.splitlines())
        pidfile = Path(self.tmp.name, 'rmux-pids', 'tracked')
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
        first_id = Path(self.tmp.name, 'rmux-pids', 'first')
        second_id = Path(self.tmp.name, 'rmux-pids', 'second')
        self.wait_for(lambda: first_id.exists() and second_id.exists())
        first_id.write_text(second_id.read_text())
        result = self.run_rmux('kill', 'first')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('does not own socket', result.stderr)
        self.assertIsNone(first.poll())
        self.assertIsNone(second.poll())
        os.write(fd1, b'exit\n')
        os.write(fd2, b'exit\n')

    def test_kill_and_up_do_not_kill_replacement(self):
        first, fd = self.start('new', 'race', '--', 'sh', '-i')
        self.wait_for(lambda: 'race' in self.run_rmux('ls').stdout.splitlines())
        lock_path = Path(self.tmp.name, 'rmux-locks', 'race')
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
