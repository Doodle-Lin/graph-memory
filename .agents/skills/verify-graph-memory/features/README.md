# Feature map

| Feature | Driver | Acceptance and current coverage |
|---|---|---|
| [MCP memory continuity](mcp-continuity.md) | fresh actual stdio processes + isolated HTTP service | Self-validated 2026-11-16; not model adoption |
| [Backup and restore](backup-restore.md) | actual CLI + restored service | Self-validated 2026-11-16; excludes merge/overwrite/UI |
| [Native desktop editing](desktop-editing.md) | real Windows input + screenshots + HTTP readback | Instructions source-checked; not exercised in this skill creation |

Successful evidence: `artifacts/verification/run-e6ae3f06ea764896aac8c233a37c8b60/`. Result passed, both owned services stopped, scratch removed, evidence still present. Tested binary identities are in build.json; source baseline d6ef91a plus newly generated skill, product files unchanged.

Failed attempts retained: `run-9ac51e49823a4ea6838d32f34858e2a8` (Windows extended path string mismatch; repaired with samefile), `run-d229de05474d4c8da788a99de43572f3` (search's transient score incorrectly compared as persisted field; now removed only that field before full record equality). Both failed runs cleaned owned state. These were verifier errors, not weakened product requirements.

Claude supplied a desktop workflow draft with tools disabled. Its screenshot-only evidence rule and guessed host-configuration actions were rejected; final instructions require public state readback and exclude host configuration changes.
