# 执行与验收台账

总计划：[LONG_TERM_PLAN.md](LONG_TERM_PLAN.md)。状态由 Codex 核验后更新。Claude 自报完成仅表示交付，不能等同验收通过。

| 任务 | 状态 | 实现者 | 验收者 | 输入基线 | 证据与下一步 |
|---|---|---|---|---|---|
| L1-01 接入迁移保护 | 已推送 | 本地 Claude CLI / sonnet | Codex | d69133c | Claude 交付后 Codex 审查；68 项检查通过；明确安全拒绝提示已补齐 |
| L1-02 YAML 类型保真 | 验收通过 | Claude 初稿 / Codex 修复 | Codex | 待 L1-01 集成 | 102 项配置检查通过；原节点保真；受管 env/type 校正；详见本轮验收 |
| L1-03 未知类型兼容 | 验收通过 | 本地 Claude CLI / sonnet | Codex | 7986925 | 41项真实控件+隔离HTTP检查通过，基线同用例失败 |
| L1-04 草稿退出保护 | 验收通过 | 本地 Claude CLI，Codex 补充验证 | Codex | c3e3d47 | 73项编辑/关闭检查，原生取消/保存退出与截图通过 |
| L1-05 新版入口统一 | 验收通过 | Claude 文案初稿 / Codex 集成 | Codex | eedb26d | 新版首页/技能/架构入口，旧版归档；18 个本地链接及五工具契约核对通过 |
| L1-06 Windows 契约 CI | 验收通过 | Claude 初稿 / Codex 修正验证 | Codex | 44130cd | 远端Windows全流程通过；main必需检查规则26353094已生效 |
| L1-07 宿主矩阵 | 验收通过（分层边界） | Codex 主验收，Claude 辅助文档 | Codex | 473476e | Claude双会话闭环、Codex只读、Hermes发现；其余明确未测 |
| L2-01 导出格式 | 验收通过 | Claude序列化 / Codex契约与关系校验 | Codex | ebadaa3 | 6项格式测试；完整19项Rust通过，远端41418751899成功 |
| L2-02 一致性备份 | 已实现 | Claude / Codex | Codex | 368370c | 后续P0-P4包含恢复与往返验证；见当前治理记录 |
| L2-03 副本恢复 | 部分完成 | P0/P2/P3 | 历史独立验收 | P4 | 空目录恢复与dry-run已实现；合并与冲突预览未实现 |
| L2-04/05/06 | 已交付对应能力 | P1/P2/P3 | 历史独立验收 | P4 | 软撤回恢复、桌面数据入口、草稿持久化；不代表整个L2全部完成 |
| L3 时间与证据 | 部分完成 | P0 | 历史验收 | P4 | 依据标签已实现；事件时间、期间变更查询与证据完整生命周期待做 |
| L4 检索 | 字面评测与改进已交付 | P3/P4 | 历史验收+本轮复测 | 8fb9689 | 语义P5单独保留，未验收；索引与规模优化待做 |
| L5 发布运维 | 部分完成 | P2/P3/P4 | 历史发布物验收 | P4 | ZIP打包/校验、协议边界已实现；签名、安装、升级回滚等待做 |

## 当前交付状态（2026-11-16）

当前状态以[治理与交付记录](GOVERNANCE_20261116.md)为准。以下按日期追加的内容是当时的历史记录；其中“未实现”“未推送”“下一项”等描述不自动代表当前状态。P0-P4是对长期L任务的分批交付，不是一套替代路线图。

## 2026-11-02 首批启动

- 用户授权：制定详细长线计划并落盘，逐步委托 Claude，由 Codex 验收。
- 工作区基线干净；未改变真实宿主配置或记忆内容。
- 委派用受限 Read/Edit/Write 工具、空 MCP 配置和隔离文件副本；测试和集成由 Codex 执行。
- 本台账不是后台自动调度器；本轮真实执行情况将在下方补充。

## L1-01 验收

- Claude CLI 正常退出，耗时约166秒；仅交付指定两个文件。原始交付67项配置检查通过。
- Codex 对比 diff：确认先验证再移除别名，单旧入口克隆保留属性，多入口拒绝，旧工具权限不做推测转换。
- 验收补充：消除测试可空警告；用仅容纳应用常量文案的专用异常保留明确拒绝原因，解析错误仍屏蔽原配置片段；新增提示检查。
- 独立命令：dotnet run --project tests/Desktop.Setup.Tests，68项通过。基线失败证据见 OVERALL_AUDIT.md 的隔离旧限制丢失复现。
- 没有修改真实宿主配置；没有声称宿主端已验证。L1-02在该集成结果的隔离副本继续实施。

## L1-02 首次验收退回

- Claude首次正常退出，约289秒，提交节点保真实现与类型测试；尚未合入。
- Codex编译发现 SequenceStyle 命名空间错误，测试还依赖默认 object 反序列化推断类型；首次验收不通过。
- 一次针对性返修：正确命名空间、显式标量类型推断、受管 env 字符串类型、enabled 布尔类型与仅类型改变时的 Changed 检测；禁止修改无关格式行为。
- L1-01已推送，提交310e61e。新增测试在d69133c旧源的隔离副本失败（旧字段丢失），证明回归用例能捕获旧行为。

## L1-02 最终验收

- 一次返修委派超过300秒，没有可用修正版；父shell超时后子CLI仍占用管道，Codex核对PID/父PID/本次参数后停止该子进程。未停止用户其他Claude任务。
- Codex接手：修正SequenceStyle命名空间，启用测试反序列化的未加引号类型推断并检查数值类型；受管env显式写字符串，enabled写布尔，类型变化本身可触发Changed。移除不真实的“节点字节完全不变”注释，仅备份恢复承诺原字节。
- 补充保守策略：旧入口tools空列表也可能是空白名单，拒绝自动映射。
- 独立回归：102项配置检查通过；旧310e61e源运行同一组新增测试会明确失败于legacy YAML numeric timeout corrupted。桌面构建0警告0错误。
- 保留原节点的timeout数字、布尔、null、带引号字符串、嵌套对象/数组和额外env；测试覆盖备份恢复、v1限制、重复接入及受管类型规范化。未宣称实际Hermes宿主全链路已验证；显式自定义YAML标签还未做单独宿主兼容测试。
- 真实Agent配置和知识库没有被本批合成测试改动；运行中的桌面程序未被替换。自包含候选包输出artifacts/desktop-l1，供后续原生验收与安装升级任务使用。

## 下一批

按LONG_TERM_PLAN.md执行L1-03 → L1-04：先使未知kind打开/保存不被误改，再处理当前和跨项目草稿退出保护。Claude运行器下一次必须以整个子进程树为超时边界，避免shell结束后子CLI悬挂；不把该启动器缺陷伪装成模型完成。

