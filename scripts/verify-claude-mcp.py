"""Opt-in real Claude CLI acceptance. Uses synthetic data, never user MCP config.

Requires a configured Claude model provider; starts two model-backed sessions.
Raw synthetic transcripts stay under ignored artifacts/host-acceptance.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[1]
TOOLS = {"memory_projects", "memory_context", "memory_search", "memory_write", "memory_update"}


def stop_tree(process):
    if process.poll() is None:
        if os.name == "nt":
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                           capture_output=True, timeout=15, check=True)
        else:
            process.kill()
        process.wait(timeout=15)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    default_cli = Path(os.environ.get("APPDATA", "")) / "npm/node_modules/@anthropic-ai/claude-code/bin/claude.exe"
    parser.add_argument("--claude-exe", type=Path, default=default_cli)
    parser.add_argument("--binary-dir", type=Path, default=ROOT / "artifacts/ci-desktop")
    parser.add_argument("--model", default="sonnet")
    parser.add_argument("--timeout", type=int, default=240)
    args = parser.parse_args()
    if not __debug__:
        parser.error("Acceptance assertions must be enabled; do not run Python with -O")
    if args.timeout <= 0:
        parser.error("--timeout must be positive")
    suffix = ".exe" if os.name == "nt" else ""
    service = (args.binary_dir / ("graph-memory-service" + suffix)).resolve()
    mcp = (args.binary_dir / ("mcp_server" + suffix)).resolve()
    cli = args.claude_exe.resolve()
    for binary in (cli, service, mcp):
        if not binary.is_file():
            parser.error(f"Missing executable: {binary}")
    run = ROOT / "artifacts/host-acceptance" / uuid.uuid4().hex
    run.mkdir(parents=True)
    data = run / "data"
    cwd = run / "session"
    cwd.mkdir()
    env = {k: v for k, v in os.environ.items() if not k.upper().startswith(("GM_", "ANTHROPIC"))}
    env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] = "1"
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    report = {"host": "Claude Code", "requested_model": args.model, "passed": False}
    report["version"] = subprocess.check_output([str(cli), "--version"], env=env, text=True, timeout=20).strip()

    with (run / "service.stdout.log").open("w") as out, (run / "service.stderr.log").open("w") as err:
        owned = subprocess.Popen([str(service), "--data-dir", str(data)], cwd=cwd,
                                 env=env, stdout=out, stderr=err, creationflags=flags)
        try:
            deadline = time.monotonic() + 20
            while True:
                if owned.poll() is not None:
                    raise RuntimeError("Isolated service exited; inspect service.stderr.log")
                try:
                    info = json.loads((data / "service.json").read_text(encoding="utf8"))
                    break
                except (OSError, ValueError):
                    if time.monotonic() > deadline:
                        raise TimeoutError("Isolated service readiness timed out")
                    time.sleep(.1)

            def api(path, body=None):
                request = urllib.request.Request(info["api_url"] + path,
                    data=None if body is None else json.dumps(body).encode(),
                    headers={"Content-Type": "application/json"})
                with urllib.request.urlopen(request, timeout=10) as response:
                    return json.load(response)

            if api("/api/service")["pid"] != owned.pid:
                raise RuntimeError("Discovery does not belong to the isolated service")
            project = api("/api/v1/projects", {"name": "Host acceptance Alpha"})["id"]
            other = api("/api/v1/projects", {"name": "Unrelated Beta"})["id"]
            secret = "ALPHA-" + uuid.uuid4().hex
            forbidden = "BETA-" + uuid.uuid4().hex
            revised = "CORRECTED-" + uuid.uuid4().hex
            title = "Host-check-" + uuid.uuid4().hex
            for scope, pid, body in [("project", project, "交接校验码：" + secret),
                                     ("project", other, "其他项目校验码：" + forbidden),
                                     ("personal", None, "合成测试经验：核对记录版本后再纠正，保留来源与适用条件。")]:
                payload = {"memory_scope": scope, "event_id": uuid.uuid4().hex,
                           "content": body, "source": "synthetic host acceptance", "conditions": "test only"}
                if pid:
                    payload["project_id"] = pid
                api("/api/v1/memories", payload)
            config = {"mcpServers": {"graph_memory_v1": {"command": str(mcp), "args": [], "env": {
                "GM_MEMORY_V1": "1", "GM_SERVICE_DIR": str(data), "GM_SERVICE_EXE": str(service)}}}}
            config_path = run / "mcp.json"
            config_path.write_text(json.dumps(config), encoding="utf8")
            instructions = (ROOT / "desktop/AGENT_INSTRUCTIONS.md").read_text(encoding="utf8")

            def session(number, prompt):
                command = [str(cli), "-p", "--model", args.model, "--tools", "",
                    "--allowedTools", *["mcp__graph_memory_v1__" + t for t in sorted(TOOLS)],
                    "--strict-mcp-config", "--mcp-config", str(config_path),
                    "--settings", '{"disableAllHooks":true}', "--setting-sources", "user",
                    "--disable-slash-commands", "--no-session-persistence", "--permission-mode", "dontAsk",
                    "--append-system-prompt", instructions, "--output-format", "stream-json", "--verbose"]
                transcript = run / f"session-{number}.jsonl"
                with transcript.open("w", encoding="utf8") as stdout, (run / f"session-{number}.stderr.log").open("w", encoding="utf8") as stderr:
                    process = subprocess.Popen(command, cwd=cwd, env=env, stdin=subprocess.PIPE,
                        stdout=stdout, stderr=stderr, text=True, encoding="utf8", creationflags=flags)
                    try:
                        process.communicate(prompt, timeout=args.timeout)
                    except subprocess.TimeoutExpired:
                        stop_tree(process)
                        raise TimeoutError(f"Claude session {number} exceeded {args.timeout}s")
                    if process.returncode != 0:
                        raise RuntimeError(f"Claude session {number} exited {process.returncode}; inspect logs")
                events = [json.loads(line) for line in transcript.read_text(encoding="utf8").splitlines() if line.strip()]
                results = [e for e in events if e.get("type") == "result"]
                if not results or results[-1].get("is_error"):
                    raise RuntimeError(f"Session {number} did not finish successfully")
                final = results[-1].get("result", "")
                calls = [block["name"] for e in events if e.get("type") == "assistant"
                         for block in e.get("message", {}).get("content", []) if block.get("type") == "tool_use"]
                if any(not name.startswith("mcp__graph_memory_v1__") for name in calls):
                    raise AssertionError("Session invoked a tool outside the isolated MCP")
                if forbidden in transcript.read_text(encoding="utf8"):
                    raise AssertionError("Unrelated project content leaked into session")
                return final, calls, events

            first, calls, events = session(1,
                f"这是已授权的合成隔离验收。请按使用约定查询项目 Host acceptance Alpha 的交接背景，"
                f"回答其交接校验码；不要读取其他项目。然后创建一条个人经验，标题为 {title}，正文为‘合成测试初稿’，"
                f"source=synthetic host acceptance，conditions=test only。随后将同一条记录纠正为‘合成测试纠正版 {revised}’，"
                "不要创建第二条。最终答复包含项目校验码及新记录ID、版本，不把合成经验称为现实已验证结论。")
            assert secret in first, "First session did not adopt the retrieved project value"
            discovered = {name.removeprefix("mcp__graph_memory_v1__") for e in events if e.get("type") == "system"
                          for name in e.get("tools", []) if name.startswith("mcp__graph_memory_v1__")}
            assert discovered == TOOLS, f"Host discovery mismatch: {discovered}"
            records = api("/api/v1/search", {"scope": "personal", "query": title})["memories"]
            assert len(records) == 1 and records[0]["version"] == 2 and revised in records[0]["content"], "Independent write/update verification failed"
            assert api("/api/v1/search", {"scope": "all"})["total"] == 4, "Unexpected writes or duplicates"
            second, second_calls, _ = session(2,
                f"这是一个全新的合成验收会话。请找回个人经验 {title}，逐字给出当前正文、ID和版本。"
                "如果没有检索证据就说找不到，不猜答案，不新建或修改记录。")
            assert revised in second and records[0]["id"] in second, "New session did not retrieve/adopt corrected content"
            assert not any(name.endswith(("memory_write", "memory_update")) for name in second_calls), "Read-only follow-up mutated records"
            counters = api("/api/v1/agent/status")["activity"]
            assert {c["tool"] for c in counters if c["successes"] > 0} >= {"memory_projects", "memory_write", "memory_update"}, "Missing successful host calls"
            report.update(passed=True, discovery=sorted(discovered), first_calls=calls, second_calls=second_calls,
                          independent_record_version=records[0]["version"], total_records=4,
                          retrieved_project_value=True, new_session_adopted_correction=True,
                          unrelated_project_leaked=False, counters=counters)
        finally:
            stop_tree(owned)
            (run / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf8")
            print(f"Evidence: {run}", flush=True)
    print("PASS real Claude host: discovery, project retrieval, write, versioned correction, new-session adoption")


if __name__ == "__main__":
    main()
