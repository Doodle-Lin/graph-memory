"""Opt-in model adoption: multi-turn Claude work, fresh Claude/Codex period recall.

Uses only synthetic isolated data. Requires configured host model access. No user
MCP configuration is changed. Raw prompts/events and independent HTTP evidence are
retained under ignored artifacts/work-log-adoption; model calls are not offline CI.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import subprocess
import threading
import time
import tomllib
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
TOOLS = ['memory_projects', 'memory_context', 'memory_search', 'memory_write', 'memory_update']
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding='utf8')


def stop(process):
    if process.poll() is None:
        if os.name == 'nt':
            subprocess.run(['taskkill', '/PID', str(process.pid), '/T', '/F'],
                           capture_output=True, timeout=15, check=True)
        else:
            process.terminate()
        process.wait(timeout=15)


class ClaudeSession:
    """One actual host session; send the next user message only after turn result."""
    def __init__(self, command, cwd, env, run, name, timeout):
        self.timeout = timeout
        self.events = []
        self.pending = queue.Queue()
        self.stdout = (run / (name + '.jsonl')).open('w', encoding='utf8')
        self.stderr = (run / (name + '.stderr.log')).open('w', encoding='utf8')
        self.prompts = (run / (name + '.prompts.jsonl')).open('w', encoding='utf8')
        self.process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=self.stderr, text=True, encoding='utf8',
            creationflags=FLAGS)
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read(self):
        try:
            for line in self.process.stdout:
                self.stdout.write(line)
                self.stdout.flush()
                self.pending.put(json.loads(line))
        except Exception as error:
            self.pending.put(error)
        finally:
            self.pending.put(None)

    def turn(self, prompt):
        message = {'type': 'user', 'message': {'role': 'user', 'content': prompt}}
        self.prompts.write(json.dumps(message, ensure_ascii=False) + '\n')
        self.prompts.flush()
        self.process.stdin.write(json.dumps(message, ensure_ascii=False) + '\n')
        self.process.stdin.flush()
        turn_events = []
        deadline = time.monotonic() + self.timeout
        while True:
            try:
                event = self.pending.get(timeout=max(.01, deadline - time.monotonic()))
            except queue.Empty:
                raise TimeoutError('Claude turn timed out; see raw evidence')
            if event is None:
                raise RuntimeError('Claude ended before a completed turn')
            if isinstance(event, Exception):
                raise event
            turn_events.append(event)
            self.events.append(event)
            if event.get('type') == 'result':
                assert not event.get('is_error'), event
                return event.get('result', ''), turn_events

    def close(self):
        try:
            if self.process.poll() is None:
                self.process.stdin.close()
                try:
                    self.process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    stop(self.process)
            self.reader.join(timeout=5)
        finally:
            self.stdout.close()
            self.stderr.close()
            self.prompts.close()


def calls(events):
    return [block for event in events if event.get('type') == 'assistant'
            for block in event.get('message', {}).get('content', [])
            if block.get('type') == 'tool_use']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    npm = Path(os.environ.get('APPDATA', '')) / 'npm/node_modules'
    parser.add_argument('--claude-exe', type=Path, default=npm/'@anthropic-ai/claude-code/bin/claude.exe')
    parser.add_argument('--binary-dir', type=Path, default=ROOT/'src-tauri/target/debug')
    parser.add_argument('--model', default='sonnet')
    parser.add_argument('--timeout', type=int, default=240)
    parser.add_argument('--codex-exe', type=Path, help='Additionally run an isolated read-only Codex reader')
    args = parser.parse_args()
    if not __debug__ or args.timeout <= 0:
        parser.error('Assertions must be enabled and timeout positive')
    service = (args.binary_dir/'graph-memory-service.exe').resolve()
    mcp = (args.binary_dir/'mcp_server.exe').resolve()
    cli = args.claude_exe.resolve()
    for path in (service, mcp, cli, *([args.codex_exe] if args.codex_exe else [])):
        if not path.is_file():
            parser.error(f'Missing executable: {path}')
    run = ROOT/'artifacts/work-log-adoption'/uuid.uuid4().hex
    cwd = run/'session'
    cwd.mkdir(parents=True)
    data = run/'data'
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(('GM_', 'ANTHROPIC'))}
    env['CLAUDE_CODE_DISABLE_AUTO_MEMORY'] = '1'
    report = {'passed': False, 'requested_model': args.model, 'codex_requested': bool(args.codex_exe)}
    report['claude_version'] = subprocess.check_output([str(cli), '--version'], env=env, text=True, timeout=20).strip()
    report['binaries'] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in (service, mcp)}
    print(f'Evidence: {run}', flush=True)
    service_log = (run/'service.log').open('w', encoding='utf8')
    owned = subprocess.Popen([str(service), '--data-dir', str(data)], env=env,
        stdout=service_log, stderr=service_log, creationflags=FLAGS)
    sessions = []
    try:
        deadline = time.monotonic() + 20
        while True:
            try:
                info = json.loads((data/'service.json').read_text(encoding='utf8'))
                def api(path, body=None):
                    request = urllib.request.Request(info['api_url']+path,
                        data=None if body is None else json.dumps(body).encode(),
                        headers={'Content-Type': 'application/json'})
                    with urllib.request.urlopen(request, timeout=10) as response:
                        return json.load(response)
                identity = api('/api/service')
                assert identity['pid'] == owned.pid and os.path.samefile(identity['data_dir'], data)
                api('/api/v1/projects')
                break
            except (OSError, ValueError):
                if time.monotonic() > deadline or owned.poll() is not None:
                    raise RuntimeError('Owned service not ready')
                time.sleep(.1)
        save(run/'service-identity.json', identity)
        project = api('/api/v1/projects', {'name': '合成日报 Alpha'})['id']
        other = api('/api/v1/projects', {'name': '无关 Beta'})['id']
        forbidden = 'UNRELATED-'+uuid.uuid4().hex
        api('/api/v1/memories', dict(memory_scope='project', project_id=other,
            event_id='beta', content=forbidden))
        config = run/'mcp.json'
        save(config, {'mcpServers': {'graph_memory_v1': {'command': str(mcp), 'env': {
            'GM_MEMORY_V1': '1', 'GM_API_URL': info['api_url']}}}})
        instructions = (ROOT/'desktop/AGENT_INSTRUCTIONS.md').read_text(encoding='utf8')
        report['instructions_sha256'] = hashlib.sha256(instructions.encode('utf8')).hexdigest()
        command = [str(cli), '-p', '--model', args.model, '--tools', '',
            '--allowedTools', *['mcp__graph_memory_v1__'+tool for tool in TOOLS],
            '--strict-mcp-config', '--mcp-config', str(config), '--settings', '{"disableAllHooks":true}',
            '--setting-sources', 'user', '--disable-slash-commands', '--no-session-persistence',
            '--permission-mode', 'dontAsk', '--append-system-prompt', instructions,
            '--input-format', 'stream-json', '--output-format', 'stream-json', '--verbose']
        writer = ClaudeSession(command, cwd, env, run, 'writer', args.timeout)
        sessions.append(writer)
        marker1, marker2 = ['SYNTH-'+uuid.uuid4().hex for _ in range(2)]
        prompts = [
            '这是合成场景，所有事实仅为用户陈述，不是真实命令结果。项目是“合成日报 Alpha”，'
            '当地工作日期2026-11-16，时区Asia/Shanghai（+08:00）。今天排查导出功能，'
            f'发现 export.py 缺少空值处理，修补已完成但回归尚未跑；排查编号 {marker1}。'
            '请根据进展告诉我接下来该怎么做，简短回答。',
            '继续同一天、同一个导出修复任务：我已跑完12条回归全部通过，'
            f'结果文件 reports/export-check.txt，校验编号 {marker2}。'
            '这个修复任务今天已经完成，生产发布尚未安排。请给出简短交接结论。',
            '现在是2026-11-17（+08:00），还是合成日报 Alpha。我今天才提出一个新计划：'
            '下周评估导出压缩方案，没有改代码或做测试。请帮我列出需要先确认的两点。'
        ]
        snapshots = []
        for number, prompt in enumerate(prompts, 1):
            _, events = writer.turn(prompt)
            assert all(c['name'].startswith('mcp__graph_memory_v1__') for c in calls(events))
            records = api('/api/v1/search', {'scope': 'project', 'project_id': project, 'kind': 'work_log', 'limit': 100})
            save(run/f'after-turn-{number}.json', records)
            snapshots.append(records['memories'])
            print(f'Completed work turn {number}, persisted logs={records["total"]}', flush=True)
        writer.close()
        sessions.remove(writer)
        first, second, third = snapshots
        assert len(first) == 1, 'First milestone was not automatically logged exactly once'
        original = first[0]
        assert original['work_log']['date'] == '2026-11-16'
        assert original['work_log']['status'] in ('in_progress', 'blocked')
        assert marker1 in json.dumps(original, ensure_ascii=False)
        assert len(second) == 1 and second[0]['id'] == original['id'], 'Same task/day duplicated'
        updated = second[0]
        assert updated['version'] > original['version'] and updated['work_log']['status'] == 'completed'
        assert all(m in json.dumps(updated, ensure_ascii=False) for m in (marker1, marker2))
        assert updated['work_log']['date'] == '2026-11-16' and updated['work_log']['utc_offset'] == '+08:00'
        assert updated['evidence_type'] in ('user_confirmed', 'user_or_agent_statement'), 'User claims inflated to tool verification'
        assert len(third) == 2, 'Next-day plan did not append exactly one separate log'
        planned = next(m for m in third if m['id'] != original['id'])
        assert planned['work_log']['date'] == '2026-11-17' and planned['work_log']['status'] == 'planned'
        assert next(m for m in third if m['id'] == original['id']) == updated, 'Next day rewrote previous day'
        ids = {m['id'] for m in third}
        assert len({e['session_id'] for e in writer.events if e.get('type') == 'result'}) == 1, 'Writer was not a single multi-turn session'
        assert forbidden not in json.dumps(writer.events, ensure_ascii=False), 'Other project leaked to writer'
        before = api('/api/v1/search', {'scope': 'all', 'limit': 100})['memories']
        query = ('仅查询“合成日报 Alpha”在2026-11-16至2026-11-22（包含首尾，Asia/Shanghai）的工作情况。'
                 '按日期说明具体行动、结果、完成与计划，保留记录中的完整校验编号，并引用ID、版本与来源。'
                 '说明没有记录的日期覆盖缺口。本次是只读复盘，不新建或修改记录。')
        reader = ClaudeSession(command, cwd, env, run, 'reader', args.timeout)
        sessions.append(reader)
        answer, events = reader.turn(query)
        reader.close()
        sessions.remove(reader)
        reader_calls = calls(events)
        assert not any(c['name'].endswith(('memory_write', 'memory_update')) for c in reader_calls)
        assert any(c['name'].endswith('memory_search') and c['input'].get('date_field') == 'work_date'
                   and c['input'].get('from') == '2026-11-16' and c['input'].get('to') == '2026-11-23'
                   and c['input'].get('scope') == 'project' and c['input'].get('project_id') == project
                   for c in reader_calls), 'Reader did not query actual work dates/project'
        assert all(value in answer for value in (marker1, marker2, *ids)), 'Fresh reader failed to adopt saved details'
        assert forbidden not in json.dumps(events, ensure_ascii=False)
        assert api('/api/v1/search', {'scope': 'all', 'limit': 100})['memories'] == before
        report.update(claude_passed=True, same_day_update=True, next_day_append=True,
            statement_evidence_preserved=True, new_session_recall=True,
            observed_models=sorted({e.get('message', {}).get('model', '') for e in writer.events if e.get('type') == 'assistant'}))
        if args.codex_exe:
            report['codex'] = codex_read(args, run, cwd, env, mcp, info['api_url'], query, (marker1, marker2, *ids), forbidden, api, before)
        report['passed'] = True
    except Exception as error:
        report['error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        try:
            for session in sessions:
                session.close()
        finally:
            stop(owned)
            service_log.close()
            report['owned_service_stopped'] = owned.poll() is not None
            save(run/'report.json', report)
    print('PASS automatic multi-turn work logs and new-session recall', flush=True)


def codex_read(args, run, cwd, env, mcp, url, query, values, forbidden, api, before):
    config_path = Path(os.environ.get('CODEX_HOME', Path.home()/'.codex'))/'config.toml'
    config = tomllib.loads(config_path.read_text(encoding='utf8'))
    command = [str(args.codex_exe.resolve()), 'exec', '--ephemeral', '--skip-git-repo-check',
               '--sandbox', 'read-only', '--json', '-C', str(cwd)]
    overrides = {'approval_policy': 'never', 'features.shell_tool': False, 'features.multi_agent': False,
        'features.apps': False, 'mcp_servers.graph_memory_v1.command': str(mcp),
        'mcp_servers.graph_memory_v1.args': [],
        'mcp_servers.graph_memory_v1.enabled': True, 'mcp_servers.graph_memory_v1.required': True,
        'mcp_servers.graph_memory_v1.env.GM_MEMORY_V1': '1',
        'mcp_servers.graph_memory_v1.env.GM_API_URL': url,
        # Codex merges nested CLI overrides into its existing entry. Override the
        # managed-service settings too: they take precedence over GM_API_URL.
        'mcp_servers.graph_memory_v1.env.GM_SERVICE_DIR': str(run/'data'),
        'mcp_servers.graph_memory_v1.env.GM_SERVICE_EXE': str(mcp.parent/'graph-memory-service.exe'),
        'mcp_servers.graph_memory_v1.enabled_tools': ['memory_projects', 'memory_search'],
        'developer_instructions': (ROOT/'desktop/AGENT_INSTRUCTIONS.md').read_text(encoding='utf8')}
    for name in config.get('mcp_servers', {}):
        if name != 'graph_memory_v1':
            overrides[f'mcp_servers.{name}.enabled'] = False
    for tool in ('memory_projects', 'memory_search'):
        overrides[f'mcp_servers.graph_memory_v1.tools.{tool}.approval_mode'] = 'approve'
    for key, value in overrides.items():
        command += ['-c', key+'='+json.dumps(value, ensure_ascii=False)]
    with (run/'codex.jsonl').open('w', encoding='utf8') as out, (run/'codex.stderr.log').open('w', encoding='utf8') as err:
        process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=out,
            stderr=err, text=True, encoding='utf8', creationflags=FLAGS)
        try:
            process.communicate(query, timeout=args.timeout)
            assert process.returncode == 0, 'Codex failed; see stderr'
        finally:
            stop(process)
    events = [json.loads(line) for line in (run/'codex.jsonl').read_text(encoding='utf8').splitlines() if line.strip()]
    items = [e.get('item', {}) for e in events if e.get('type') == 'item.completed']
    answer = '\n'.join(i.get('text', '') for i in items if i.get('type') == 'agent_message')
    used = [i for i in items if i.get('type') == 'mcp_tool_call']
    assert used and all(i.get('server') == 'graph_memory_v1' and i.get('tool') in ('memory_projects', 'memory_search') for i in used)
    project_ids = {m['project_id'] for m in before if m.get('kind') == 'work_log'}
    assert any(i.get('tool') == 'memory_search' and i.get('arguments', {}).get('date_field') == 'work_date'
               and i['arguments'].get('scope') == 'project' and i['arguments'].get('project_id') in project_ids
               and i['arguments'].get('from') == '2026-11-16' and i['arguments'].get('to') == '2026-11-23'
               for i in used), 'Codex did not query the isolated project work dates'
    assert not any(i.get('type') in ('command_execution', 'file_change') for i in items)
    assert all(value in answer for value in values), 'Codex did not adopt saved details'
    assert forbidden not in json.dumps(events, ensure_ascii=False)
    assert api('/api/v1/search', {'scope': 'all', 'limit': 100})['memories'] == before
    return {'passed': True, 'cross_host_recall': True, 'read_only': True,
            'version': subprocess.check_output([str(args.codex_exe), '--version'], text=True, timeout=20).strip()}


if __name__ == '__main__':
    main()
