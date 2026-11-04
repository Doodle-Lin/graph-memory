# Graph Memory

本仓库以 **Avalonia Windows x64 可运行开发版** 为统一入口，通过 **MCP 自动记忆** 沉淀跨会话经验与项目记录：Agent 经 MCP 检索、写入和纠正记忆，桌面端展示接入状态、最近记忆与人工纠正，不要求用户逐轮复制上下文。

## 快速开始

从仓库根目录运行以下命令（已有源码 checkout）。

构建前提：

- Rust Windows MSVC 工具链（含 C++ 构建工具）
- .NET 10 SDK

```powershell
./scripts/build-desktop.ps1
./artifacts/desktop/GraphMemory.Desktop.exe
```

分发或移动时**复制整个输出目录**（`artifacts/desktop`）：其中已包含 .NET 运行时、Rust 服务与 MCP，只复制 exe 无法运行。

构建脚本会覆盖 `artifacts/desktop`。更新已有安装前，先在桌面端正常保存并退出，再释放该目录下程序的占用；关闭窗口后服务仍在后台运行，MCP 宿主也可能占用文件；不要强制终止含未保存工作的进程。

## 五个 MCP 工具（v1）

| 工具 | 用途 |
| --- | --- |
| `memory_projects` | 列出项目及真实项目 ID，避免猜测项目 |
| `memory_context` | 任务开始或跨会话续接时获取上下文 |
| `memory_search` | 关键词检索；支持 scope、时间区间、kind 与分页 |
| `memory_write` | 写入个人经验或项目记录 |
| `memory_update` | 按 id 与 expected_version 纠正已有记忆；`retract=true` 撤回、`restore=true` 恢复（都产生新版本，不删除记录） |

日常踩坑和跨项目方法用 `memory_scope=personal`（不填 project_id）；项目进展、决策、阻塞用 `memory_scope=project` 并填写真实 project_id。阻塞、下一步、决策请如实标注 `kind`：`memory_context` 会把它们归入"未解决阻塞 / 下一步 / 最近决策"优先展示。依据用可选 `evidence_type` 标注（`user_or_agent_statement` 陈述默认、`user_confirmed`、`tool_result`、`inference`），缺少依据就保持陈述。`memory_search` 支持中文自然问句（按词条 + 字符 bigram 匹配）并按相关度排序，每条返回 `score`；默认隐藏已撤回记录（响应另有 `retracted_excluded` 计数），复核历史时才传 `include_retracted=true`。结论被证伪时用 `memory_update` 的 `retract=true` 撤回而不是改写正文：撤回保留全部版本、默认检索不再返回，需要时可 `restore=true` 恢复，并应填写真实原因。

桌面端打开即自动加载最近记忆，不再需要先点"刷新记忆"；记录与编辑页显示依据标签，空库给出接入/记录引导；列表默认隐藏已撤回记录，勾选"显示已撤回"可查看原因摘要，编辑器提供"撤回这条 / 恢复这条"。

## 桌面端与 Agent 接入

1. 打开“接入 Agent”，选择 Claude Code、Codex 或 Hermes，点击“检测配置”与“一键接入”，再在宿主中重载 MCP 或重启宿主。
2. 将 [Agent 使用约定](desktop/AGENT_INSTRUCTIONS.md) 加入宿主指令；桌面端不会自动改写宿主全局指令。
3. 建议验收：在隔离数据目录启动 MCP，创建一条个人经验、检索同一关键词、按版本纠正后再检索，桌面端应能看到对应调用。

默认工作库为 `%LOCALAPPDATA%/GraphMemory/workspace-v1/work-memory.db`；`--data-dir <目录>` 可指定独立目录。新旧数据不自动互通。

## 备份、校验与恢复

三种入口，语义相同（都只通过 `graph-memory-backup`，桌面不直接改库）：

1. **桌面**【数据备份与恢复】面板：显示当前工作库目录与计数；导出备份、校验备份、恢复到空目录（`apply` 前二次确认，明示当前工作库不会被写入/覆盖/合并/删除）；目标文件已存在时界面直接拒绝，绝不覆盖。
2. **命令行**：`graph-memory-backup export --data-dir DIR --output FILE.gmbackup`、`verify FILE.gmbackup`、`restore --input FILE.gmbackup --data-dir EMPTY_DIR [--dry-run]`。详见[备份文档](docs/BACKUP.md)。
3. **发布包**：`scripts/package-release.ps1` 产出 `artifacts/release/graph-memory-<版本>-win-x64/` 与同名 ZIP（含 4 个可执行文件、`VERSION.txt`、`SHA256SUMS.txt`）；`scripts/verify-release.ps1` 校验版本一致性、哈希、ZIP 解压后 smoke 与真实 export→verify→restore 往返。当前版本 **0.3.0**，解压即用、无需安装。

恢复只写入**空的** `--data-dir`，不会覆盖现有工作库；需要把备份并回现有库时请人工核对记录 ID 与版本（合并式恢复仍未实现）。

## 文档

- [桌面端使用说明](desktop/README.md)
- [可安装的 Agent 技能](SKILL.md)
- [Agent 使用约定](desktop/AGENT_INSTRUCTIONS.md)
- [无界面服务](docs/HEADLESS_SERVICE.md)
- [备份、校验与恢复](docs/BACKUP.md)
- [中文检索基线](docs/design/RECALL_BASELINE.md)
- [长期规划](docs/design/LONG_TERM_PLAN.md)
- [执行账本](docs/design/EXECUTION_LEDGER.md)
- [旧版入口说明](docs/legacy/README.md)

## 边界

这是可运行开发版，尚非完整发布版：检索为词条 + 中文 bigram 匹配而非语义向量——[中文检索基线](docs/design/RECALL_BASELINE.md) 给出可复现实测值（Recall@5 0.796、跨项目污染 0、撤回泄漏 0；**换个说法而不是照抄措辞时 Recall@5 只有 0.333**，这是当前最真实的短板）；依据可标注但仍需人工判断，不自动标记为已验证；旧数据迁移、批量撤回与撤回审批、合并式恢复、自动更新/安装程序与代码签名、完整证据生命周期与完整鉴权尚未完成。工作库已支持一致性备份、校验与**恢复到空目录**（桌面面板 + CLI，见[备份文档](docs/BACKUP.md)），恢复不是合并、也不会覆盖现有工作库，且**只接受完全为空的目标目录**。记录支持软撤回（`retract`/`restore`，产生新版本、不物理删除）。**未保存的桌面草稿现在会持久化**（进程被杀也能恢复；草稿存在工作库之外、不进记忆库、不参与检索与导出）。服务只监听本机并拒绝浏览器 Origin 请求，但有 32 KiB 头 / 1 MiB 正文 / 15 秒 / 64 并发的资源上限；本机其他进程仍属信任边界，调用统计不构成安全审计。工具失败时如实说明；成功读取也不等于本次任务已验证成功。

## License

MIT

宿主实际验证范围与未完成项见[兼容验收矩阵](docs/HOST_COMPATIBILITY.md)；配置成功不等于Agent已经调用或采用记忆。
