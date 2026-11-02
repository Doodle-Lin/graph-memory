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
| `memory_update` | 按 id 与 expected_version 纠正已有记忆 |

日常踩坑和跨项目方法用 `memory_scope=personal`（不填 project_id）；项目进展、决策、阻塞用 `memory_scope=project` 并填写真实 project_id。

## 桌面端与 Agent 接入

1. 打开“接入 Agent”，选择 Claude Code、Codex 或 Hermes，点击“检测配置”与“一键接入”，再在宿主中重载 MCP 或重启宿主。
2. 将 [Agent 使用约定](desktop/AGENT_INSTRUCTIONS.md) 加入宿主指令；桌面端不会自动改写宿主全局指令。
3. 建议验收：在隔离数据目录启动 MCP，创建一条个人经验、检索同一关键词、按版本纠正后再检索，桌面端应能看到对应调用。

默认工作库为 `%LOCALAPPDATA%/GraphMemory/workspace-v1/work-memory.db`；`--data-dir <目录>` 可指定独立目录。新旧数据不自动互通。

## 文档

- [桌面端使用说明](desktop/README.md)
- [可安装的 Agent 技能](SKILL.md)
- [Agent 使用约定](desktop/AGENT_INSTRUCTIONS.md)
- [无界面服务](docs/HEADLESS_SERVICE.md)
- [长期规划](docs/design/LONG_TERM_PLAN.md)
- [执行账本](docs/design/EXECUTION_LEDGER.md)
- [旧版入口说明](docs/legacy/README.md)

## 边界

这是可运行开发版，尚未完整发布：检索为关键词匹配而非语义向量；依据目前统一为陈述，不自动标记为已验证；旧数据迁移、软删除/恢复、导出、完整证据生命周期与完整鉴权尚未完成。服务只监听本机并拒绝浏览器 Origin 请求，但本机其他进程仍属信任边界，调用统计不构成安全审计。工具失败时如实说明；成功读取也不等于本次任务已验证成功。

## License

MIT

宿主实际验证范围与未完成项见[兼容验收矩阵](docs/HOST_COMPATIBILITY.md)；配置成功不等于Agent已经调用或采用记忆。
