# Work logs, project/date recall and correction

User path: an Agent records a meaningful work milestone, updates that task on the same local day, appends next-day work, then answers a period question. This feature verifies the public protocol deterministically; [model adoption](model-adoption.md) verifies whether an actual Agent chooses to use it.

Sources: `src-tauri/src/work_log.rs` (`WorkLog::validate/schema`), `src-tauri/src/memory_store.rs` (`create`, `update`, `search`, `parse_work_log`), `src-tauri/src/bin/mcp_server.rs` (`tools/list`, `tools/call`), `tests/test_work_logs.py`. Work dates are declared local civil days, not creation/update timestamps or a historical snapshot.

## Preconditions and command

From the repository root, build as in SKILL.md, then:

```powershell
python -X utf8 .agents/skills/verify-graph-memory/scripts/verify.py run
if ($LASTEXITCODE -ne 0) { throw 'work-log acceptance failed; retain the printed evidence directory' }
```

The helper owns its fresh service, schema, data and random port. It creates Alpha/Beta via `POST /api/v1/projects` and uses returned IDs, never assumed IDs from the user's library. A personal note without work metadata represents unknown occurrence date. Each MCP call runs a new process after doctor.

## Actual actions and expected state

1. `memory_write`: Alpha `kind=work_log`, full `work_log={date:"2024-02-29",utc_offset:"+08:00",status:"in_progress",outcome:"…",next_steps:["…"]}`, concrete synthetic content/source. Repeat exactly the same event ID/request: same ID, no duplicate.
2. `memory_update` with ID/expected_version=1, full updated content/title and complete work_log: same ID becomes v2/completed, date stays February 29 although written/updated now.
3. Append March 1 `planned` under a distinct event ID; append a February 29 log in Beta as a scope sentinel. The February 29 Alpha record is unchanged by these additions.
4. `memory_search` with `scope=project`, Alpha ID, `kind=work_log`, `date_field=work_date`, `from=2024-02-29`, `to=2024-03-02`, `limit=1`: total2, page1 March1, next_offset1, page2 February29/v2, next_offset null. No Beta log.
5. Set `to=2024-03-01`: only February29 returns. Personal work-date search returns0; all-scope work-date search returns3, excluding the undated personal note.
6. Invalid February29 in 2023 and null work_log update must return MCP isError. Read back the original log: still one matching ID at v2, no accepted invalid change.
7. Full backup restore stage re-queries both Alpha logs and compares all persisted fields (only search score excluded); re-exported full payload must equal the original backup.

Evidence: `actions.json` includes requests, MCP initialization, replies and rejected calls; `work-logs.json` includes dates/statuses/IDs/versions and pages. `result.json.features.work_logs` and `backup_restore` must pass separately. Follow SKILL cleanup; evidence remains after owned temporary data and both services are removed.

Excluded: autonomous model decisions, native date UI (not implemented), IANA timezone conversion, cross-device sync, event-as-of snapshots, concurrent snapshot pagination and large reports. Only a two-item deterministic paging case is covered here; the full contract has additional input validation cases. Zero logs means no matching recorded coverage, never proof of no work.
