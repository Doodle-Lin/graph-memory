"""Integration check for the isolated headless service; requires built binaries."""
import json
import os
from pathlib import Path
import socket
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
                # v1 responses are UTF-8 JSON; without an explicit charset a strict
                # client decodes Chinese as Latin-1 and silently corrupts it.
                with urllib.request.urlopen(urllib.request.Request(info['api_url']+'/api/v1/projects'),timeout=3) as response:
                    self.assertEqual(response.headers.get('Content-Type'),'application/json; charset=utf-8')
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
                # Real MCP round trips, including a stale correction from another client.
                def call_tool(name, arguments):
                    request={'jsonrpc':'2.0','id':20,'method':'tools/call','params':{'name':name,'arguments':arguments}}
                    result=subprocess.run([str(BIN/('mcp_server'+SUFFIX))],input=json.dumps(request)+'\n',text=True,encoding='utf8',capture_output=True,timeout=10,cwd=cwd,env={**env,'GM_API_URL':info['api_url'],'GM_MEMORY_V1':'1'},creationflags=FLAGS)
                    self.assertEqual(result.returncode,0,result.stderr)
                    reply=json.loads(result.stdout)['result']
                    return reply, json.loads(reply['content'][0]['text']) if not reply.get('isError') else None
                reply, projects=call_tool('memory_projects',{})
                self.assertEqual({x['id'] for x in projects['projects']},{alpha,beta})
                reply, results=call_tool('memory_search',{'project_id':alpha,'query':'deployment'})
                self.assertEqual([x['id'] for x in results['memories']],[a['id']])
                search_schema=next(t for t in replies[0]['result']['tools'] if t['name']=='memory_search')['inputSchema']['properties']
                self.assertTrue({'scope','from','to','date_field','kind','limit','offset'}.issubset(search_schema))
                reply, page=call_tool('memory_search',{'scope':'project','project_id':alpha,'from':'2000-01-01T00:00:00+08:00','to':'2100-01-01T00:00:00+08:00','limit':1})
                self.assertFalse(reply.get('isError'),reply)
                self.assertEqual(page['total'],1)
                self.assertEqual(page['memories'][0]['id'],a['id'])
                self.assertIsNone(page['next_offset'])
                invalid, _=call_tool('memory_search',{'from':'2026-11-01'})
                self.assertTrue(invalid.get('isError'),invalid)
                # The Avalonia personal view serializes an optional project as JSON null.
                personal_page=api(info,'/api/v1/search',{'project_id':None,'query':''})
                self.assertEqual(personal_page['total'],2)
                self.assertTrue(all(x['memory_scope']=='personal' for x in personal_page['memories']))
                reply, corrected=call_tool('memory_update',{'id':experience['id'],'expected_version':3,'content':'PowerShell correction verified through MCP'})
                self.assertFalse(reply.get('isError'),reply)
                self.assertEqual(corrected['memory']['version'],4)
                stale, _=call_tool('memory_update',{'id':experience['id'],'expected_version':3,'content':'must not overwrite'})
                self.assertTrue(stale.get('isError'),stale)
                reply, result=call_tool('memory_context',{'query':'PowerShell'})
                self.assertIn('PowerShell correction verified through MCP',result['text'])
                self.assertNotIn('must not overwrite',result['text'])
                counters={x['tool']:x for x in api(info,'/api/v1/agent/status')['activity']}
                self.assertEqual(counters['memory_update']['calls'],2)
                self.assertEqual(counters['memory_update']['successes'],1)
                # Retraction hides a record without deleting it; history keeps every version.
                self.assertEqual([r['id'] for r in api(info,'/api/v1/search',{'scope':'project','project_id':alpha})['memories']],[a['id']])
                retracted=api(info,'/api/v1/memories/retract',{'id':a['id'],'expected_version':1,'reason':'结论被证伪'})['memory']
                self.assertEqual((retracted['version'],retracted['retract_reason']),(2,'结论被证伪'))
                self.assertIsNotNone(retracted['retracted_at'])
                hidden=api(info,'/api/v1/search',{'scope':'project','project_id':alpha})
                self.assertEqual((hidden['total'],hidden['retracted_excluded']),(0,1))
                shown=api(info,'/api/v1/search',{'scope':'project','project_id':alpha,'include_retracted':True})
                self.assertEqual((shown['total'],shown['memories'][0]['retract_reason']),(1,'结论被证伪'))
                with self.assertRaises(urllib.error.HTTPError) as already:
                    api(info,'/api/v1/memories/retract',{'id':a['id'],'expected_version':2})
                self.assertEqual(already.exception.code,400)
                restored_record=api(info,'/api/v1/memories/restore',{'id':a['id'],'expected_version':2,'reason':'复核通过'})['memory']
                self.assertEqual(restored_record['version'],3)
                self.assertIsNone(restored_record['retracted_at'])
                self.assertIsNone(restored_record['retract_reason'])
                history=api(info,'/api/v1/memories/history',{'id':a['id']})['history']
                self.assertEqual([v['version'] for v in history],[3,2,1])
                self.assertIsNotNone(history[1]['retracted_at'])
                self.assertIsNone(history[2]['retracted_at'])
                self.assertEqual([r['id'] for r in api(info,'/api/v1/search',{'scope':'project','project_id':alpha})['memories']],[a['id']])
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

