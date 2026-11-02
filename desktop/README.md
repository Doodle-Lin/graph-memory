# Avalonia 桌面端

Windows x64 可运行开发版：双击 `artifacts/desktop/GraphMemory.Desktop.exe`。移动时复制整个目录，已包含运行时、Rust 服务及 MCP。

## 使用

主流程是 **Agent 经 MCP 检索、写入和纠正记忆**。桌面负责接入状态、最近记忆与人工纠正，不要求每轮复制上下文。

1. 打开“接入 Agent”，选择 Claude Code、Codex、Hermes 命令行或 Hermes 桌面版。核对用户配置位置，点击“检测配置”及“一键接入”，再在宿主中重载 MCP 或重启宿主。初次打开默认宿主会自动检测。
2. 将 [Agent 使用约定](AGENT_INSTRUCTIONS.md) 加入宿主指令。MCP initialize 也提供 instructions，但宿主是否采纳由宿主决定。
3. Agent 开始任务时调用 memory_context / memory_search；重要经验和决策完成后调用 memory_write，纠正时调用 memory_update。
4. 桌面每 10 秒更新 MCP 路由调用统计；“刷新记忆”读取最新列表。首次无调用时明确显示未观察到调用。统计不证明宿主已经正确配置，也不证明 Agent 使用了返回内容。

- 个人经验 → 记一条 → 保存：无需项目，可记录踩坑、方法、来源和适用条件。
- 新建项目后记录进展、决策、阻塞、下一步。项目视图同时显示个人经验，不混入其他项目事实。
- 手动备用 → 准备复制背景：只供不支持 MCP 的工具，复制前读取最新记忆。
- Agent写入的自定义记录类型会保留并显示为自定义选项，编辑正文和跨项目恢复草稿不会把它悄悄改成笔记；只有主动切换类型才会更改。
- 选择记录可纠正、查看版本历史、恢复上一版；并发版本冲突时保留当前草稿，可加载历史核对，再明确选择“放弃草稿并加载最新”。恢复上一版、提炼经验前须先保存当前编辑；提炼不会覆盖个人经验中已有的草稿。
- 项目记录可以提炼为个人经验草稿，保留原案例引用，核对适用条件后保存。
- 一键接入会合并用户级配置，备份原文件，迁移单个可识别且无工具权限策略的旧 Graph Memory 入口，保留其他设置和其他 MCP 服务；现有 v1 工具限制保持不变。旧入口包含工具限制/未知权限策略，或检测到多个记忆入口时，拒绝自动迁移，需手动核对，避免扩大权限。Hermes 的非受管 YAML 节点保留数值、布尔、空值和字符串语义；配置格式和注释可能被序列化器规范化；原始字节保存在配置旁的 `.graph-memory-*.bak` 中。
- “撤销上次接入”恢复原始配置；若原文件不存在，则撤销创建。若配置之后已被其他程序修改，拒绝覆盖，需手动比较备份。配置写入采用原子替换并在写入前进行乐观冲突检查。
- “测试本地组件”实际启动 MCP 并核对五个工具，不写入测试记忆，也不把测试结果当成所选宿主已连接的证据。
- 高级选项仍可复制 JSON / TOML 和使用说明；不会自动改写宿主全局指令或重启运行中的 Agent。项目级配置、权限审批及管理策略仍由宿主处理。

Claude Code 默认读取 `~/.claude.json`（支持 `CLAUDE_CONFIG_DIR`），Codex 使用 `CODEX_HOME/config.toml` 或 `~/.codex/config.toml`，Hermes CLI 使用 `HERMES_HOME/config.yaml` 或 `~/.hermes/config.yaml`。Hermes CN 桌面版单列本机常用路径；其他发行版、Profile 或远程宿主请自行选择正确路径或使用手动配置。含 YAML 锚点/别名、多文档或无法识别同名入口的配置会拒绝自动合并。

