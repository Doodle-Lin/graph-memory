"""Owned black-box MCP, dated work logs and backup verification; Python 3.12 stdlib."""
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
    result['features'] = {name: 'not_run' for name in ('mcp_continuity', 'work_logs', 'backup_restore')}

    def save(name, obj):
        (evidence / name).write_text(json.dumps(obj, ensure_ascii=False, indent=2), encoding='utf8')

    def command(name, args, input_text=None, extra_env=None):
        try:
            p = subprocess.run([str(bin_dir / (name + '.exe')), *map(str, args)],
                               input=input_text, capture_output=True, text=True, encoding='utf8',
                               timeout=30, cwd=scratch, env={**env, **(extra_env or {})}, creationflags=FLAGS)
        except subprocess.TimeoutExpired as error:
            def decoded(value):
                return value.decode('utf8', errors='replace') if isinstance(value, bytes) else value
            events.append({'command': name, 'args': list(map(str, args)), 'input': input_text,
                           'timeout': True, 'stdout': decoded(error.stdout), 'stderr': decoded(error.stderr)})
            raise
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

    def mcp(info, name, arguments, expect_error=False):
        doctor(info['api_url'], info['pid'], Path(info['data_dir']))
        requests = [dict(jsonrpc='2.0', id=1, method='initialize', params={}),
                    dict(jsonrpc='2.0', id=2, method='tools/call', params=dict(name=name, arguments=arguments))]
        output = command('mcp_server', [], ''.join(json.dumps(r) + '\n' for r in requests),
                         {'GM_API_URL': info['api_url'], 'GM_MEMORY_V1': '1'})
        responses = [json.loads(line) for line in output.splitlines()]
        initialized = next(r for r in responses if r.get('id') == 1)
        require('error' not in initialized and initialized['result']['serverInfo']['name'] == 'graph-memory', 'MCP initialize failed')
        result['mcp_version'] = initialized['result']['serverInfo']['version']
        response = next(r for r in responses if r.get('id') == 2)
        require('error' not in response, 'MCP protocol error')
        require(bool(response['result'].get('isError')) == expect_error, 'unexpected MCP success/error')
        if expect_error:
            return response['result']
        return json.loads(response['result']['content'][0]['text'])

    def project(info, name):
        body = {'name': name}
        request = urllib.request.Request(info['api_url'] + '/api/v1/projects',
            data=json.dumps(body).encode('utf8'), headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=3) as response:
            created = json.load(response)
        events.append({'method': 'POST', 'path': '/api/v1/projects', 'body': body, 'result': created})
        return created['id']

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
        result['features']['mcp_continuity'] = 'passed'

        # Work dates are civil days, independent of today's write/update timestamps.
        alpha, beta = project(info, 'Verifier Alpha'), project(info, 'Verifier Beta')
        metadata = dict(date='2024-02-29', utc_offset='+08:00', status='in_progress',
                        outcome='修补完成，回归待执行', next_steps=['运行隔离回归'])
        log_seed = dict(memory_scope='project', project_id=alpha, event_id='work-log-'+uuid.uuid4().hex,
                        title='导出修复待回归', content='WORKVERIFY：修补export.py空值处理。', kind='work_log',
                        source='synthetic verifier', evidence_type='user_or_agent_statement', work_log=metadata)
        day1 = mcp(info, 'memory_write', log_seed)['memory']
        require(mcp(info, 'memory_write', log_seed)['memory']['id'] == day1['id'], 'work log retry duplicated')
        completed = {**metadata, 'status': 'completed', 'outcome': '12条合成回归通过', 'next_steps': []}
        day1 = mcp(info, 'memory_update', dict(id=day1['id'], expected_version=1,
            title='导出修复回归完成', content='WORKVERIFY：修补export.py空值处理，12条合成回归通过。', work_log=completed))['memory']
        require(day1['version'] == 2 and day1['work_log'] == completed, 'same-day correction lost metadata/version')
        day2 = mcp(info, 'memory_write', {**log_seed, 'event_id': log_seed['event_id']+'-next',
            'title': '计划评估导出压缩', 'content': 'WORKVERIFY：仅提出评估压缩方案的计划，未改代码、未执行测试。',
            'work_log': {**metadata, 'date': '2024-03-01', 'status': 'planned', 'outcome': '仅计划评估压缩，尚未执行'}})['memory']
        mcp(info, 'memory_write', {**log_seed, 'project_id': beta, 'event_id': log_seed['event_id']+'-beta'})
        period = dict(scope='project', project_id=alpha, kind='work_log', date_field='work_date',
                      **{'from': '2024-02-29', 'to': '2024-03-02', 'limit': 1})
        page1 = mcp(info, 'memory_search', period)
        page2 = mcp(info, 'memory_search', {**period, 'offset': page1['next_offset']})
        require(page1['total'] == 2 and page1['next_offset'] == 1 and page2['next_offset'] is None, 'pagination contract failed')
        require([p['memories'][0]['id'] for p in (page1, page2)] == [day2['id'], day1['id']], 'work date ordering or project boundary failed')
        require(page2['memories'][0]['work_log']['date'] == '2024-02-29', 'late correction moved occurrence date')
        exclusive = mcp(info, 'memory_search', {**period, 'to': '2024-03-01'})
        require(exclusive['total'] == 1 and exclusive['memories'][0]['id'] == day1['id'], 'exclusive upper boundary failed')
        require(mcp(info, 'memory_search', {'scope': 'personal', 'date_field': 'work_date'})['total'] == 0, 'unknown-date personal note leaked into period')
        require(mcp(info, 'memory_search', {'scope': 'all', 'date_field': 'work_date'})['total'] == 3, 'all scope includes unknown dates or misses dated logs')
        mcp(info, 'memory_write', {**log_seed, 'event_id': log_seed['event_id']+'-bad',
            'work_log': {**metadata, 'date': '2023-02-29'}}, expect_error=True)
        mcp(info, 'memory_update', dict(id=day1['id'], expected_version=2, work_log=None), expect_error=True)
        after_rejection = mcp(info, 'memory_search', {**period, 'to': '2024-03-01'})
        require(after_rejection['total'] == 1 and after_rejection['memories'][0]['version'] == 2, 'rejected mutation changed data')
        unchanged = dict(after_rejection['memories'][0])
        unchanged.pop('score', None)
        require(unchanged == day1, 'rejected update changed persisted fields')
        require(mcp(info, 'memory_search', {'scope': 'all'})['total'] == 4, 'invalid creation leaked a record')
        save('work-logs.json', {'day1': day1, 'day2': day2, 'pages': [page1, page2], 'after_rejection': after_rejection})
        result['features']['work_logs'] = 'passed'
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
        restored_logs = mcp(copy, 'memory_search', {**period, 'limit': 100})
        expected_logs = {m['id']: m for m in (day1, day2)}
        require(restored_logs['total'] == 2, 'restore lost project logs')
        for record in restored_logs['memories']:
            record = dict(record)
            record.pop('score', None)
            require(record == expected_logs[record['id']], 'restored work-log fields differ')
        again = evidence / 'restored.gmbackup'
        command('graph-memory-backup', ['export', '--data-dir', restored, '--output', again])
        require(json.loads(backup.read_text(encoding='utf8'))['payload'] == json.loads(again.read_text(encoding='utf8'))['payload'], 'restored full payload differs')
        save('source-final-doctor.json', doctor(info['api_url'], info['pid'], data))
        save('restored-final-doctor.json', doctor(copy['api_url'], copy['pid'], restored))
        result['features']['backup_restore'] = 'passed'
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
