# MCP memory continuity

Entry: an Agent uses `memory_write`, `memory_search`, `memory_update`, `memory_context`. Sources: `src-tauri/src/bin/mcp_server.rs` tool dispatch, `src-tauri/src/memory_store.rs` create/update/context, `src-tauri/src/service.rs` discovery and ownership.

Run from repo root: `python -X utf8 .agents/skills/verify-graph-memory/scripts/verify.py run` after SKILL build/doctor prerequisites. No real host account required. Seed is one unique event ID, personal scope, Chinese content containing VERIFY, source `isolated verifier`.

The real driver launches a fresh MCP executable per call, sending JSON-RPC initialize then tools/call on stdin. It writes the seed twice with identical event_id/arguments (same ID expected), searches in another process (exactly one matching ID), updates with expected_version=1 (version2), then retrieves context in another process (corrected content included). MCP is explicitly routed to the owned service; no fallback to old tools or default store.

Evidence: actions.json records raw inputs/stdout/exit codes; source-doctor.json binds PID/data directory; build.json identifies executable bytes. Require no JSON-RPC error/isError, correct ID/version/count and corrected text, not merely exit0.

Excluded from this personal continuity feature: autonomous model use, host configuration migration, project/date filters, retraction and contention. See the separate [work-log feature](work-logs.md) for project/date coverage and [model adoption](model-adoption.md) for real host behavior. Existing tests/test_headless_service.py and the full contract cover additional protocol branches; a personal-flow pass does not prove them. Cleanup and failed-attempt retention follow SKILL.md.
