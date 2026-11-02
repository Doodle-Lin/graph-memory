# Avalonia 桌面端

Windows x64 可运行开发版：双击 `artifacts/desktop/GraphMemory.Desktop.exe`。移动时复制整个目录，已包含运行时、Rust 服务及 MCP。

## 使用

主流程是 **Agent 经 MCP 检索、写入和纠正记忆**。桌面负责接入状态、最近记忆与人工纠正，不要求每轮复制上下文。

1. 在“接入 Agent”中复制对应宿主配置，合并到该宿主的 MCP 配置并重启宿主；不要覆盖已有其他服务。
2. 将 [Agent 使用约定](AGENT_INSTRUCTIONS.md) 加入宿主指令。MCP initialize 也提供 instructions，但宿主是否采纳由宿主决定。
3. Agent 开始任务时调用 memory_context / memory_search；重要经验和决策完成后调用 memory_write，纠正时调用 memory_update。
4. 桌面每 10 秒更新 MCP 路由调用统计；“刷新记忆”读取最新列表。首次无调用时明确显示未观察到调用。统计不证明宿主已经正确配置，也不证明 Agent 使用了返回内容。

- 个人经验 → 记一条 → 保存：无需项目，可记录踩坑、方法、来源和适用条件。
- 新建项目后记录进展、决策、阻塞、下一步。项目视图同时显示个人经验，不混入其他项目事实。
- 手动备用 → 准备复制背景：只供不支持 MCP 的工具，复制前读取最新记忆。
- 选择记录可纠正、查看版本历史、恢复上一版；并发版本冲突时保留当前草稿。
- 项目记录可以提炼为个人经验草稿，保留原案例引用，核对适用条件后保存。
- Agent 接入可复制通用 JSON 或 Codex TOML 配置及使用说明。应用不会改写宿主配置，宿主实际调用后才会积累记忆。

默认目录 `%LOCALAPPDATA%/GraphMemory/workspace-v1`，新模型保存在 `work-memory.db`。可用 `--data-dir <目录>` 指定独立目录。窗口关闭后服务继续运行；MCP 通过发现文件握手，按需启动服务。旧图谱未迁移或改写。

## 构建验证
需要 Rust Windows 工具链与 .NET 10 SDK：
```powershell
./scripts/build-desktop.ps1
python tests/test_headless_service.py
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --lib
./artifacts/desktop/GraphMemory.Desktop.exe --smoke-test --data-dir "$env:TEMP/gm-desktop-smoke-unique"
```
烟雾测试会写入示例，务必指定新的临时目录。集成测试自动隔离数据。

## 边界
这是可运行开发版，尚非完整发布版。检索为关键词匹配，没有语义向量；无主题时经验推荐为简单文本重合，中文长句应输入具体关键词。依据目前统一为陈述，不自动标记为验证事实。旧数据迁移、软删除/恢复、导出、完整证据生命周期、模型管理、完整鉴权尚未完成。服务监听本机并拒绝浏览器 Origin 请求，本机其他进程仍属于信任边界。

原生窗口已通过可访问性结构和键盘操作验证保存及续接。直接捕获接口超时后，已通过 Alt+Print 与画图保存实际窗口截图，检查首页、编辑页和续接页，并修正展开区边框与黑色焦点框。多 DPI 和最小窗口尺寸仍待验证。

调用统计只保留每种工具的累计次数、成功次数、最近调用时间和结果，不保存查询正文。失败写入不算成功，统计写入失败也不把已成功的记忆写入伪装为失败。此本机接口不是宿主身份认证，任何可信本机进程均可调用；不要把调用计数视为安全审计。
