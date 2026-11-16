---
name: verify-graph-memory
description: Verify Graph Memory through real isolated MCP sessions, backup CLI and native Avalonia user flows. Use for repeatable end-to-end acceptance, regression diagnosis and evidence collection; distinguish native UI, headless controls and model adoption.
---

# Graph Memory verification

Run from the repository root (currently `E:/workspace/graph-memory`). Read `docs/design/GOVERNANCE_20261116.md` and the [feature map](features/README.md) first. P4 is the delivery baseline; P5 on `codex/p5-semantic-hold-20261116` is unaccepted. Recheck these facts on future runs; do not assume a clean working tree or current binaries.

## Launch and dependencies

Windows x64, Python 3.12+, Rust MSVC toolchain and C++ build tools. .NET 10 SDK is additionally required for desktop checks; on the original host it is `C:/Users/302288/.codex/tools/dotnet/dotnet.exe`. Resolve installed tools rather than guessing another machine has this path.

```powershell
git status --short
git rev-parse HEAD
cargo build --manifest-path src-tauri/Cargo.toml --locked --no-default-features --bin graph-memory-service --bin mcp_server --bin graph-memory-backup
if ($LASTEXITCODE -ne 0) { throw 'build failed' }
python -X utf8 .agents/skills/verify-graph-memory/scripts/verify.py run
if ($LASTEXITCODE -ne 0) { throw 'verification failed; inspect printed evidence path' }
```

The driver accepts `--bin-dir ABSOLUTE_DIRECTORY` for freshly built or extracted binaries. It never builds implicitly. Record HEAD/dirty state and build output alongside its `build.json` hashes; hashes identify actual tested binaries, not proof they match arbitrary source edits. Do not run the default desktop build script against a user's running `artifacts/desktop` installation. Use the CI candidate or an isolated publish directory.

No external account, token or model is required. The driver strips inherited `GM_*` settings, creates unique temporary source/restore/draft directories, starts its own service with random local port, and uses `GM_MEMORY_V1=1` / `GM_API_URL` only in fresh MCP child processes. MemoryStore initializes schema only in the owned empty source directory. No real knowledge or host configuration is read or changed.

## Doctor (read-only)

`run` executes doctor after launching each service and again before cleanup. For a manually launched, owned instance:

```powershell
python -X utf8 .agents/skills/verify-graph-memory/scripts/verify.py doctor --url $info.api_url --pid $ownedService.Id --data-dir $runData
```

Repeat on a new session or unexpected behavior. Doctor compares `/api/service` PID, directory (Windows file identity), address and protocol against supplied ownership; then requires `/api/health` status and `/api/v1/projects` to respond. An open port or discovery file alone is insufficient: discovery precedes MemoryStore readiness. For a manual process also verify `StartInfo.FileName` and its hash. Doctor does not authenticate other local processes; the product currently has no token-based local authentication. Never attach to an unrelated service because a port happens to answer.

Concurrent instances work with separate data directories and ports. Two writers cannot own one workspace. One driver controls each native window; do not share a live UI instance across agents.

## Drive and evidence

Use the feature files, which cite source entry points. The helper invokes actual stdio MCP and the shipped backup CLI, not internal methods. Its default run exercises both MCP and backup mapped features. Native UI is a separate manual/computer-use feature and is not covered by a service pass.

Each run creates `artifacts/verification/run-UUID/` outside disposable state:

- `build.json`: executable paths and SHA-256.
- `source-doctor.json`, `restored-doctor.json`: instance identity/readiness.
- `actions.json`: commands, synthetic requests, responses and exit codes.
- service logs and `workspace.gmbackup`: synthetic backup retained as evidence.
- `result.json`: pass/failure, scratch path, process cleanup and removal outcome.

Evidence may contain synthetic record bodies; do not change this harness to use real private records. Preserve failed attempts. Never call a screenshot substitute for state verification: use UI screenshots plus public API readback, and distinguish content persistence from visual usability.

## Cleanup

The helper owns process handles, terminates only those services, escalates to killing only those handles after timeout, and waits. It checks its UUID scratch directory is within the temporary root before deleting it. It leaves evidence and verifies actions/result files survive. Any cleanup error fails the run. If interrupted outside normal exception handling, inspect recorded PID/path, verify executable/start identity before stopping, and remove only that run's scratch; never kill by process name or erase a shared workspace.

For native UI, save/discard only synthetic drafts, close the owned window, then stop the separately owned service. Closing a normal desktop window alone does not stop the service. Preserve screenshots/logs before scratch removal.

## Broader checks and limits

`scripts/test-windows.ps1 -Dotnet <dotnet-path> -Python python` runs full source regression and P4 quality gates. Editor.Tests uses headless controls/reflection for some paths; its 215 assertions are not native input/screenshot proof. `--smoke-test` skips window creation. `scripts/verify-release.ps1` separately checks an actual package; debug success does not validate a ZIP. `scripts/verify-claude-mcp.py` uses a real external model route and is excluded from the default offline verifier; do not infer model adoption from stdio success.

Current self-validation and gaps are in `features/README.md`. Use `$maintain-verification-skill` when source, controls, CLI or package behavior changes. This skill grants no deployment or host-configuration permission and creates no schedule.
