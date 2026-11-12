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


if __name__ == '__main__':
    unittest.main()
