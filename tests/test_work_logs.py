"""Work dates through real MCP, including late correction and backup roundtrip."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'src-tauri/target/debug'
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0


class WorkLogs(unittest.TestCase):
    def test_cross_session_periods_and_restore(self):
        env = {k: v for k, v in os.environ.items() if not k.upper().startswith('GM_')}
        with tempfile.TemporaryDirectory(prefix='gm-worklog-') as folder:
            data = Path(folder) / 'source'
            with (Path(folder) / 'service.log').open('w') as log:
                service = subprocess.Popen([str(BIN/'graph-memory-service.exe'), '--data-dir', str(data)],
                                           env=env, stdout=log, stderr=log, creationflags=FLAGS)
                try:
                    deadline = time.monotonic() + 15
                    while True:
                        try:
                            info = json.loads((data/'service.json').read_text())
                            with urllib.request.urlopen(info['api_url']+'/api/v1/projects', timeout=2):
                                break
                        except (OSError, ValueError):
                            if time.monotonic() > deadline:
                                raise RuntimeError('service not ready')
                            time.sleep(.05)

                    def tool(name, args, bad=False):
                        request = dict(jsonrpc='2.0', id=1, method='tools/call', params=dict(name=name, arguments=args))
                        p = subprocess.run([str(BIN/'mcp_server.exe')], input=json.dumps(request)+'\n',
                                           text=True, encoding='utf8', capture_output=True, timeout=10,
                                           env={**env, 'GM_API_URL': info['api_url'], 'GM_MEMORY_V1': '1'}, creationflags=FLAGS)
                        self.assertEqual(p.returncode, 0, p.stderr)
                        result = json.loads(p.stdout)['result']
                        self.assertEqual(bool(result.get('isError')), bad, result)
                        return None if bad else json.loads(result['content'][0]['text'])

                    req = urllib.request.Request(info['api_url']+'/api/v1/projects',
                        data=json.dumps({'name':'工作日志测试'}).encode(), headers={'Content-Type':'application/json'})
                    with urllib.request.urlopen(req) as response:
                        project = json.load(response)['id']
                    metadata = dict(date='2024-02-29', utc_offset='+08:00', status='completed',
                                    outcome='隔离测试已通过', next_steps=['检查真实宿主采用'])
                    seed = dict(memory_scope='project', project_id=project, event_id='log-1',
                                kind='work_log', content='修复接口并执行测试，输出保存在本地。', work_log=metadata)
                    record = tool('memory_write', seed)['memory']
                    self.assertEqual(tool('memory_write', seed)['memory']['id'], record['id'])
                    query = dict(scope='project', project_id=project, kind='work_log', date_field='work_date',
                                 **{'from':'2024-02-29', 'to':'2024-03-01'})
                    self.assertEqual(tool('memory_search', query)['total'], 1)
                    tool('memory_update', dict(id=record['id'], expected_version=1, content='后来补充：测试细节完整保留。'))
                    self.assertEqual(tool('memory_search', query)['memories'][0]['version'], 2)
                    self.assertEqual(tool('memory_search', {**query, 'from':'2024-03-01','to':'2024-03-02'})['total'], 0)
                    tool('memory_write', dict(memory_scope='personal', event_id='old', content='旧记录日期未知'))
                    self.assertEqual(tool('memory_search', dict(scope='all',date_field='work_date'))['total'], 1)
                    for date in ('2023-02-29','2024-2-29','2024-13-01'):
                        tool('memory_write', {**seed,'event_id':date,'work_log':{**metadata,'date':date}}, bad=True)
                    tool('memory_write', {**seed,'event_id':'bad-status','work_log':{**metadata,'status':'done'}}, bad=True)
                    tool('memory_search', {**query,'from':'2024-02-29T00:00:00+08:00'}, bad=True)
                    for offset in ('+24:00','+08:60','Asia/Shanghai'):
                        tool('memory_write', {**seed,'event_id':offset,'work_log':{**metadata,'utc_offset':offset}}, bad=True)
                    tool('memory_update', dict(id=record['id'], expected_version=2, content='invalid', work_log=None), bad=True)
                    self.assertEqual(tool('memory_search', query)['memories'][0]['version'], 2)
                    other = tool('memory_write', {**seed,'event_id':'next-day', 'work_log':{**metadata,'date':'2024-03-01','status':'blocked','outcome':'等待复核'}})['memory']
                    period = {**query,'from':'2024-02-29','to':'2024-03-02','limit':1}
                    page = tool('memory_search', period)
                    self.assertEqual((page['total'],page['next_offset']), (2,1))
                    self.assertEqual(page['memories'][0]['id'],other['id'])
                    self.assertEqual(tool('memory_search',{**period,'offset':1})['memories'][0]['id'],record['id'])
                    self.assertEqual(tool('memory_search', dict(scope='personal',date_field='work_date'))['total'],0)
                    backup = Path(folder)/'work.gmbackup'
                    restore = Path(folder)/'restore'
                    restore.mkdir()
                    def cli(*args):
                        p = subprocess.run([str(BIN/'graph-memory-backup.exe'), *map(str,args)], capture_output=True,
                                           text=True, timeout=15, env=env, creationflags=FLAGS)
                        self.assertEqual(p.returncode,0,p.stderr)
                    cli('export','--data-dir',data,'--output',backup)
                    cli('verify',backup)
                    cli('restore','--input',backup,'--data-dir',restore)
                    again=Path(folder)/'again.gmbackup'
                    cli('export','--data-dir',restore,'--output',again)
                    original=json.loads(backup.read_text(encoding='utf8'))
                    self.assertEqual(original['payload'],json.loads(again.read_text(encoding='utf8'))['payload'])
                    current=next(json.loads(m['body']) for m in original['payload']['memories'] if m['id']==record['id'])
                    self.assertEqual(current['work_log'],metadata)
                finally:
                    service.terminate()
                    try: service.wait(timeout=5)
                    except subprocess.TimeoutExpired: service.kill(); service.wait(timeout=5)


if __name__ == '__main__':
    unittest.main()
