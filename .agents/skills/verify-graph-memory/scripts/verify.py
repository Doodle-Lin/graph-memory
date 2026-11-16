"""Owned, isolated black-box MCP and backup verification; Python 3.12 stdlib."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[4]
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0


def require(value, message):
    if not value:
        raise RuntimeError(message)


def api(url, path):
    with urllib.request.urlopen(url + path, timeout=3) as response:
        return json.load(response)


def doctor(url, pid, data):
    require(url.startswith('http://127.0.0.1:'), 'only local isolated service supported')
    info = api(url, '/api/service')
    require(info['pid'] == pid, 'service PID mismatch')
    require(os.path.samefile(info['data_dir'], data), 'workspace mismatch')
    require(info['api_url'] == url and info['protocol_version'] == 1, 'protocol/address mismatch')
    require(api(url, '/api/health')['status'] == 'ok', 'health not ready')
    api(url, '/api/v1/projects')  # discovery alone precedes MemoryStore readiness
    return info


def run(bin_dir):
    evidence = ROOT / 'artifacts/verification' / ('run-' + uuid.uuid4().hex)
    evidence.mkdir(parents=True)
    scratch = Path(tempfile.mkdtemp(prefix='gm-verifier-')).resolve()
    data, restored = scratch / 'source', scratch / 'restored'
    restored.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith('GM_')}
    env['GM_DRAFT_DIR'] = str(scratch / 'drafts')
    events, processes, handles = [], [], []
    result = {'status': 'failed', 'evidence': str(evidence), 'scratch': str(scratch)}

    def save(name, obj):
        (evidence / name).write_text(json.dumps(obj, ensure_ascii=False, indent=2), encoding='utf8')

    def command(name, args, input_text=None, extra_env=None):
        p = subprocess.run([str(bin_dir / (name + '.exe')), *map(str, args)],
                           input=input_text, capture_output=True, text=True, encoding='utf8',
                           timeout=30, cwd=scratch, env={**env, **(extra_env or {})}, creationflags=FLAGS)
        events.append({'command': name, 'args': list(map(str, args)), 'input': input_text,
                       'exit': p.returncode, 'stdout': p.stdout, 'stderr': p.stderr})
        require(p.returncode == 0, name + ' failed')
        return p.stdout

    def start(folder):
        log = (evidence / (folder.name + '-service.log')).open('w', encoding='utf8')
        handles.append(log)
        p = subprocess.Popen([str(bin_dir / 'graph-memory-service.exe'), '--data-dir', str(folder)],
                             cwd=scratch, env=env, stdout=log, stderr=log, creationflags=FLAGS)
        processes.append(p)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            require(p.poll() is None, 'owned service exited during startup')
            try:
                info = json.loads((folder / 'service.json').read_text(encoding='utf8'))
                checked = doctor(info['api_url'], p.pid, folder)
                save(folder.name + '-doctor.json', checked)
                return checked
            except (OSError, ValueError):
                time.sleep(.05)
        raise RuntimeError('service readiness timed out')

    def mcp(info, name, arguments):
        requests = [dict(jsonrpc='2.0', id=1, method='initialize', params={}),
                    dict(jsonrpc='2.0', id=2, method='tools/call', params=dict(name=name, arguments=arguments))]
        output = command('mcp_server', [], ''.join(json.dumps(r) + '\n' for r in requests),
                         {'GM_API_URL': info['api_url'], 'GM_MEMORY_V1': '1'})
        responses = [json.loads(line) for line in output.splitlines()]
        response = next(r for r in responses if r.get('id') == 2)
        require('error' not in response and not response['result'].get('isError'), 'MCP call failed')
        return json.loads(response['result']['content'][0]['text'])

    try:
        identities = {}
        for name in ('graph-memory-service', 'mcp_server', 'graph-memory-backup'):
            path = bin_dir / (name + '.exe')
            with path.open('rb') as binary:
                identities[name] = {'path': str(path), 'sha256': hashlib.file_digest(binary, 'sha256').hexdigest()}
        save('build.json', identities)
        info = start(data)
        seed = {'memory_scope': 'personal', 'event_id': 'verifier-' + uuid.uuid4().hex,
                'title': '验证经验', 'content': 'VERIFY经验：先核对实例，再执行操作。', 'source': 'isolated verifier'}
        written = mcp(info, 'memory_write', seed)['memory']
        require(mcp(info, 'memory_write', seed)['memory']['id'] == written['id'], 'retry duplicated record')
        found = mcp(info, 'memory_search', {'scope': 'personal', 'query': 'VERIFY'})
        require(found['total'] == 1 and found['memories'][0]['id'] == written['id'], 'new session recall failed')
        updated = mcp(info, 'memory_update', {'id': written['id'], 'expected_version': 1,
                      'content': 'VERIFY经验：通过新会话再次核对实例。'})['memory']
        require(updated['version'] == 2, 'version did not advance')
        context = mcp(info, 'memory_context', {'query': 'VERIFY'})
        require(updated['content'] in context['text'], 'context missing corrected content')
        backup = evidence / 'workspace.gmbackup'
        command('graph-memory-backup', ['export', '--data-dir', data, '--output', backup])
        command('graph-memory-backup', ['verify', backup])
        command('graph-memory-backup', ['restore', '--input', backup, '--data-dir', restored, '--dry-run'])
        require(not list(restored.iterdir()), 'dry run wrote target')
        command('graph-memory-backup', ['restore', '--input', backup, '--data-dir', restored])
        copy = start(restored)
        recovered = mcp(copy, 'memory_search', {'scope': 'personal', 'query': 'VERIFY'})
        recovered_record = dict(recovered['memories'][0])
        recovered_record.pop('score', None)  # search-only ranking, not stored record data
        require(recovered_record == updated, 'restored record differs')
        require(mcp(copy, 'memory_write', seed)['memory']['version'] == 1, 'creation replay response lost')
        require(mcp(copy, 'memory_search', {'scope': 'personal', 'query': 'VERIFY'})['total'] == 1, 'restore replay duplicated record')
        doctor(info['api_url'], info['pid'], data)
        result['status'] = 'passed'
    except Exception as error:
        result['error'] = str(error)
    finally:
        cleanup_errors = []
        for p in reversed(processes):
            try:
                if p.poll() is None:
                    p.terminate()
                    try:
                        p.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        p.kill()
                        p.wait(timeout=5)
            except Exception as error:
                cleanup_errors.append(str(error))
        for handle in handles:
            handle.close()
        try:
            require(scratch.parent == Path(tempfile.gettempdir()).resolve() and scratch.name.startswith('gm-verifier-'), 'cleanup path outside owned scratch')
            shutil.rmtree(scratch)
        except Exception as error:
            cleanup_errors.append(str(error))
        result['cleanup_errors'] = cleanup_errors
        result['owned_processes_stopped'] = all(p.poll() is not None for p in processes)
        result['scratch_removed'] = not scratch.exists()
        if cleanup_errors:
            result['status'] = 'failed'
        save('actions.json', events)
        save('result.json', result)
    require((evidence / 'actions.json').is_file() and (evidence / 'result.json').is_file(), 'evidence lost after cleanup')
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='mode', required=True)
    r = sub.add_parser('run')
    r.add_argument('--bin-dir', type=Path, default=ROOT / 'src-tauri/target/debug')
    d = sub.add_parser('doctor')
    d.add_argument('--url', required=True)
    d.add_argument('--pid', required=True, type=int)
    d.add_argument('--data-dir', required=True, type=Path)
    args = parser.parse_args()
    if args.mode == 'doctor':
        print(json.dumps(doctor(args.url, args.pid, args.data_dir), ensure_ascii=False))
    else:
        raise SystemExit(run(args.bin_dir.resolve()))