## L1-03 验收（2026-11-02）

- 本地Claude在隔离副本交付，97秒正常退出，只修改MainWindow.axaml及其代码；未使用真实MCP、配置或记忆库。
- 实现以原始kind字符串作为选项值和草稿值，保留五个中文内置标签，未知值单独显示且不累积。HasDraft、创建签名、写入/纠正、草稿恢复使用同一值。
- Codex独立建立Desktop.Editor.Tests：加载实际App/XAML/MainWindow控件，使用Avalonia.Headless运行事件处理逻辑，连接新临时目录中的真实Rust HTTP服务；没有复制一套假的保存逻辑。
- 修改前第一条用例失败：opening kind creates false draft: lesson。修改后41项通过：lesson、中文、空类型、decision；未编辑关闭、内容保存、跨项目草稿恢复、显式改note、恢复前版、新记录默认note、选项不累积。
- 本轮不是原生像素/多DPI验收，不将Headless控件验证称为Windows截图验证。后续L1-04原生关窗交互可一并复核动态类型显示。
- 委派运行器改用文件收集输出及整棵子进程树超时终止，正常完成未触发超时。测试只使用隔离数据；候选包更新至artifacts/desktop-l1。
- 下一项：L1-04草稿退出保护；仍待开始，不是后台自动运行状态。

## L1-04 验收（2026-11-02）

- Claude CLI约277秒完成实现：Closing拦截、窗口内确认层、复用SaveCurrentAsync逐个保存草稿、部分保存提示、失败保留窗口；不停止后台服务。
- 委派偏差：虽以隔离目录启动并要求限定路径，Claude实际把补丁写入主工作区对应三个文件，隔离副本未变化。Codex逐项检查工作区diff后才接受；“cwd隔离+提示限制”不能视作文件系统沙箱。今后优先将有限文件内容作为输入、禁用写工具，要求返回补丁，由Codex应用；需要写工具时必须使用实际权限隔离并核验落点。
- Codex新增独立关闭用例。旧c3e3d47实现失败于dirty window closed without protection；新代码73项通过。包括仅标题、仅其他项目草稿、重复关窗、Escape取消、禁用底层UI、跨项目全部保存、冲突部分成功、重试不重复写入、恢复最新后关闭、busy拦截、断线保留、显式放弃。
- Codex把提示中的“作用域/草稿表”改为用户能理解的“份草稿/未保存的编辑”；明确部分成功数量。
- 原生验收以独立ClosingReview入口加载同一MainWindow实现，连接artifacts/closing-uat-data；真实用户库和运行中的桌面未替换。Alt+F4触发确认、取消默认焦点、Escape保留正文、键盘选择保存退出均通过；API确认正文完整且只写一次，关窗后后台服务仍响应。
- 实际截图artifacts/desktop-closing-review.png通过Alt+Print及画图保存，1162×812下确认层文字和三按钮完整。没有声称多DPI/最小窗口/所有异常路径均做了原生像素测试，异常路径由真实控件+隔离HTTP测试覆盖。
- 草稿仍只在内存。此任务保护正常关窗，不承诺崩溃/强制终止恢复；持久化恢复属于L2-06。
- 下一项L1-05：统一新版入口及旧版归档，随后L1-06主流程CI。尚未启动后台自动任务。

## L1-05 验收（2026-11-02）

- 本地 Claude CLI 33 秒正常结束。本轮直接调用 CLI 可执行文件，通过参数数组传入空 tools、空 MCP 配置；仅给有限的新版文档内容，要求返回文本，不授予编辑工具。没有委派配置、记忆库、提交或推送。
- 返回 JSON 缺少开头左花括号，Codex检查全文后修复格式再解析，未把格式错误当作可直接应用的补丁。Codex补齐时间/分页边界、默认数据目录、个人范围不传项目ID和发布边界。
- 根 README 以 Avalonia Windows x64 开发版和 MCP 自动记忆为入口；根 SKILL 仅指导五个 v1 工具，不要求逐轮写入，缺少工具时不回退旧版。根 ARCHITECTURE 改为新版导航；旧 README、架构和行为指令分别移入 docs/legacy，旧指令去掉技能 frontmatter 并避免使用 SKILL.md 文件名，连接示例匿名化。
- 旧 Tauri 接入器仍属于旧代码，可能寻找根 SKILL；归档明确禁止用旧接入器部署新版技能，不声称旧链路继续兼容。新版桌面不自动改写宿主指令。
- Codex独立核对源代码 scoped_tools_list：README/SKILL与全部五工具一致；18个相对文档链接可解析；PowerShell构建脚本语法有效；git diff --check通过。技能校验器最初因Windows默认GBK读取UTF-8失败，以python -X utf8重跑通过。
- 核对build-desktop.ps1及csproj的实际构建/复制目标；桌面文档的smoke目录改为GUID，避免重复使用固定测试库。这是文档批次，没有重建或覆盖运行中的用户安装，也没有宣称在全新机器完成构建或真实宿主调用。此前代码验收记录继续有效。
- 下一项L1-06：Windows干净checkout契约CI；随后L1-07真实宿主矩阵。两项尚未完成。

## L1-06 本地验收（2026-11-02）

- Claude CLI 71秒返回workflow和runner文本草案，无工具权限。返回带代码围栏及额外说明，Codex解析后审查；草稿错误猜测两个测试项目路径，已按仓库真实路径修正。路由stderr报告GLM-5.2映射，不把请求sonnet等同于底层模型身份。
- 增加Windows push/PR/手动工作流，contents只读权限、并发取消与45分钟上限；不发布或修改宿主。共用test-windows.ps1检查每个命令退出码，WinExe显式等待并读取退出码与日志。
- Cargo.lock从忽略改为跟踪，Rust构建/库测试使用--locked。原build-desktop脚本同步锁定依赖；CI使用独立artifacts/ci-desktop。
- 本地完整脚本通过：11项Rust、102项配置、73项真实控件+隔离HTTP、真实MCP服务生命周期集成及自包含客户端smoke。已有Rust警告保留，不声称零警告。
- 独立注入首个cargo命令退出23，验证立即中止且恢复GM环境。首次测试发现.NET环境变量置null仍使PowerShell Env项可见，改用Env provider删除/恢复后通过。没有使用真实Agent配置和工作库。
- 工作流YAML解析及事件/权限核对通过；Actions版本参数对照各官方action.yml。GitHub Actions已启用；main目前没有分支保护或ruleset。远端干净运行和必需检查设置尚未完成，不提前标验收通过。

### L1-06 首轮远端失败与修复