class ProtocolLimitsTest(unittest.TestCase):
    """L5-02: the local HTTP protocol must enforce resource limits on real sockets."""

    def setUp(self):
        self.env={k:v for k,v in os.environ.items() if not k.startswith('GM_')}
        self.folder=tempfile.TemporaryDirectory(prefix='gm-limits-')
        self.addCleanup(self.folder.cleanup)
        self.cwd=Path(self.folder.name)
        self.data=self.cwd/'isolated'
        self.process=subprocess.Popen(
            [str(BIN/('graph-memory-service'+SUFFIX)),'--data-dir',str(self.data)],
            cwd=self.cwd,env=self.env,stdout=subprocess.PIPE,stderr=subprocess.PIPE,creationflags=FLAGS)
        self.addCleanup(self._stop)
        deadline=time.monotonic()+15
        info=None
        while time.monotonic()<deadline:
            if self.process.poll() is not None:
                self.fail(self.process.communicate()[1].decode(errors='replace'))
            try:
                info=json.loads((self.data/'service.json').read_text())
                break
            except (OSError,ValueError):
                time.sleep(.05)
        if info is None:
            self.fail('service readiness timed out')
        self.port=int(info['api_url'].rsplit(':',1)[1])

    def _stop(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.communicate(timeout=5)
        # A leaked service keeps a lock on its exe and data directory; also kill the
        # pid published in service.json, then wait until it is really gone.
        try:
            published=int(json.loads((self.data/'service.json').read_text())['pid'])
        except (OSError,ValueError,KeyError):
            published=None
        for pid in {self.process.pid,published}-{None}:
            for _ in range(20):
                try:
                    os.kill(pid,9)
                except OSError:
                    break
                time.sleep(.1)

    def raw(self,payload,shutdown_write=False,read=True,timeout=25):
        """Send raw bytes, return (status, headers dict, raw header text).

        With read=False the open socket is returned to the caller (used to hold
        connections open); the caller owns closing it.
        """
        sock=socket.create_connection(('127.0.0.1',self.port),timeout=5)
        sock.settimeout(timeout)
        if not read:
            sock.sendall(payload)
            return sock
        try:
            sock.sendall(payload)
            if shutdown_write:
                sock.shutdown(socket.SHUT_WR)
            data=b''
            while b'\r\n\r\n' not in data:
                try:
                    chunk=sock.recv(4096)
                except ConnectionResetError:
                    # Windows can turn an immediate close into an RST that discards the
                    # buffered response (seen with the 40 KB header rejection).
                    break
                if not chunk:
                    break
                data+=chunk
            if not data:
                raise ConnectionResetError('connection reset before any response bytes')
            head=data.split(b'\r\n\r\n',1)[0].decode('latin-1')
            lines=head.split('\r\n')
            headers={}
            for line in lines[1:]:
                if ':' in line:
                    name,value=line.split(':',1)
                    headers[name.strip().lower()]=value.strip()
            return int(lines[0].split(' ')[1]),headers,head
        finally:
            sock.close()

    def assert_json_error(self,status,headers,expected):
        self.assertEqual(status,expected,headers)
        self.assertEqual(headers.get('content-type'),'application/json; charset=utf-8')
        self.assertTrue(headers.get('content-length','').isdigit(),headers)

    def test_request_line_and_header_limits(self):
        filler=b'x'*(40*1024)
        status,headers,_=self.raw(
            b'GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Filler: '+filler+b'\r\n\r\n')
        self.assert_json_error(status,headers,431)
        status,headers,_=self.raw(b'GET /'+b'a'*(8*1024+100)+b' HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n')
        self.assert_json_error(status,headers,431)
        # The service must still answer normal traffic after rejecting those.
        status,headers,_=self.raw(b'GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n')
        self.assertEqual(status,200,headers)

    def test_body_limit_and_lying_content_length(self):
        # Declared size is over the 1 MiB limit: rejected while reading, no body sent at all.
        status,headers,_=self.raw(
            b'POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\n'
            b'Content-Type: application/json\r\nContent-Length: 2097152\r\n\r\n')
        self.assert_json_error(status,headers,413)
        # "Lying" Content-Length: declares a huge size but sends only a few bytes.
        status,headers,_=self.raw(
            b'POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\n'
            b'Content-Type: application/json\r\nContent-Length: 999999999\r\n\r\n{}')
        self.assert_json_error(status,headers,413)
        # Declared body is within the limit but the client stops early: malformed, not a hang.
        status,headers,_=self.raw(
            b'POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\n'
            b'Content-Type: application/json\r\nContent-Length: 100\r\n\r\n{"name":',shutdown_write=True)
        self.assert_json_error(status,headers,400)

    def test_disallowed_api_methods_return_405_with_allow(self):
        for method,path in [(b'DELETE',b'/api/v1/projects'),(b'PUT',b'/api/v1/memories'),(b'PATCH',b'/api/v1/search')]:
            status,headers,_=self.raw(method+b' '+path+b' HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n')
            self.assert_json_error(status,headers,405)
            self.assertEqual(headers.get('allow'),'GET, POST')
        # The method rejection must not have mutated anything.
        status,headers,_=self.raw(b'GET /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n')
        self.assertEqual(status,200,headers)

    def test_malformed_requests_are_400_and_never_crash(self):
        for payload in [b'GARBAGE-NO-SPACES\r\n\r\n',b'GET\r\n\r\n',b'GET /ok\r\n\r\n',
                        b'GET  HTTP/1.1\r\n\r\n',b'GET /x HTTP/2.0\r\n\r\n',
                        b'GET /x HTTP/1.1\r\nBroken-Header\r\n\r\n']:
            status,headers,_=self.raw(payload)
            self.assert_json_error(status,headers,400)
        # Still alive after all of that garbage.
        status,headers,_=self.raw(b'GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n')
        self.assertEqual(status,200,headers)

    def test_excess_concurrent_connections_get_503(self):
        # Hold 120 idle connections open without sending a request: the limit is
        # enforced when the connection is admitted, so the excess ones must be
        # refused immediately with 503 (no queueing) while the first 64 wait.
        held=[]
        try:
            for _ in range(120):
                sock=socket.create_connection(('127.0.0.1',self.port),timeout=5)
                sock.settimeout(3)
                held.append(sock)
            time.sleep(1.5)
            rejected=0
            waiting=0
            timed_out=0
            for sock in held:
                try:
                    data=b''
                    while b'\r\n\r\n' not in data:
                        chunk=sock.recv(4096)
                        if not chunk:
                            break
                        data+=chunk
                    if data.startswith(b'HTTP/1.1 503'):
                        rejected+=1
                        head=data.split(b'\r\n\r\n',1)[0].decode('latin-1').lower()
                        self.assertIn('content-type: application/json; charset=utf-8',head)
                        self.assertIn('content-length:',head)
                        self.assertIn('connection: close',head)
                    elif data.startswith(b'HTTP/1.1 408'):
                        # An admitted connection that sat idle past the 15s deadline.
                        timed_out+=1
                    elif data:
                        self.fail('unexpected response under load: %r' % data[:120])
                except socket.timeout:
                    # An admitted connection is still waiting for its request.
                    waiting+=1
                except ConnectionError:
                    # A server-side close can arrive as RST on Windows.
                    rejected+=1
            self.assertGreaterEqual(rejected,120-64,
                'only %d of 120 connections were rejected' % rejected)
            self.assertEqual(waiting+timed_out,64,
                'expected only the admitted connections to be served, got %d open / %d timed out'
                % (waiting,timed_out))
        finally:
            for sock in held:
                try:
                    sock.close()
                except OSError:
                    pass
        # Once the clients disconnect, the service must recover.
        deadline=time.monotonic()+30
        while time.monotonic()<deadline:
            try:
                status,headers,_=self.raw(b'GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n',timeout=5)
                if status==200:
                    return
            except OSError:
                pass
            time.sleep(.25)
        self.fail('service did not recover after the concurrency burst')

if __name__=='__main__':
    unittest.main()
