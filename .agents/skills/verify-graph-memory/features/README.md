# Feature map

| Feature | Driver | Acceptance and current coverage |
|---|---|---|
| [MCP memory continuity](mcp-continuity.md) | fresh actual stdio processes + isolated HTTP service | Fresh pass 2026-11-17; not model adoption |
| [Work logs and period recall](work-logs.md) | project creation + actual MCP + restore readback | Fresh pass 2026-11-17; dates, project boundaries, two-page recall, corrections and metadata restore |
| [Backup and restore](backup-restore.md) | actual CLI + restored service | Fresh pass 2026-11-17; full payload equal, excludes merge/overwrite/UI |
| [Native desktop editing](desktop-editing.md) | real Windows input + screenshots + HTTP readback | Not rerun this update; previous functional pass, visual capture blocked |
| [Model adoption](model-adoption.md) | real Claude multi-turn/new session + optional Codex reader | Not rerun this update; prior 0.4.0 sample passed; Codex writing/Hermes unverified |

Current self-validation: `artifacts/verification/run-44be0b67a1034a9f98dd73f68ce215d0/`, source c7a5b22 plus this skill/helper update, actual MCP version0.4.0. Launch/build → source doctor → three mapped features → restored doctor → final doctors → cleanup all passed. Both owned services stopped, scratch removed, actions/backup/build/result evidence survived. Build output and source provenance are retained alongside this run. The helper is stdlib-only; no model, auth or user workspace was used in this fresh run. An initial successful run (`run-b45900a163194c9da259a47821451537`) is also retained; the final run strengthens rejection checks to compare complete fields and total record count.

Past native evidence: `artifacts/verification/ui-8e7491299ba04c7d88dba467a8b18c3a/`: UIA/public control input and HTTP create/edit/cancel/save-close passed; screenshot capture timed out and coordinate geometry was unavailable, so visual acceptance remains blocked. Owned processes stopped; that attempt's scratch was retained after cleanup policy rejection. This is not a fresh visual pass and must not be counted as complete native coverage.

Past model evidence: `artifacts/work-log-adoption/b88f33d9140b429793046c3cac91c5e5/`, installed0.4.0 binary hashes, Claude Code2.1.295 (response self-reported GLM-5.3), Codex CLI0.166.0 read-only cross-host adoption; owned service stopped. Rerun the opt-in feature to claim current route/host adoption. Claude also provided a tools-disabled documentation draft for this update; Codex corrected its overbroad “always writes report” cleanup claim and kept model execution separate from the offline self-validation.

The skill is usable and self-validated for the three offline features. It is not an all-surfaces green verdict. Use `$maintain-verification-skill` for subsequent upkeep; do not create a background schedule from this map.

Original successful evidence: `artifacts/verification/run-e6ae3f06ea764896aac8c233a37c8b60/`. Result passed, both owned services stopped, scratch removed, evidence still present. Tested binary identities are in build.json; source baseline d6ef91a plus newly generated skill, product files unchanged.

Failed attempts retained: `run-9ac51e49823a4ea6838d32f34858e2a8` (Windows extended path string mismatch; repaired with samefile), `run-d229de05474d4c8da788a99de43572f3` (search's transient score incorrectly compared as persisted field; now removed only that field before full record equality). Both failed runs cleaned owned state. These were verifier errors, not weakened product requirements.

Claude supplied a desktop workflow draft with tools disabled. Its screenshot-only evidence rule and guessed host-configuration actions were rejected; final instructions require public state readback and exclude host configuration changes.