- HTTPS推送新增workflow被现有凭据缺少workflow scope拒绝。未扩权或修改凭据；使用已有SSH密钥和单命令代理成功推送58c774e，没有改全局Git配置。
- 首轮远端run 41413688439在Rust库测试失败：旧import_all_writes_real_data依赖开发机真实记忆，干净Windows没有数据而断言total>0。失败后下游没有继续，证明脚本能传播测试失败。
- 更正本地隔离结论：旧Rust导入测试曾读取本机现有记忆并导入临时测试库，不会改原文件，但不能称为纯合成隔离。本轮发现后移除这些依赖及无文件就跳过的行为；此前服务/桌面/配置测试仍使用隔离目录。
- Codex以空HERMES_HOME/CLAUDE_HOME/CODEX_HOME运行旧测试二进制，复现exit101。随后保持公开导入API及默认目录规则，抽出显式目录的内部实现；Claude CLI 48秒提供固定样本测试（无工具权限），Codex审查应用并补充重新打开数据库核验数量及来源。
- 新6个样本用例覆盖Hermes MEMORY/USER/技能及跳过自身技能、Claude frontmatter/噪声、Codex JSONL噪声/坏行、三源汇总、缺失源；每个UUID临时目录，释放数据库后清理，不改全局环境、不跳过、不读取真实宿主目录。
- 修改后本地完整脚本通过：13项Rust、102项配置、73项编辑/关闭、stdio/HTTP及自包含客户端smoke。最后新增持久化断言再跑13项Rust通过。等待第二轮远端干净验证。

### L1-06 最终验收