格式依据：[Claude Code MCP](https://code.claude.com/docs/en/mcp)、[Codex MCP 配置示例](https://developers.openai.com/learn/docs-mcp)、[Hermes MCP](https://hermes-agent.nousresearch.com/docs/user-guide/features/mcp)。

默认目录 `%LOCALAPPDATA%/GraphMemory/workspace-v1`，新模型保存在 `work-memory.db`。可用 `--data-dir <目录>` 指定独立目录。窗口关闭后服务继续运行；MCP 通过发现文件握手，按需启动服务。旧图谱未迁移或改写。

## 构建验证
需要 Rust Windows 工具链与 .NET 10 SDK：
```powershell
./scripts/build-desktop.ps1
python tests/test_headless_service.py
dotnet run --project tests/Desktop.Setup.Tests
dotnet run --project tests/Desktop.Editor.Tests
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --lib
./artifacts/desktop/GraphMemory.Desktop.exe --smoke-test --data-dir "$env:TEMP/gm-desktop-smoke-unique"
```
烟雾测试会写入示例，必须显式指定新的临时目录；缺少目录或使用 `--service-url` 时拒绝执行，避免默认写入日常工作库。集成测试自动隔离数据。

## 边界
这是可运行开发版，尚非完整发布版。检索为关键词匹配，没有语义向量；无主题时经验推荐为简单文本重合，中文长句应输入具体关键词。依据目前统一为陈述，不自动标记为验证事实。旧数据迁移、软删除/恢复、导出、完整证据生命周期、模型管理、完整鉴权尚未完成。服务监听本机并拒绝浏览器 Origin 请求，本机其他进程仍属于信任边界。

原生窗口已通过可访问性结构和键盘操作验证保存及续接。直接捕获接口超时后，已通过 Alt+Print 与画图保存实际窗口截图，检查首页、编辑页和续接页，并修正展开区边框与黑色焦点框。多 DPI 和最小窗口尺寸仍待验证。

调用统计只保留每种工具的累计次数、成功次数、最近调用时间和结果，不保存查询正文。失败写入不算成功，统计写入失败也不把已成功的记忆写入伪装为失败。此本机接口不是宿主身份认证，任何可信本机进程均可调用；不要把调用计数视为安全审计。

## 周报、月报与项目复盘

使用 `memory_search` 获取报告素材，`memory_context` 仍用于任务续接，不能代替完整清单。

- `scope=current`（默认）：指定项目加个人经验；省略项目时仅个人经验。`scope=project` 要求 `project_id`，严格只查该项目。`scope=personal` 只查个人；`scope=all` 显式跨所有项目与个人，后二者不可同时指定项目。
- `date_field=created_at` 筛选新沉淀的知识；默认 `updated_at` 筛选最近维护过的记录。`from` 包含下界、`to` 不包含上界，两者必须为带时区的 RFC3339。无边界时不限时间。
- `kind` 精确匹配类型；`query` 为可选关键词（空格分词且全部匹配标题、正文或适用条件）。省略关键词可枚举范围内记录。
- 推荐 `limit=100`（1–500），按返回的 `next_offset` 继续，直到 null。`total` 为筛选总数；`filters` 回显范围。为兼容已有桌面，省略 limit 仍返回所有匹配。按所选时间倒序、ID 稳定排序；分页期间写入或更新可能造成偏移，应在稳定数据上汇总并按 ID 去重。

例如上海时区 2026 年 10 月跨项目新知识：

```json
{"scope":"all","date_field":"created_at","from":"2026-10-01T00:00:00+08:00","to":"2026-11-01T00:00:00+08:00","limit":100,"offset":0}
```

项目复盘改为 `scope=project` 并传入 `memory_projects` 返回的真实项目 ID；只复盘决策时再加 `kind=decision`。Agent 可据素材组织成果、决策、问题、经验和后续行动，并标注记录 ID、版本、来源及适用条件；没有依据的进展不能补写。

**时间含义：**创建/更新时间不是事件发生时间，历史导入的创建日期可能是导入日。返回的是最新版本，不是历史月末快照；某记录在期间内更新、之后再次更新时，不会出现在该期间的 updated_at 筛选中。当前接口不提供逐次变更日志或 occurred_at 筛选，不能用它证明某个时期全部发生过的工作。

编辑回归使用 [Avalonia Headless](https://docs.avaloniaui.net/docs/testing/setting-up-the-headless-platform) 加载真实XAML控件并连接隔离HTTP服务，覆盖打开、保存、跨项目草稿和版本恢复；它不替代原生截图、多DPI或键盘可达性检查。运行前需构建Rust服务；可给Desktop.Editor.Tests传入服务可执行文件路径。
