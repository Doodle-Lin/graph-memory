# Graph Memory V1: task recovery first

## Product contract
Graph Memory succeeds only when a new agent session can resume a project using *verified* past decisions, status, blockers, and next actions. A retrieved node is not evidence of successful recovery.

## Primary journey
1. Agent finishes a development session and captures a decision, evidence, unresolved task and source.
2. A second agent starts without the prior transcript.
3. It retrieves the project's latest verified context, identifies what remains uncertain, and continues without repeating failed attempts.
4. User can inspect and correct the memory used.

## Current risks (inspect, don't assume fixed)
- SKILL.md says retrieve before every answer and write after valuable answers; that is a prompt instruction, not proof of MCP invocation.
- regression.py snapshots rankings against a mutable local database; its placeholder queries cannot establish task recovery quality.
- Similarity-based merging may erase contradictory but useful history.
- Rust desktop engine and Python engine need separate regression coverage.
- Exact identifiers must survive LLM extraction and remain retrievable.

## P0 acceptance gates
- Every test case declares source facts, expected memory IDs and forbidden claims.
- Evaluate retrieval recall@5 and exact-identifier preservation separately from end-to-end answer correctness.
- Record whether MCP was called, which IDs were returned, which were cited/used, and whether the answer preserved unresolved blockers.
- Reject an evaluation if no baseline, no source evidence, or a different dataset is used.
- Never put real credentials, private hostnames, or tokens in fixtures.

## Implementation order
1. Land deterministic offline evaluation fixture and validator.
2. Add structured MCP retrieval/write event tracing with secret redaction and opt-in retention.
3. Add project context assembly and explicit verified/uncertain status.
4. Add conflict-aware updates and reversible consolidation.
5. Build a project-first desktop UI after backend gates pass.

## Non-goals for this PR
No rewrite of retrieval ranking, schema migration, or automatic merge/forget changes. These need measured baselines and migration safety.