- 代码提交44130cdc5a74c943e12929bcfccf7ff39665fd8b的[远端运行41414448415](https://github.com/Doodle-Lin/graph-memory/actions/runs/41414448415)结论success。Windows-2022新checkout、不复用构建缓存：服务/MCP构建、13项Rust、102项配置、73项编辑与退出、真实stdio/HTTP集成、自包含客户端隔离smoke全部通过；Codex读取作业日志逐项核对，非仅看绿色徽标。
- 依据L1-06“失败阻断合入”验收要求，为main启用规则Require Windows v1 contract（ruleset 26353094），限定检查名Windows v1 contract及GitHub Actions app 15368，严格要求与最新目标分支验证，无绕过者。读取rules/branches/main确认生效。不修改其他分支规则，也没有合并到main。
- 新规则意味着main更新必须已有该检查通过；旧分支若没有工作流，需要带入新工作流后再验收。规则配置在GitHub，仓库文件本身不能替代服务器设置。
- 本台账更新仅记录上述已验证代码与服务器状态；后续文档提交产生的运行是另一条记录，不能混称为44130cd的运行。
- L1-06完成。下一项L1-07：真实宿主验收矩阵，区分配置、工具发现、实际调用与结果采用。用户正在运行的artifacts/desktop仍未被本轮CI产物替换。

## L1-07 验收（2026-11-02）

- 前置473476e的远端运行41415133900已success。工作区干净，未更改真实配置或工作库。用户配置只提取Graph Memory条目和布尔状态，没有输出其他服务凭据。
- 按claude-delegate技能委托本地Claude CLI起草分层矩阵，13秒完成，无工具权限；Codex纠正草稿把“双会话”写成A/B/C三会话的问题，并用实际日志填入结果。没有沿用草稿中的待填结论。
- 新增可复跑scripts/verify-claude-mcp.py：隔离合成库、strict MCP、空builtin工具、只允许五个v1工具、禁用hooks/自动记忆、两个新会话、独立HTTP校验、跨项目哨兵、超时进程树及服务清理。模型验收显式执行，不加入无凭据CI。防止Python -O禁用断言的前置检查返回exit2。
- Claude Code2.1.295实际发现五工具并各成功调用一次；项目随机值检索、个人记录写入、expected_version纠正至v2、另一个新会话准确找回纠正内容均通过；总记录4、无重复、其他项目哨兵未进入会话。请求sonnet但代理响应自报DeepSeek-V4.1-Flash-B200，不能宣称验证了Sonnet模型。
- Codex CLI0.157.1读取到v1配置。首次新进程调用被approval_policy=never阻止，服务零调用；没有把调用尝试当成功。仅在第二次隔离进程将暴露工具缩为projects/search并分别设置approval_mode=approve，实际各成功一次，模型复述随机码且库内仍只有种子记录。用户配置与写入权限未改。当前Codex桌面对话仍暴露旧七工具，按技能约定未回退调用旧库。
- Hermes CLI0.21.2日常无Graph Memory条目；独立临时HERMES_HOME下真实mcp test握手1703ms、发现五工具。收集器首次向GBK输出对勾异常，UTF-8原日志完整，重新读取确认；测试服务已结束。模型读写未测。Hermes CN Desktop旧条目的exe不存在，保留实验性/未测标记，不自动覆盖用户配置。
- [宿主矩阵](../HOST_COMPATIBILITY.md)记录原始证据位置、二进制SHA-256、适用范围、失败及复测结果。配置102项回归与宿主调用证据分别报告；没有声称所有任务会自主采用记忆。
- L1-07满足“分层矩阵+至少一个真实新会话完整闭环”的门槛；L1各项完成。仍待Codex写入纠正、桌面重载和Hermes模型闭环；这些不能用Claude结果替代。下一主线L2-01导出格式及L2-02一致性备份，先让数据可托付。

## L2-01 本地验收（2026-11-02）

- 前置ebadaa3远端run41416963877已success。核对MemoryStore实际SQL表及create/update/undo事务后，由Codex制定[导出格式v1](BACKUP_FORMAT_V1.md)：整个工作库四张业务表，保留所有版本、来源引用、事件请求/响应原始字符串；不收集宿主凭据、发现文件、调用统计或草稿。明确明文隐私边界，不声称记录正文已脱敏。
- Claude CLI69秒交付纯序列化文本，禁用工具和MCP；Codex审查后应用。没有委派真实库导出或文件覆盖。序列化包含固定版本、manifest计数、主键排序和payload SHA-256，64MiB边界；checksum不作为签名或真实性证明。
- Codex独立实现关系校验：索引/body一致、项目与原案例引用、完整连续历史、最新版本等同当前、创建事件与版本1一致、幂等事件不可缺失。首次编译暴露Codex校验器将临时response内id借用留在集合的生命周期错误，改为拥有的String后通过。补充nullable project_id字段必须显式存在，避免缺键被serde悄悄填null。
- 独立测试通过真实MemoryStore API生成合成SQLite，不读取真实工作库。中文emoji长文、空及自定义kind、纠正/撤销至v3、原案例关系、未知请求字段、event_id原始空白及原始JSON空白全部往返保留；事件request/response逐字节相同。重新排序输入仍得到确定输出；独立hashlib提供空payload固定摘要。
- 破坏测试覆盖未知字段/版本、重复JSON字段、缺nullable键、错计数/摘要、截断、64MiB超限、乱序、重复主键、丢项目/历史/事件、失效原案例、错误时间、最新记录不一致、事件错误指向v3；即便重算正确摘要仍拒绝关系错误。不回显合成私密正文。
- 本地完整19项Rust测试通过；最终nullable/重复键补充后6项格式测试再次通过；服务及MCP的headless构建通过。保留既有Rust警告，没有声称零警告。
- 本批仅交付纯内存encode/decode与契约。没有导出CLI、活动库一致性快照、真正恢复或幂等重放演练；不能拿本批测试证明L2-02/03完成。下一项L2-02将以同一SQLite读事务采集完整数据，并验证活动写入、损坏及失败不覆盖原库。


## L2-02 本地验收（2026-11-02）

- 前置0a07452的远端运行41418751899已success。Claude CLI以禁用工具、空MCP方式98秒返回只读SQLite快照模块；Codex审查应用。没有把真实库、宿主配置或凭据交给委派。
- 在一个只读事务中完成结构检查、quick_check、大小预检及四表读取。明确读取WAL已提交数据、排除未提交数据，不调用会建库/迁移的MemoryStore::open；未知版本、表/列或视图拒绝。业务内容仍由L2-01校验器检查完整引用与历史。
- Codex实现独立graph-memory-backup export/verify CLI，必须显式指定目录与新输出。暂存文件write_all+sync_all后同目录硬链接发布，已有文件及竞争创建目标均不覆盖；文件系统不支持硬链接时拒绝，无覆盖式回退。错误不回显记录内容。
- 6项独立Rust专项测试覆盖读取四表中途并发提交、WAL与未提交事务、排除调用统计、缺库/父目录、现有目标、竞争发布、注入部分写入失败与临时文件清理、未知结构/不一致数据拒绝且原库不修复，以及损坏文件不改写。
- 新tests/test_backup_cli.py启动临时目录真实服务，HTTP线程持续创建项目记录，连续4次导出/校验并独立检查计数、连续历史及最新版本；中文空格路径、必填参数、未知开关、错误路径、目标原库、禁止覆盖、截断/未知版本/摘要损坏均验证。服务保持同一实例，输出失败后原记录数量不变；测试未使用真实用户库。
- 完整scripts/test-windows.ps1通过：三Rust二进制构建、25项Rust、102项配置、73项真实控件+HTTP、stdio/HTTP集成、新备份CLI集成、自包含桌面smoke。候选包artifacts/ci-desktop包含备份exe；用户运行中的旧桌面包未替换。已有Rust警告及NuGet NU1900漏洞源访问警告保留，不能称零警告或漏洞检查通过。
- [使用说明](../BACKUP.md)明确明文、64MiB、整库范围、硬链接约束和编码时间不等于业务截止时间。未实测断电/真实磁盘耗尽，不承诺断电后目录项持久性；强制终止可能留下暂存文件。正常失败路径已验证。
- 当前交付是CLI备份与校验，不含恢复、桌面数据管理入口或真实用户库备份。本地验收通过，当前提交远端CI待核验；下一项L2-03将恢复到新目录并演练ID/版本/引用与幂等重放，不能提前标完成。

## P0 可用性批次（2026-11-04，团队并行，基线 368370c）

触发：用户要求"优化成一个实际可用易用的应用"。契约：[USABILITY_P0.md](USABILITY_P0.md)（唯一来源，含 P0-A/B/C/D 与验收命令）。分工：rust-core（P0-A + MCP schema）、data-restore（P0-B + 缺陷修复）、desktop-ux（P0-C）、verifier2（P0-D 独立验收，只读产品代码）、lead（契约、集成、最终验收）。

实测起点（lead 在黑盒 HTTP 上复现）：`memory_search` 中文自然问句 6/6 全 0 命中（按空格分词导致整句成为单个子串）；`memory_context` 只按时间倒序返回，不区分阻塞/下一步/决策；`evidence_type` 恒为陈述；备份只能导出/校验、不能恢复。

实现与验收结果：

| 项 | 内容 | 结果 |
|---|---|---|
| P0-A1 | search 词条匹配（CJK>2 字符 bigram 兜底）+ 相关度打分 + `ranked`/`score` | 中文长问句命中、纯噪声 0 召回、AND 语义、项目隔离、空 query 时间倒序均通过 |
| P0-A2 | context 新增 `blockers`/`next_steps`/`recent_decisions`，`text` 改结论优先固定段落 | 11 键齐全、分组精确、组内倒序且≤10、空组"（无）"、40/20 截断语义不变 |
| P0-A3 | create/update 可选 `evidence_type`（4 值白名单，非法 400） | 缺省陈述、4 合法值接受、4 非法值 400、update/undo 语义不变 |
| P0-A4 | MCP `memory_write`/`memory_update` schema 暴露 `evidence_type` | 真实 stdio `tools/list` 含 4 值 enum；旧七工具 schema 未变 |
| P0-B | `graph-memory-backup restore --input --data-dir [--dry-run]` | 校验失败不写文件；非空目标拒绝；恢复库四表计数与 manifest 逐表一致；原库 SHA256 不变 |
| P0-C | 桌面首屏自动加载、依据标签与下拉、空库引导、续接区 | Editor 73、Setup 102 全绿；真实服务与 stub 探针 30 项通过 |
| P0-D | 独立黑盒验收 `scripts/verify-usability.ps1` | **39 PASS / 0 FAIL / 0 SKIP**；证据 `artifacts/usability-verify/run-20261104-132258/`（report.txt/json、ACCEPTANCE.md、http/*.json、*.argv.txt、套件原始日志） |

集成期发现并修复的真实缺陷（P1，阻断备份恢复）：`backup_format/validate.rs` 把 `evidence_type` 当作修订不可变字段，A3 生效后任何发生过依据变更的工作库 `export` 都失败（`revision immutable fields differ`）。修复：从不可变集合移除该字段，保留 created_at/memory_scope/project_id/origin_memory_id 约束；同步修订 [BACKUP_FORMAT_V1.md](BACKUP_FORMAT_V1.md) 与契约。回归：36 项 Rust（新增 2 项）+ ADV-04A 真实 CLI export/verify。

lead 最终验收（本轮）：
- `cargo test --no-default-features --lib` → 36 passed；
- `tests/Desktop.Setup.Tests` → PASS 102；`tests/Desktop.Editor.Tests` → PASS 73（真实 XAML 控件 + 隔离 HTTP）；
- `test_headless_service.py`、`test_backup_cli.py` → 各 Ran 1 test OK（exit 0）；
- 自包含发布 `artifacts/desktop-p0` + 全新临时目录 smoke → PASS；
- 独立复验缺陷修复：evidence_type v1→v2 后 export/restore/verify 均 exit 0，计数一致。

边界与未完成：本批不是语义检索（无向量）；未被 Agent 显式要求时不会自动写入；恢复不是合并、无冲突预览；桌面无导出/恢复入口；软删除/撤回、occurred_at、真实宿主全链路矩阵仍属 L2/L3/L5。文档已同步：README、desktop/README、SKILL、AGENT_INSTRUCTIONS、docs/BACKUP。本批产物为 `artifacts/desktop-p0`，未替换用户正在运行的 `artifacts/desktop`。未提交、未推送。

P0 后续：本批 13 个文件已提交为 5 个本地提交 `4f6f882`(引擎) / `9d4ea21`(恢复+格式修复) / `412767e`(桌面) / `814d3a8`(契约与验收脚本) / `13b9525`(文档)，分支 `codex/avalonia-mcp-memory`，**未推送**。

## P1 数据生命周期批次（2026-11-04，团队并行，基线 13b9525）

触发：P0 之后下一步是让"错误结论可撤回、可恢复"，即 L2-04。契约：[DATA_LIFECYCLE_P1.md](DATA_LIFECYCLE_P1.md)（唯一来源）。分工与 P0 相同：rust-core（P1-A + agent/call 转发）、data-restore（P1-B 备份兼容）、desktop-ux（P1-C 桌面入口）、verifier2（P1-D 独立验收）、lead（契约、集成、最终验收）。

| 项 | 内容 | 结果 |
|---|---|---|
| P1-A | `retract`/`restore` 端点、`retracted_at`/`retract_reason`、search 默认排除 + `include_retracted`/`retracted_excluded`、context 始终排除并在 text 追加「已排除 N 条撤回记录」 | 45 项 Rust 全绿；真实 stdio 端到端：v1→v2 撤回、默认 0 命中、`include_retracted` 命中、v3 恢复、互斥 400、409 无新版本 |
| P1-A2 | `/api/v1/agent/call` 完成 `memory_update` 的 `retract`/`restore` 转发（MCP 仍五工具） | schema + 转发均实测；互斥/非布尔 400 不写版本 |
| P1-B | 撤回字段视为可变修订字段、随 body 原样往返 | 45+3 项；含 4 个撤回状态的库 export→verify→restore 计数与历史一致、再导出 `payload_sha256` 相同；旧包仍可解码 |
| P1-C | 列表默认隐藏已撤回 + 「显示已撤回」开关 + 原因摘要；编辑器「撤回这条 / 恢复这条」；409 保留草稿 | Setup 102、Editor **73→86**（追加 13 项，0 删除） |
| P1-D | 独立验收 `scripts/verify-usability.ps1`（新增 P1-01..P1-09） | **51 PASS / 0 FAIL / 0 SKIP**；证据 `artifacts/usability-verify/run-20261104-151219/`（report.json/txt、ACCEPTANCE.md、逐断言 argv + 原始 stdout/stderr、sqlite 只读核对） |

独立验收过程中 verifier 首轮报 6 个 FAIL，lead 逐条用真实服务 + 真实 stdio 复现后判定**全部是验收脚本/宿主问题、产品无缺陷**：`P1HeadTitle` 未在 `[pscustomobject]` 声明导致连带崩溃；P1-02 把 `total` 与 `retracted_excluded` 混用（正确期望是排除后 `project_total=0`、`excluded=1`）；旧库夹具缺 v1 创建事件（既有校验 `missing creation event`）；PowerShell 5.1 按 GBK 解析无 BOM 脚本 + stdin 编码导致中文 JSON 损坏。lead 独立日志：`artifacts/usability-verify/lead-p1-mcp-check.log`。

lead 最终验收（本轮，独立重跑 verifier 脚本）：`scripts/verify-usability.ps1 -Full` → exit 0、51 PASS / 0 FAIL，证据 `run-20261104-152041`；`tests/test_headless_service.py` 由 lead 追加撤回/恢复断言（21 行）后通过；自包含发布 `artifacts/desktop-p1` + 全新临时目录 smoke exit 0。
P1 提交：`dd570b6`(契约) / `9b24f36`(引擎) / `15b42e7`(备份) / `c4682e5`(桌面)，均本地未推送。

边界与未完成（P1）：撤回是状态标记而非物理删除，没有批量撤回、撤回审批、撤回原因强制或撤回统计；`retracted_at` 不做严格 RFC3339 校验（与既有宽松 serde 策略一致）；旧库兼容只覆盖"无撤回字段的 body"，不含历史遗留 schema；occurred_at 事件时间、真实宿主全链路矩阵仍属 L3/L5。`artifacts/desktop-p1` 未替换用户正在运行的 `artifacts/desktop`。

## P2 发布就绪批次（2026-11-04，团队并行，基线 435894f）

触发：P0/P1 之后最大的"不可用"点是普通用户仍要敲命令行才能备份/恢复，且项目没有版本一致的可分发产物。契约：[RELEASE_READY_P2.md](RELEASE_READY_P2.md)。分工：desktop-ux（P2-A/B）、data-restore（P2-C）、lead（契约、集成、独立复核）、verifier2（P2-D，**该实例在产出结果前失败**，由 lead 接管独立复核）。

| 项 | 内容 | 结果 |
|---|---|---|
| P2-A | 桌面【数据备份与恢复】面板：库目录与计数、导出、校验、恢复到空目录（二次确认）、目标已存在 UI 先拒绝 | Editor **86→127**、Setup 102（lead 复跑确认编译 0 警告 0 错误） |
| P2-B | 桌面侧 headless 用例（真实 CLI + 临时目录）：导出/覆盖拒绝/dry-run/非空目标/截断包/空路径 | 见上，P2 段落 28 项 |
| P2-C | `scripts/package-release.ps1` + `scripts/verify-release.ps1`；版本统一 **0.3.0**（Cargo.toml/csproj/mcp_server.rs/Cargo.lock/VERSION.txt） | 打包 exit 0（227 文件 153.99MB、ZIP 62.48MB），verify-release 20/20 PASS |
| P2-D | 独立复核（lead 接管）：`artifacts/usability-verify/lead-p2-probe.ps1`，**只用 ZIP 解压出的发布物二进制** + 对抗性 CLI 用例 | **20 PASS / 0 FAIL**；日志 `artifacts/usability-verify/lead-p2-probe.log` |

**lead 独立复核中发现的真实缺陷（P2 阻断级，已修）**：v1 API 返回中文 JSON，但响应头只有 `Content-Type: application/json`（HTTP 默认字符集是 ISO-8859-1）。字节本身是合法 UTF-8，**缺陷在"声明"而非字节**：缺 charset 时严格按规范回退的客户端会按 Latin-1 解码中文，桌面端 `memory_context` 正文与中文错误信息显示为乱码；宽松客户端（Python urllib、reqwest）因默认 UTF-8 而看不见问题，所以 P0/P1 的 Python/Rust 验收全部漏过。
定位过程：lead 的 P2 探针断言 `text` 含「未解决阻塞」失败 → 改用显式 UTF-8 读取原始字节即通过（证明字节正确）→ 去掉探针 charset 提示后仍失败（证明客户端确实在按 Latin-1 回退）→ 根因确认为缺失 `charset=utf-8`。
第二方复核（verifier3）进一步收紧了判定口径：**决定性证据是 Windows PowerShell 5.1 的 `Invoke-RestMethod`**（缺 charset → 按 Latin-1 回退，正文出现 `ä»¥ä¸‹...`、标记 False；发布物 → 正文正确、标记 True）；而**现代 .NET（net10 的 `ReadAsStringAsync`）修复前后都正常**（默认 UTF-8），因此"桌面端必然乱码"这一表述不准确——缺陷本体是响应头声明缺失，桌面端受影响与否取决于客户端实现。该二方复核同时确认：ZIP 与目录 227=227 全字节一致、独立重跑 `verify-release.ps1` 20/20、亲自做的 export→verify→restore 往返 `payload_sha256` 一致。
修复：`static_server.rs` 的 `send_json`/`send_err` 统一声明 `application/json; charset=utf-8`；`tests/test_headless_service.py` 增加响应头断言；重新打包并复验。修复后：lead 探针 20/20、verify-release 20/20、headless 套件通过。（探针中还有两处**探针自身**的问题：P1 转义写坏了中文模式；`Start-Process -ArgumentList` 不转义含空格路径——产品 CLI 本身对中文空格路径工作正常。）

**第二方复核另发现缺陷 D1（发布物级，已开 task-20 修复）**：「恢复到非空目录必须拒绝」在链路上不一致。契约 `RELEASE_READY_P2.md` 要求"任何非空目标拒绝"，已出货桌面 UI（`EnumerateFileSystemEntries().Take(1)`）也如此，但 CLI `backup_io.rs` 只检查目标下是否已有 `work-memory.db`：目标只放一个无关文件 `keep.txt` 时 `restore` **exit 0 并写入 `work-memory.db`**（lead 已复现，`artifacts/usability-verify/lead-d1-repro.log`），而目标含 `work-memory.db` 时才拒绝。不是数据损坏（无关文件未被覆盖），是契约与实现不符 + CLI/UI 语义分叉。裁定：改实现（统一为"目标必须已存在且完全为空"），不改文档、桌面无需改动。

lead 最终验收（独立重跑权威脚本）：`scripts/package-release.ps1` exit 0 → `scripts/verify-release.ps1` **20/20 PASS** → `artifacts/usability-verify/lead-p2-probe.ps1` **20 PASS / 0 FAIL**（解压发布物跑检索/续接/撤回/恢复/导出往返/并发导出/覆盖拒绝/dry-run）→ `tests/test_headless_service.py` exit 0。
P2 提交：`4213fa5`(版本与 bin 目标) / `c91f237`(打包与发布验证) / `ff20274`(桌面数据面板) / `53698ed`(charset 修复 + 契约)，均本地未推送。

边界与未完成（P2）：没有自动更新器、安装程序、代码签名、云端同步或跨平台包；恢复仍是"只写空目录"，合并式恢复归 L2-03；`package-release.ps1` 需要本机 Rust + .NET 10 SDK；发布包内 README 为脚本生成的精简版。

第二方复核（verifier3，task-15，`artifacts/usability-verify/p2-verifier3/`，30 PASS / 0 FAIL）：修复了 P2 批次"缺少第二台独立执行验收"的残余风险，并证明在**从 `53698ed` 用 git archive 导出的 P2 基线树**上回归全绿（cargo 45、Setup 102、Editor 127、Python 19、headless OK、ZIP 内 smoke exit 0），因此 P3 未提交改动不会污染 P2 判定。其两点非缺陷观察已记录：LF-only/CRLF 客户端差异（后由 task-18 证明是真实链路缺陷，见 P3 批次）、以及各版本字段语义（见 P3_CONTRACT.md 术语口径）。

## P3 批次（2026-11-04，团队并行，基线 a1bc215）

触发：P0/P1/P2 之后，"能装、能用、数据可托付"已成立，但还有三处直接影响实际可用：草稿只在内存里（崩溃即丢）、召回质量没有任何数字、本机 HTTP 接口没有资源边界。契约：[P3_CONTRACT.md](P3_CONTRACT.md)。分工：desktop-ux（task-16）、rust-core（task-17）、data-restore（task-18/20）、lead（契约 + 集成 + 打包 + 冻结诊断）、verifier3（task-19，第二方独立验收）。

| 项 | 内容 | 结果 |
|---|---|---|
| task-16 | 草稿持久化与崩溃恢复：工作库之外分片、原子写、损坏降级、过期基线提示、放弃即删条目 | Editor **127 → 215**（+88 断言，含 3 个真实双进程杀进程用例）；Setup 102 不变 |
| task-17 | 中文检索评测集与基线（68 查询 / 27 记录，冻结二进制 + SHA256） | Recall@5 **0.7959**、MRR **0.7619**、跨项目污染 **0**、撤回泄漏 **0**、无答案误召回 **0**；两次运行完全一致 |
| task-18 | 本机协议加固：431/413（边读边判）/408/503 并发 64/405+Allow/400 不 panic | Rust **45 → 60**；Python headless 1 → 6 用例 |
| task-20 | D1 修复：restore 拒绝任何非空目标目录 | Rust 60（新增 3 用例）；CLI 六案（含 dry-run、空子目录、目录不存在）全符合 |
| task-19 | 第三批独立验收（第二方） | 见下"残余风险" |

**本批发现并修复的两个真实缺陷**

1. **静默写入失败（task-18 顺带发现，严重）**：`read_full_request` 把请求头与正文一起做 `\n→\r\n` 归一化，再用 `find("\r\n\r\n")` 切正文。于是所有用 **LF-only 请求体**的客户端（例如 Python `http.client` 默认）JSON 正文解析为空：`POST` 返回 200、库里却什么都没写，`search` 返回空。既有 Python 用例一直用 `urllib`（发 CRLF）所以完全掩盖了它；task-17 的评测器用 LF 请求体时也踩到，但当时被当成"写 200 但检索不到"的怪现象。修复：头部按行读取并归一化，正文按声明的 `Content-Length` 单独切分，并给 LF-only 路径补了回归。
2. **D1（task-15 发现，发布物级）**：见上一节；`backup_io.rs` 现在要求目标"已存在且完全为空"，`read_dir` 出错 fail closed，dry-run 同样拒绝。

**lead 独立验收（P3）**：`cargo build --no-default-features` exit 0 → `cargo test --no-default-features --lib -- --test-threads=1` **60 passed / 0 failed**；`dotnet run tests/Desktop.Setup.Tests` **PASS 102**；`tests/Desktop.Editor.Tests` **PASS 215**；`python tests/test_headless_service.py` 6/6、`tests/test_backup_cli.py` 1/1；`scripts/package-release.ps1` exit 0（227 文件、ZIP 62.50MB）→ `scripts/verify-release.ps1` **20/20 PASS**；`tests/eval/run_eval.py` 在**加固后**的二进制上重跑，Recall@5/MRR 与基线**逐位相同**（说明协议加固没有改变检索行为，也证明 task-17 的冻结二进制方法有效）。
P3 提交：`5c8a49f`(评测集与基线) / `9e9ea94`(协议加固 + LF-only 修复) / `9944fe9`(草稿持久化) / `14642a0`(restore 空目标) / `5543190`(文档与账本) / `6c54917`(413 可读性)，均本地未推送。

**残余风险（必须如实记录）**：P3 契约与评分口径的 task-19（第三批独立验收）在打包冻结之后才开跑。**task-19 结论（2026-11-05 回填）**：在最终调试二进制 `graph-memory-service.exe` SHA256 `9369C954…1C65`、`graph-memory-backup.exe` `163089C4…3DD1`、HEAD `6c54917` 上 **P3 产品代码 = PASS**（section A 协议+D1 40/40、section D 草稿崩溃恢复 21/21、section E eval+原始 HTTP 22/22、RST 竞态 8/8、`verify-usability.ps1 -Full` 57 passed / 0 failed、cargo 62→63 / Setup 102 / Editor 215 / Python 7+1 / smoke exit=0、eval 四跑 metrics 逐位一致）。**发布物层级 = PENDING**：`artifacts/release/**` 仍是 18:07 的包（包内 service `5CA77E22…C3BB`、ZIP SHA256 `F1D7BB04…21BCB`），**不含 task-20/task-21 修复**，必须重打包后复验才能闭环。复核者如实标注两点边界：`-Full` 的 SUITE-RUST 跑在"HEAD + task-22 检索 WIP"的树上（被测二进制未变，判定不受影响）；"过期基线 + 409"与"放弃草稿条目消失"未单独双进程复跑，由 Editor 215 项内真实服务+真实窗口用例覆盖。

**task-19 之后发现并修复的第三个真实缺陷（task-21，P3-F）**：超限正文被拒时**客户端常常收不到 413**——同一二进制里时而 `HTTP/1.1 413`、时而只剩 `ConnectionResetError`。根因是关闭路径 `drain_before_close` 只在"已缓冲数据"上排空（100 ms 超时 / 256 KiB 上限）就 `shutdown(Both)`，客户端仍在推送数 MiB 正文，Windows 关闭这种 socket 发 **RST**，RST 丢弃对端接收队列里**已经写好的 413**。修复：写完响应后先 `shutdown(Write)` 半关闭 → 排空到 **EOF** / 250 ms 空闲超时 / 8 MiB 上限 → 再 `shutdown(Both)`；413 仍是"声明超限即拒"，上限与超时都保留。修复前后对照：`readable-413` 38/40 → 48/48，压力 3×20 轮 **240/240 全部可读**；lead 独立复现三轮正文（1 MiB+64B / 2 MiB / 8 MiB）**0.00 s 收到可读 413**（`artifacts/usability-verify/lead-p3-raw.log`、`lead-p3-order.log`）。commit `6c54917`。

边界与未完成（P3）：草稿仅存本机单一副本（无云同步/加密/多用户），30 天清理只在保存时触发；协议加固只覆盖本机 HTTP 面，没有鉴权/令牌（仍属 L5-01 未做）；没有自动更新器/安装程序/代码签名/跨平台包。`artifacts/desktop`（用户正在使用的开发版）全程未被覆盖。

## P4 批次（2026-11-05，检索质量，基线 6c54917）

触发：P3 的基线把最真实的短板量了出来——`同义改写（不照抄）` Recall@5 只有 **0.3333**，即"用户换个说法就找不到自己的记忆"。契约：`docs/design/RECALL_BASELINE.md`（基线数字）+ `docs/design/USABILITY_P0.md` P0-A1（检索语义，**既有公开契约**）。

| 项 | 内容 | 结果 |
|---|---|---|
| task-22 | 检索策略原型：长 CJK 片段按二元组切成独立匹配单元 + 强/弱证据门槛 | 原数据集 Recall@5 **0.7959 → 0.8571**、MRR **0.7619 → 0.8367**、paraphrase_hard **0.3333 → 0.5333** |
| task-23 | 独立复核（实现者之外） | **不合入（当前形态）**，见下 |
| task-24 | 按复核意见收紧：多词条恢复 AND、去掉整词 token 强证据 | R@5 **0.8571**、MRR **0.8469**、探针 **0/30**、S-A1-04 **PASS** |
| task-25 | 第二轮独立复核 + 规则③专项压测 | P4-C **合入**；规则③**必须删除**（零收益 + 10/10 假阳性）；P3 发布物层级 **PASS** |
| task-26 | 规则③删除后重打包 + 发布物终验 | **P4-D 合入 + 发布物终验 PASS** |

**task-23 复核做了什么（这是本批次最有价值的部分）**：复核者没有只复算自报数字（复算属实、两轮稳定、**自建构建结果相同**），而是
1. **换领域自写了一份全新数据集**（53 查询，不复制 `tests/eval` 任何文本，不提交仓库）：Recall@5 `0.7561 → 0.9024`、paraphrase_hard `5/12 → 9/12` —— 证明提升**不是过拟合原数据集**；
2. **反作弊审查**：`diff` 只在新旧匹配函数与其单测内，无 `fs::`/`File::`/`std::env`/`include_str` 读取，无 `dataset`/`eval` 引用，无硬编码中文词表，无按长度/类别/具体 query 的分支 → **判定可接受**；
3. **把"指标没覆盖到的代价"量了出来**：构造 **30 条与官方 `no_answer` 同形的"同项目多主题"查询**（3 个短语、至多 1 段出现在同一条记录里），基线误召回 **0/30**，P4-A **27/30（90%）**，返回条数 0 → 75。官方 `no_answer` 指标只看"无答案查询是否误召回"，覆盖不到"同项目内问 A 返回 B"这类**最常见的有害召回**；
4. **发现契约被单方面破坏**：P0-A1 明确"记录必须**全部词条命中**（保持 AND 语义）"，`scripts/verify-usability.ps1` 的 **S-A1-04 在 P4-A 上 FAIL**，而实现者只在报告里写"有意放弃 AND"——二者不能并存；
5. 量化性能 p95 `34.9 → 64.3 ms`（×1.84，无数量级退化但未报告），并给出**离线变体模拟**指明收紧路线（去掉"整词 token 也算强证据"后原数据集召回不变、误召回下降）。

教训（写进流程）：**"指标变好"不等于"产品变好"**。当评测集只覆盖"跨项目污染/撤回泄漏/无答案误召回"时，任何放宽召回面的改动都会在这些指标上"零代价"，必须同时准备**同项目多主题的假召回探针**（task-24 已要求把这 30 条固化为仓库内永久回归资产）。同样，**改匹配语义就必须同步改契约与断言**，不能只在报告里声明。

**task-24（P4-C 收紧）**：多片段（≥2）恢复 P0-A1 的**全片段 AND**；单片段长 CJK 改为逐 bigram 匹配，入选条件为"≥2 个 bigram 命中"或"1 个 bigram 命中 `title`/`conditions`"；去掉"整词 token 也算强证据"。结果：Recall@5 **0.8571（保住）**、MRR **0.8469**、paraphrase_hard **0.5333**、30 条探针 **0/30**、`S-A1-04` **PASS**、污染/撤回/无答案误召回仍 0。复核者还纠正了实现者的两处错误归因（详见 task-25）。commit `f0109a5`。

**task-25（P4-D 复核）**：复核者用离线模拟器复现匹配与排序、对官方 68 条查询的命中集合**与名次序列逐位对齐（0/68 mismatch）**，据此做开/关对照，证明实现者额外添加的**规则③（"bigram 连续段落在 content 末尾"即视为强证据）零收益**——三套语料上开/关的命中与名次逐位相同、单片段查询尾部命中次数为 0；同时**对抗语料 10/10 误召回**（5 条以常见二元词收尾的记录 + 10 条句尾同形问句，例如「宠物托运投诉处理」→ 报修工单流程，唯一共享 bigram 就是句尾「处理」），且该假阳性**已进发布包**。裁定删除；实现者照办后（commit `5c0f669`）官方指标与 P4-C **逐位不变**、探针仍 0/30、专项探针 **10/10 → 3/10**（剩余 3 条来自被设计允许的"单 bigram 命中 title"规则）。复核者同时指出：实现者此前"纯变体 B 会丢掉全部收益"的判断也不成立（丢收益的是"一律 ≥2 单元"，不是复核者建议的变体）。**教训**：位置/措辞类启发式要么有可构造的反例、要么收益为 0；单元测试不要用"构造字段内容恰好满足某规则"的方式近似规则，要直接断言分支结果——实现者的一条单测正是因为 title 里恰好出现尾部 bigram 而"通过"，掩盖了规则③在真实语料上从不触发的事实。

**task-26（P4-E 发布物终验）**：规则③删除后重新打包（228 文件，ZIP SHA256 `4FF24AFF…4A82`，65,546,634 B），复核者在**包内副本**上完成终验：SECTION-B **17/17**（ZIP↔目录 **228/228 逐文件一致**、4 exe 指纹、`SHA256SUMS.txt` 227 条全过且全覆盖、解压 smoke 的 exe 与 `dotnet GraphMemory.Desktop.dll` 两种形式均 exit=0、解压 mcp `0.3.0`、并新增"包晚于规则③删除时间点"的时间线断言）；包内 service 黑盒：413（1 MiB+1 / 2 MiB / 8 MiB 三轮 + 谎报 `Content-Length`）全部可读、431、405+`Allow`、LF-only、D1 六形态 → **SECTION-A 40/40**；`check_false_recall` **0/30**、`run_eval` R@5 `0.8571428571` / MRR `0.8469387755` / para `0.5333` / 131、专项探针 **3/10**、MCP 五工具 schema 与 `0.3.0` 差异为空、README 链接 0 broken、`verify-release.ps1` 往返 20/20。**结论：P4-D 合入 + 发布物终验 PASS。**
复核者另报一项**脚本侧缺陷 C1（不阻塞产品/发布物）**：`verify-release.ps1` 传相对路径 `-PackageDir` 会假失败 19/20（`Substring($PackageDir.Length+1)` 假设绝对路径，把正常 dll 误报成 `unlisted`）。已修（入口 `Resolve-Path` 归一化，commit `ff56a95` 之后的 `scripts/verify-release.ps1`），并用**相对路径**复跑得 20/20。

**P4 提交**：`f0109a5`(检索收紧) / `a50c5e8`(验收脚本收编) / `07a623f`(打包 README 链接) / `ff56a95`(服务清单竞态) / `5c0f669`(删除规则③)，均本地未推送。

**P4 边界与未完成**：只做纯字面（词条 + bigram）策略，**没有语义/向量召回**（L4-03 未做）——`paraphrase_hard` 到 0.5333 已是纯字面策略的天花板区间，再往上必须换引擎；性能 p95 与基线同数量级但放大 1.1~1.8 倍（长中文片段产生更多 bigram 扫描），未做倒排/索引优化（L4-05 未做）；`stale_outdated` 信息性误召回 2/4 未动（属 L3 时间语义）；发布物仍无自动更新器/安装程序/代码签名/跨平台包；`version` 字段保持 0.3.0（检索语义变更已同步契约文档，未改 breaking 契约）。评测集仍是 68 条中文小样本，**长期需要更大、更接近真实使用的语料**，否则"过拟合评测集"的风险只能靠复核者自建语料来缓解。


## 2026-11-16 收口治理

- P5全部原有工作区改动保存至codex/p5-semantic-hold-20261116（f801a40，24文件），交付线回到8fb9689的P4产品基线；不是删除实验成果，也不宣称P5已验收。
- 统一台账首页L/P映射，README纠正仍引用P3指标的“当前”表述，P3基线与P5契约明确历史/实验状态；详见GOVERNANCE_20261116.md。
- Claude以禁用工具文本核对提供映射清单，Codex独立取舍；未照搬其无Origin策略等推断。
- CI纳入68查询P4质量下限及30条误召回探针；5种合成指标退化均验证会失败。编辑器入口发现dotnet run退出0却缺完成标记，改为直接执行DLL，必须同时退出0并打印最终PASS。
- 最终完整scripts/test-windows.ps1本地exit0：63项Rust、102项配置、215项编辑/草稿、7项headless HTTP/MCP、备份CLI、P4质量门槛、30探针与桌面smoke全通过。日志artifacts/governance-20261116-final.log。历史Rust警告保留。
- Recall@5=0.8571428571、MRR=0.8469387755、跨项目/撤回/无答案误召回0；过时信息仍2/4，未掩盖。没有重打包、安装用户桌面或访问真实库。
- 本批推送交付线与实验保留线；当前提交远端CI需另行核验，不能用本地PASS代替。下一批先审P5验收缺口，存储职责拆分单独成批。
