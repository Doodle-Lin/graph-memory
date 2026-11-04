# P1 数据生命周期契约：软撤回与恢复（2026-11-04）

基线：`13b9525`（P0 批次已提交，未推送）。本文件是 L2-04 的唯一契约。目标：错误结论可撤回、可恢复，默认检索/续接不再采用被撤回内容，且撤回本身可追溯（版本 + 原因），不物理删除。

## 为什么现在做

- 当前只有"纠正"：内容改错可以改回，但**结论被证伪后无法标记失效**，Agent 仍会在 search/context 中读到并采用。
- 契约依据：LONG_TERM_PLAN L2-04"默认 search/context 排除撤回记录；明确含撤回筛选可读历史；恢复产生新版本；并发 expected_version 生效；不做物理删除"。

## 数据模型（只增字段，兼容旧库）

`Memory` JSON 增加两个可选字段，均由 `serde(default)` 处理，旧 body 缺失即为未撤回：

| 字段 | 类型 | 语义 |
|---|---|---|
| `retracted_at` | string(RFC3339) 或 null | 非空表示当前版本处于**已撤回**状态；时间即撤回生效时间 |
| `retract_reason` | string 或 null | 撤回原因；恢复后清空 |

- 不新增 `status` 字段（状态由 `retracted_at` 推导，避免两处真相）。
- 撤回/恢复**都是新版本**（version+1），进入既有 `work_revisions` 历史；`undo` 语义不变。
- **修订可变字段**：`retracted_at`/`retract_reason` 允许跨版本变化（与 `evidence_type` 同理）。**不可变字段仍为**：`memory_scope`、`project_id`、`origin_memory_id`、`created_at`、version 单调性。同步修订 `BACKUP_FORMAT_V1.md`。
- 不做物理删除：没有任何端点或 CLI 会删除记录行。

## API（HTTP v1；owner: rust-core）

- `POST /api/v1/memories/retract`：`{id, expected_version, reason}` → `{"memory":{...}}`，新版本 `retracted_at=now`、`retract_reason=reason`（reason 可空字符串）。
- `POST /api/v1/memories/restore`：`{id, expected_version, reason}` → `{"memory":{...}}`，新版本 `retracted_at=null`、`retract_reason=null`。
- 两者共用既有版本冲突规则：`expected_version` 不等于当前版本 → 409，不写任何版本；记录不存在 → 404；`reason` 缺失按空字符串。
- 对已撤回记录再次 retract、对未撤回记录 restore：返回 400，明确错误信息（`memory is already retracted` / `memory is not retracted`），不产生空版本。

### 检索与续接

- `POST /api/v1/search`：**默认排除已撤回记录**（`total` 也只计未撤回）；新增入参 `include_retracted`（bool，默认 false），为 true 时返回并标记 `retracted_at`。响应新增 `"retracted_excluded": <int>`（被默认排除的条数，供调用方判断缺口）。
- `POST /api/v1/context`：**始终排除已撤回**（续接是"当前状态"，不提供开关），响应新增 `"retracted_excluded": <int>`；`text` 在截断提示后增加一行 `已排除 N 条撤回记录`（N>0 时）。
- 历史与单条读取：`/api/v1/memories/history` 保留全部版本（含撤回版本），恢复后历史可见撤回痕迹。

## MCP（保持五工具；owner: rust-core）

不新增第六个工具，避免破坏已验收的五工具契约：

- `memory_update` schema 增加可选 `retract`（bool，默认 false）与 `reason`（string）；`retract=true` 时调用 retract 端点（仅 `id`+`expected_version` 必填，`content` 改为可选），否则维持现有纠正语义。
- `memory_search` schema 增加可选 `include_retracted`（bool）。
- 旧的七工具分支行为不变。

## 备份/恢复（owner: data-restore）

- `backup_format`：撤回字段随 body 原样序列化即可（无需新表）；`validate` 将 `retracted_at`/`retract_reason` 从不可变集合排除（当前它们本就不在校验里，需确认不会因新字段被"未知字段"拒绝——**必须新增测试**：带撤回版本的库 export→verify→restore 往返，恢复库中撤回状态与版本历史保留）。
- 旧包（A3 之前、无这些字段）仍可解码；`format_version` 不变。
- `restore` 行为不变（只写空目录）。

## 桌面（owner: desktop-ux）

- 列表默认不显示已撤回记录；筛选或"显示已撤回"开关可查看，已撤回项带明确标签（如"已撤回"）与撤回原因摘要。
- 编辑器对未撤回记录提供"撤回这条"，对已撤回记录提供"恢复这条"；两者都需要原因输入（可空），并走既有版本冲突流程（409 时保留草稿并提示重读）。
- 撤回/恢复后刷新列表与续接区；不得把撤回实现成删除。
- 硬约束同 P0：不得重命名既有控件/方法；`tests/Desktop.Editor.Tests` 73 项、`tests/Desktop.Setup.Tests` 102 项必须继续通过。

## 验收（owner: verifier2，只读产品代码）

黑盒断言（追加到 `scripts/verify-usability.ps1`，新 section `P1-*`）：

1. retract 后 search 默认 0 命中、`include_retracted=true` 命中且带 `retracted_at`；`retracted_excluded` 计数正确。
2. context 不含撤回记录，`text` 出现"已排除 N 条撤回记录"，`retracted_excluded` 正确。
3. restore 后重新可检索，version+1，`retracted_at=null`，历史含撤回版本。
4. 版本冲突 409 不产生新版本；重复 retract/restore 400；不存在 404。
5. 撤回不物理删除：`history` 仍返回全部版本；直接用 sqlite 只读核对记录行仍在。
6. 备份往返：含撤回版本的库 export→verify→restore，恢复库四表计数一致且撤回状态保留（sqlite 只读核对）。
7. 旧库兼容：不带撤回字段的旧 body 仍能读、能搜、能被 MCP 写入新版本。
8. 回归：Rust / Setup 102 / Editor 73 / Python 两套件 / smoke 全绿。

## 明确不做

物理删除、批量撤回、撤回审批流、跨库合并、桌面数据管理入口（导出/恢复 UI，属 L2-05）、云同步。

## 环境

Rust `cargo` 在 PATH；.NET 10 SDK 在 `%USERPROFILE%\.dotnet`（不在 PATH，需设 `DOTNET_ROOT` 与 `PATH`）；服务二进制 `src-tauri/target/debug/graph-memory-service.exe`（改动后 `cargo build --no-default-features`）；PowerShell 5.1，写文件显式 UTF-8。
