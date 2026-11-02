"""Exercise actual backup CLI against an isolated, concurrently written service."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time
import unittest
import urllib.request

BIN = Path(__file__).resolve().parents[1] / 'src-tauri/target/debug'
SUFFIX = '.exe' if os.name == 'nt' else ''
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0


class BackupCliTest(unittest.TestCase):
    def test_live_export_and_failures(self):
        env = {k: v for k, v in os.environ.items() if not k.startswith('GM_')}
        with tempfile.TemporaryDirectory(prefix='gm-backup-') as folder:
            root = Path(folder)
            data = root / '中文 工作库'

            def cli(*args, code=0):
                result = subprocess.run(
                    [str(BIN / ('graph-memory-backup' + SUFFIX)), *map(str, args)],
                    cwd=root, env=env, capture_output=True, text=True, encoding='utf8',
                    timeout=15, creationflags=FLAGS)
                self.assertEqual(result.returncode, code, result.stderr)
                return json.loads(result.stdout) if code == 0 else result

            cli(code=2)
            cli('export', '--data-dir', data, code=2)
            cli('export', '--data-dir', data, '--output', root / 'x', '--force', code=2)
            cli('export', '--data-dir', data, '--output', root / 'x', code=1)
            self.assertFalse(data.exists())
            self.assertFalse((root / 'x').exists())
            service = subprocess.Popen(
                [str(BIN / ('graph-memory-service' + SUFFIX)), '--data-dir', str(data)],
                cwd=root, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
                creationflags=FLAGS)
            stop = threading.Event()
            wrote = threading.Event()
            errors = []
            writer = None
            try:
                deadline = time.monotonic() + 20
                while True:
                    self.assertIsNone(service.poll(), 'isolated service exited')
                    try:
                        info = json.loads((data / 'service.json').read_text(encoding='utf8'))

                        def api(path, body=None):
                            request = urllib.request.Request(
                                info['api_url'] + path,
                                data=json.dumps(body).encode() if body is not None else None,
                                headers={'Content-Type': 'application/json'})
                            with urllib.request.urlopen(request, timeout=3) as response:
                                return json.load(response)

                        api('/api/v1/projects')
                        break
                    except (OSError, ValueError):
                        if time.monotonic() > deadline:
                            self.fail('isolated service readiness timed out')
                        time.sleep(.05)
                project = api('/api/v1/projects', {'name': '测试项目'})['id']
                original = api('/api/v1/memories', {
                    'memory_scope': 'personal', 'event_id': 'seed',
                    'content': '经验正文：备份应包含 WAL 中的提交。'})['memory']
                api('/api/v1/memories/update', {
                    'id': original['id'], 'expected_version': 1, 'content': '修订后的经验'})
                self.assertTrue((data / 'work-memory.db-wal').exists())

                def write_loop():
                    try:
                        for i in range(1000):
                            if stop.is_set():
                                break
                            api('/api/v1/memories', {
                                'memory_scope': 'project', 'project_id': project,
                                'event_id': f'live-{i}', 'content': f'并发提交 {i}'})
                            wrote.set()
                            stop.wait(.01)
                    except Exception as error:
                        errors.append(error)

                writer = threading.Thread(target=write_loop)
                writer.start()
                self.assertTrue(wrote.wait(5), errors)
                for i in range(4):
                    output = root / f'备份 {i}.gmbackup'
                    exported = cli('export', '--data-dir', data, '--output', output)
                    self.assertEqual(cli('verify', output)['counts'], exported['counts'])
                    backup = json.loads(output.read_text(encoding='utf8'))
                    payload = backup['payload']
                    self.assertEqual(set(payload), {'projects', 'memories', 'revisions', 'events'})
                    self.assertEqual(backup['manifest']['counts'], {
                        key: len(value) for key, value in payload.items()})
                    memories = {row['id']: json.loads(row['body']) for row in payload['memories']}
                    self.assertGreaterEqual(len(memories), 2)
                    self.assertEqual(memories[original['id']]['version'], 2)
                    self.assertEqual(len(payload['events']), len(memories))
                    self.assertEqual(len(payload['revisions']), len(memories) + 1)
                    for mid, memory in memories.items():
                        revisions = [row for row in payload['revisions'] if row['memory_id'] == mid]
                        self.assertEqual([row['version'] for row in revisions], list(range(1, memory['version'] + 1)))
                        self.assertEqual(json.loads(revisions[-1]['body']), memory)
                    saved = output.read_bytes()
                    cli('export', '--data-dir', data, '--output', output, code=1)
                    self.assertEqual(output.read_bytes(), saved)
                stop.set()
                writer.join(5)
                self.assertFalse(writer.is_alive())
                self.assertFalse(errors, errors)
                self.assertIsNone(service.poll())
                self.assertEqual(api('/api/service')['instance_id'], info['instance_id'])
                before = api('/api/v1/search', {'scope': 'all', 'limit': 1})['total']
                cli('export', '--data-dir', data, '--output', root / 'missing' / 'x', code=1)
                cli('export', '--data-dir', data, '--output', data / 'work-memory.db', code=1)
                self.assertFalse((root / 'missing').exists())
                self.assertEqual(api('/api/v1/search', {'scope': 'all', 'limit': 1})['total'], before)
                for mutation in ('truncated', 'version', 'digest'):
                    broken = root / (mutation + '.gmbackup')
                    document = json.loads(saved)
                    if mutation == 'truncated':
                        contents = saved[:len(saved) // 2]
                    else:
                        field, value = ('format_version', 99) if mutation == 'version' else ('payload_sha256', '0' * 64)
                        document['manifest'][field] = value
                        contents = json.dumps(document).encode()
                    broken.write_bytes(contents)
                    cli('verify', broken, code=1)
                    self.assertEqual(broken.read_bytes(), contents)
                self.assertFalse(list(root.glob('.graph-memory-*.tmp')))
            finally:
                stop.set()
                if writer is not None:
                    writer.join(5)
                service.terminate()
                try:
                    service.communicate(timeout=10)
                except subprocess.TimeoutExpired:
                    service.kill()
                    service.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
