# Backup and restored continuity

User entry: `graph-memory-backup export`, `verify`, `restore --dry-run`, `restore`. Source: `src-tauri/src/bin/graph-memory-backup.rs`, `src-tauri/src/backup_io.rs`. Requirements: same binary directory as service, new backup filename, existing completely empty restore directory. A populated destination is not authorized for this feature.

Execute SKILL.md's `verify.py run`. After the MCP seed has reached version2 it performs these real public commands:

```text
graph-memory-backup export --data-dir SOURCE --output EVIDENCE/workspace.gmbackup
graph-memory-backup verify EVIDENCE/workspace.gmbackup
graph-memory-backup restore --input EVIDENCE/workspace.gmbackup --data-dir EMPTY --dry-run
graph-memory-backup restore --input EVIDENCE/workspace.gmbackup --data-dir EMPTY
```

Each exit must be0. After dry-run the target directory must still be completely empty (not just lacking work-memory.db). After apply, launch an independent service on the restored directory, doctor it, and search via fresh MCP. Every stored field must equal the version2 source record; ignore only search's added ranking score. Replay the original creation request: response must be original version1 and subsequent search must still have exactly one record. Source doctor must still succeed.

Evidence: backup file, CLI arguments/stdout/exit codes in actions.json, restored-doctor.json and final cleanup verdict. This verifies preserved data and creation idempotency, not just JSON parse success. Existing tests/test_backup_cli.py and Rust backup_io tests cover concurrent export, failure paths and no-overwrite; this mapped feature does not replace them. No merge, real user recovery, package signing or desktop click coverage is claimed.
