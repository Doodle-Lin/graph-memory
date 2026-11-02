"""Integration check for the isolated headless service; requires built binaries."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
import urllib.request
import urllib.error

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'src-tauri' / 'target' / 'debug'
FLAGS = subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0
SUFFIX = '.exe' if os.name == 'nt' else ''

class HeadlessServiceTest(unittest.TestCase):
    def test_service_lifecycle_and_mcp(self):
        env = {k:v for k,v in os.environ.items() if not k.startswith('GM_')}
        with tempfile.TemporaryDirectory(prefix='gm-headless-') as folder:
            cwd = Path(folder)
            data = cwd / 'isolated'
            processes = []
            def start():
                p = subprocess.Popen([str(BIN / ('graph-memory-service'+SUFFIX)), '--data-dir',str(data)], cwd=cwd,env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=FLAGS)
                processes.append(p)
                return p
            def discover(p, old=None):
                deadline = time.monotonic()+15
                while time.monotonic()<deadline:
                    if p.poll() is not None:
                        self.fail(p.communicate()[1].decode(errors='replace'))
                    try:
                        info = json.loads((data/'service.json').read_text())
                        if info['instance_id'] != old:
                            return info
                    except (OSError,ValueError):
                        pass
                    time.sleep(.05)
                self.fail('service readiness timed out')
            def api(info,path,body=None):
                request = urllib.request.Request(info['api_url']+path,data=json.dumps(body).encode() if body is not None else None,headers={'Content-Type':'application/json'})
                with urllib.request.urlopen(request,timeout=3) as response:
                    return json.load(response)
            try:
                first=start(); info=discover(first)
                self.assertFalse((cwd/'frontend').exists())
                self.assertEqual(api(info,'/api/service')['instance_id'],info['instance_id'])
                self.assertEqual(api(info,'/api/health')['status'],'ok')
                # The read-only graph endpoint must not deadlock the engine mutex.
                self.assertEqual(api(info,'/api/graph')['nodes'],[])
                node=api(info,'/api/write',{'content':'ZX8397 diagnostic experience: check process lifetime first.','title':'ZX8397 experience','node_type':'knowledge','source':'test:isolated'})['node']
                found=api(info,'/api/retrieve',{'query':'ZX8397','top_k':5})
                self.assertIn(node['id'],[r['id'] for r in found])
                self.assertEqual(len(api(info,'/api/graph')['nodes']),1)
                for path in ['/', '/index.html', '/api/forget']:
                    with self.assertRaises(urllib.error.HTTPError) as failure:
                        api(info,path)
                    self.assertEqual(failure.exception.code,404)
                duplicate=start(); _,error=duplicate.communicate(timeout=5)
                self.assertNotEqual(duplicate.returncode,0)
                self.assertIn(b'another service owns',error)
                self.assertEqual(api(info,'/api/health')['nodes'],1)
                payload='\n'.join(json.dumps(x) for x in [
                    {'jsonrpc':'2.0','id':1,'method':'initialize','params':{}},
                    {'jsonrpc':'2.0','id':2,'method':'tools/call','params':{'name':'retrieve','arguments':{'query':'ZX8397','top_k':5}}}])+'\n'
                mcp=subprocess.run([str(BIN/('mcp_server'+SUFFIX))],input=payload,text=True,encoding='utf8',capture_output=True,timeout=10,cwd=cwd,env={**env,'GM_API_URL':info['api_url']},creationflags=FLAGS)
                self.assertEqual(mcp.returncode,0,mcp.stderr)
                responses=[json.loads(line) for line in mcp.stdout.splitlines()]
                self.assertIn('serverInfo',responses[0]['result'])
                self.assertNotIn('error',responses[1])
                self.assertIn('ZX8397',json.dumps(responses[1],ensure_ascii=False))
                alpha=api(info,'/api/v1/projects',{'name':'Alpha'})['id']
                beta=api(info,'/api/v1/projects',{'name':'Beta'})['id']
                personal={'memory_scope':'personal','event_id':'experience-1','content':'PowerShell may stop child processes on session exit.','title':'Process lifetime','conditions':'Windows PowerShell','source':'verified case note'}
                experience=api(info,'/api/v1/memories',personal)['memory']
                self.assertIsNone(experience['project_id'])
                self.assertEqual(api(info,'/api/v1/memories',personal)['memory']['id'],experience['id'])
                a=api(info,'/api/v1/memories',{'memory_scope':'project','project_id':alpha,'event_id':'alpha-1','content':'Alpha deployment blocked','kind':'blocker'})['memory']
                b=api(info,'/api/v1/memories',{'memory_scope':'project','project_id':beta,'event_id':'beta-1','content':'Beta deploy succeeded'})['memory']
                records=api(info,'/api/v1/search',{'project_id':alpha})['memories']
                self.assertEqual({r['id'] for r in records},{experience['id'],a['id']})
                no_project=api(info,'/api/v1/search',{})['memories']
                self.assertEqual([r['id'] for r in no_project],[experience['id']])
                context=api(info,'/api/v1/context',{'project_id':beta,'query':'PowerShell'})
                self.assertEqual(context['personal_experiences'][0]['conditions'],'Windows PowerShell')
                self.assertNotIn('Alpha deployment',context['text'])
                self.assertIn('Beta deploy succeeded',context['text'])
                updated=api(info,'/api/v1/memories/update',{'id':experience['id'],'expected_version':1,'content':'Revised method for PowerShell'})['memory']
                self.assertEqual(updated['version'],2)
                with self.assertRaises(urllib.error.HTTPError) as conflict:
                    api(info,'/api/v1/memories/update',{'id':experience['id'],'expected_version':1,'content':'stale overwrite'})
                self.assertEqual(conflict.exception.code,409)
                restored=api(info,'/api/v1/memories/undo',{'id':experience['id'],'expected_version':2})['memory']
                self.assertEqual(restored['content'],personal['content'])
                self.assertEqual(restored['version'],3)
                self.assertEqual(len(api(info,'/api/v1/memories/history',{'id':experience['id']})['history']),3)
                with self.assertRaises(urllib.error.HTTPError) as invalid:
                    api(info,'/api/v1/memories',{'memory_scope':'project','event_id':'invalid','content':'no project'})
                self.assertEqual(invalid.exception.code,400)
                v1payload='\n'.join(json.dumps(x,ensure_ascii=False) for x in [
                    {'jsonrpc':'2.0','id':10,'method':'tools/list'},
                    {'jsonrpc':'2.0','id':11,'method':'tools/call','params':{'name':'memory_write','arguments':{'memory_scope':'personal','event_id':'mcp-personal','content':'日常工作经验：先核实适用条件，不把一次失败写成普遍规律。','conditions':'未经验证的案例','source':'隔离测试'}}},
                    {'jsonrpc':'2.0','id':12,'method':'tools/call','params':{'name':'memory_context','arguments':{'project_id':beta,'query':'PowerShell'}}}])+'\n'
                scoped=subprocess.run([str(BIN/('mcp_server'+SUFFIX))],input=v1payload,text=True,encoding='utf8',capture_output=True,timeout=10,cwd=cwd,env={**env,'GM_API_URL':info['api_url'],'GM_MEMORY_V1':'1'},creationflags=FLAGS)
                self.assertEqual(scoped.returncode,0,scoped.stderr)
                replies=[json.loads(line) for line in scoped.stdout.splitlines()]
                self.assertIn('memory_write',[t['name'] for t in replies[0]['result']['tools']])
                self.assertNotIn('forget',[t['name'] for t in replies[0]['result']['tools']])
                for reply in replies:
                    self.assertNotIn('error',reply)
                    self.assertFalse(reply.get('result',{}).get('isError',False),reply)
                self.assertIn('PowerShell',json.dumps(replies[2]))
                activity=api(info,'/api/v1/agent/status')['activity']
                self.assertEqual({x['tool'] for x in activity},{'memory_write','memory_context'})
                self.assertTrue(all(x['successes']==1 and x['last_ok'] for x in activity))
                self.assertNotIn('PowerShell',json.dumps(activity))
                # Failed writes are visible but never counted as successful writes.
                with self.assertRaises(urllib.error.HTTPError):
                    api(info,'/api/v1/agent/call',{'tool':'memory_write','arguments':{'memory_scope':'project','event_id':'failed','content':'missing project'}})
                write_activity=next(x for x in api(info,'/api/v1/agent/status')['activity'] if x['tool']=='memory_write')
                self.assertEqual((write_activity['calls'],write_activity['successes'],write_activity['last_ok']),(2,1,False))
                self.assertFalse(api(info,'/api/v1/context',{'project_id':beta})['truncated'])
                # Abrupt termination releases the directory lock; stale discovery is replaced.
                first.kill(); first.communicate(timeout=5)
                restarted=start(); fresh=discover(restarted,info['instance_id'])
                self.assertNotEqual(fresh['instance_id'],info['instance_id'])
                self.assertEqual(api(fresh,'/api/health')['nodes'],1)
                self.assertFalse((data/'graph.db').exists())
                self.assertEqual(len(api(fresh,'/api/v1/search',{})['memories']),2)
                request=urllib.request.Request(fresh['api_url']+'/api/v1/projects',data=b'{"name":"web"}',headers={'Origin':'https://example.com','Content-Type':'application/json'})
                with self.assertRaises(urllib.error.HTTPError) as rejected:
                    urllib.request.urlopen(request,timeout=3)
                self.assertEqual(rejected.exception.code,403)
                # MCP starts a fresh service from stale discovery without a desktop.
                restarted.kill(); restarted.communicate(timeout=5)
                managed_env={**env,'GM_MEMORY_V1':'1','GM_SERVICE_DIR':str(data),'GM_SERVICE_EXE':str(BIN/('graph-memory-service'+SUFFIX))}
                try:
                    managed=subprocess.run([str(BIN/('mcp_server'+SUFFIX))],input=v1payload,text=True,encoding='utf8',capture_output=True,timeout=25,cwd=cwd,env=managed_env,creationflags=FLAGS)
                    self.assertEqual(managed.returncode,0,managed.stderr)
                    managed_replies=[json.loads(line) for line in managed.stdout.splitlines()]
                    for reply in managed_replies:
                        self.assertNotIn('error',reply)
                        self.assertFalse(reply.get('result',{}).get('isError',False),reply)
                    self.assertIn('PowerShell',json.dumps(managed_replies[2]))
                    self.assertNotEqual(json.loads((data/'service.json').read_text())['instance_id'],fresh['instance_id'])
                finally:
                    managed_info=json.loads((data/'service.json').read_text())
                    if managed_info['instance_id']!=fresh['instance_id']:
                        os.kill(managed_info['pid'],9)
                        time.sleep(.3)
            finally:
                for process in processes:
                    if process.poll() is None:
                        process.kill()
                    process.communicate(timeout=5)

if __name__=='__main__':
    unittest.main()
