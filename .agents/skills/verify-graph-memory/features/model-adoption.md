# Real multi-turn model adoption and cross-host recall

User path: work with an Agent across turns without asking it to save each turn, then ask another session/host for that project's work over a date range. Sources: `scripts/verify-work-log-adoption.py` (`ClaudeSession`, `main`, `codex_read`), `desktop/AGENT_INSTRUCTIONS.md`, `docs/HOST_COMPATIBILITY.md`. This is opt-in external model execution, not part of the credential-free default run.

## Launch, authentication and doctor

Build service/MCP first as in SKILL.md. Use an already authorized model route and configured Claude Code executable; the script does not log in or install a host. Optional Codex needs its executable plus readable `CODEX_HOME/config.toml` (default user .codex). Resolve actual local paths, do not assume the npm layout on another host. Do not use Python -O: checks require assertions.

```powershell
python -X utf8 scripts/verify-work-log-adoption.py --binary-dir src-tauri/target/debug
if ($LASTEXITCODE -ne 0) { throw 'model adoption failed; retain evidence' }
# To include Codex, append: --codex-exe <actual-absolute-path-to-codex.exe>
# Claude override: --claude-exe <actual-path> --model <authorized-alias> --timeout 240
```

The script prints `artifacts/work-log-adoption/<uuid>/` immediately. Its startup doctor hashes binaries, checks owned PID and directory through `/api/service`, then requires `/api/v1/projects`. Actual first model request checks route authentication. Unlike the offline helper, it does not automatically run the full health/protocol doctor before each model turn. To diagnose a live run, use the recorded identity and owned directory:

```powershell
$run = '<absolute evidence directory printed by this invocation>'
$instance = Get-Content "$run/service-identity.json" -Raw | ConvertFrom-Json
python -X utf8 .agents/skills/verify-graph-memory/scripts/verify.py doctor --url $instance.api_url --pid $instance.pid --data-dir "$run/data"
```

Run that read-only check on unexpected behavior; fail if ownership changed. After cleanup, doctor correctly fails because the owned service has stopped. Diagnose authentication via redacted stderr, never print credentials or broaden permissions to force a pass.

## Isolation and drive

Unique synthetic data and working directory per run; no real knowledge or configuration edits. Claude receives strict temporary MCP config, five v1 tools only, disabled builtin tools/hooks/automatic memory/slash commands/session persistence, and explicit Agent instructions. Three user turns contain simulated progress, not an instruction to write a log. This tests adoption **with installed guidance**, not unguided behavior or actual execution of the simulated export repair.

1. First turn creates exactly one November16 log with a random detail marker and incomplete status.
2. Second turn updates the same ID/version, completed status and both random details. User claims must remain statement/user-confirmed evidence, not tool verification.
3. Third turn creates a November17 planned log, leaving yesterday's corrected record identical. Writer result events must share one session ID.
4. New Claude session asks for November16–22. It must use Alpha/project/work_date/[16,23), adopt both markers and IDs and make no write/update calls; independent HTTP before/after records must match. Beta's random sentinel must never enter either transcript.
5. Optional Codex fresh process has shell/apps/multi-agent off, only projects/search allowed and other configured MCP servers disabled. Temporary overrides cover **command, args, GM_SERVICE_DIR, GM_SERVICE_EXE and GM_API_URL**: directory-managed discovery takes precedence over API URL. It must use the same project/date query and adopt details without mutations.

Inspect final answers as well as assertions: completion vs plans, actual sources, date gaps and stale titles. Title consistency and prose coverage are not fully machine-graded. Scripts check current records, not an immutable historical event timeline. Only two logs are used: this model run does not prove multi-page reporting.

## Evidence, cleanup and limits

Retain writer/reader prompts and JSONL, stderr, three HTTP snapshots, service-identity.json, config, service.log and report.json. Optional Codex adds codex.jsonl/stderr. Report binds binary/instruction hashes, host versions, observed model labels, pass flags and owned-service stop status. A requested alias is not proof of model identity; previous sonnet-alias calls self-reported GLM-5.3.

Sessions and owned service are stopped in finally (Windows owned process trees only); synthetic `data/` and `session/` are deliberately retained inside the evidence directory. This script does **not** remove them like the offline helper. Confirm report and transcripts survive and owned_service_stopped=true; inspect cleanup exceptions, do not infer success from a partial transcript. On interruption verify recorded identity before any manual cleanup. Never stop the user's shared daily service. Separate UUID instances may coexist; one driver owns each stream session.

Previous successful installed-0.4.0 run: `artifacts/work-log-adoption/b88f33d9140b429793046c3cac91c5e5/`. First failing Codex isolation attempt is documented in HOST_COMPATIBILITY; repaired runs do not erase it. These are historical evidence, not a fresh execution of this skill update. Excluded: Codex autonomous writes, Hermes model adoption, long-term miss rates, current desktop-host reload, native UI, real user history and universal model reliability.
