# Agent 宿主兼容与验收矩阵

## 2026-11-17 工作日志追加验收

新入口 [verify-work-log-adoption.py](../scripts/verify-work-log-adoption.py) 用一个真实 Claude Code 会话发送三轮工作消息，再启动全新 Claude 会话和可选 Codex CLI 只读会话。工作消息没有要求“记日志”；宿主通过统一 Agent 指令主动决定调用。独立 HTTP 回读核对日期、状态、ID、版本、随机细节、记录总数及只读阶段没有写入。

- Claude Code 2.1.295：首日未完成进展自动创建一条日志；同日同任务回归完成更新原ID至v2；次日计划另建planned日志，前日保持不变。新会话按work_date和项目完整查询一周，读回两个随机编号、两条ID，说明记录空缺及用户陈述边界。
- Codex CLI 0.166.0：全新只读会话从同一隔离服务读到Claude写入的详细日志，按真实项目及work_date范围检索，正确区分完成/计划。只开放projects/search，不代表Codex自动写入已验收。
- 请求模型别名sonnet，Claude响应自报GLM-5.3；本页证明该宿主/路由样本，不宣称Anthropic Sonnet认证。Hermes的工作日志模型采用仍未验证。
- 成功证据：`artifacts/work-log-adoption/8a237eb2f1f941f2b6392f0f194550b3/`。第一次`f4b9a6d7e6f641a5b3686611d8cbc9bb`中Claude通过，Codex未通过：CLI嵌套配置合并保留了旧GM_SERVICE_DIR，覆盖GM_API_URL仍会接旧库，发生只读项目发现/空结果检索；没有写入。修复后同时覆盖command、args、GM_SERVICE_DIR、GM_SERVICE_EXE与API地址，复测成功。失败证据保留，不算隔离验收通过。
- 模型曾保留“回归待跑”旧标题，虽然正文/状态已完成；两位读者均指出差异。使用约定已补充同时更新title，不能声称提示能保证所有模型每次一致。多页大样本、长期自动记录漏记率、Hermes和Codex写入仍需后续独立验证。
- 用本机实际安装的0.4.0发布二进制再跑同一完整链路，结果通过，证据`artifacts/work-log-adoption/b88f33d9140b429793046c3cac91c5e5/`。这次完成状态与标题同步更新，模型未把用户陈述升级为工具结果，Claude与Codex新会话正确读回；仍是隔离合成任务。二进制哈希和采用指令哈希保存在report.json，测试进程已清理，日常服务保持运行。

复跑示例（先构建候选组件；需要已配置模型路由）：

```powershell
python -X utf8 scripts/verify-work-log-adoption.py --binary-dir src-tauri/target/debug
# 跨宿主再指定 --codex-exe <本机codex.exe完整路径>
```

此入口不修改日常宿主配置。成功样本不表示当前聊天已重载新版MCP；安装/切换另行记录。以下保留2026-11-02历史证据，不将旧版本矩阵当作当前状态。

本页记录 2026-11-02 的实际证据，区分配置、工具发现、调用成功、结果采用。配置文件存在或“测试本地组件”通过，不等于真实宿主已使用记忆。此轮未修改日常宿主配置或真实知识库。

## 兼容矩阵

| 宿主 | 配置证据 | 宿主工具发现 | 实际调用 | 结果采用与结论 |
|---|---|---|---|---|
| Claude Code 2.1.295 | 日常配置已有 v1；验收另用 strict 临时 MCP 配置 | 初始化事件列出全部五工具 | 五工具各成功一次；HTTP独立核对新记录v2、无重复 | 两个全新会话通过；已验证隔离任务闭环 |
| Codex CLI 0.157.1 | `codex mcp list --json`读取到启用的v1 stdio入口 | 新进程能识别并尝试调用项目列表、搜索 | 限定只读授权后projects/search均成功 | 复述未在提示中给出的随机码；写入/纠正未测 |
| Codex桌面当前对话 | 用户配置为v1 | 此对话仍仅暴露旧七工具 | 没有调用旧库，不声称v1成功 | 当前会话未重载；CLI结果不能替代桌面对话证据 |
| Hermes CLI 0.21.2 | 日常配置没有Graph Memory入口；临时HERMES_HOME配置v1 | 实际`hermes mcp test graph_memory_v1`握手成功，列出五工具 | 仅连接/发现测试，无模型读写 | 实验性；不声称日常已接入或会自动采用 |
| Hermes CN Desktop | 旧graph-memory条目的可执行文件不存在 | 未测 | 未测 | 实验性，需要用户实际安装版本的独立验证 |

JSON/TOML/YAML的102项合并与恢复回归仍有效，但只证明配置编辑逻辑，不能替代上表各层宿主证据。

## Claude：双新会话闭环

测试脚本为 [verify-claude-mcp.py](../scripts/verify-claude-mcp.py)，使用当前源码构建的 `artifacts/ci-desktop` 服务和MCP。这是需模型调用的显式验收，不加入无凭据的常规CI。

```powershell
python -X utf8 scripts/verify-claude-mcp.py
```

前提：先执行 `pwsh -File scripts/test-windows.ps1` 得到组件；本机Claude CLI已配置可用模型提供方。CLI不在默认npm路径时传 `--claude-exe <完整路径>`；可用 `--binary-dir` 指定候选组件、`--model` 指定模型别名。脚本不安装或升级宿主。

- 生成独立UUID目录和三个合成种子：Alpha项目的随机码、Beta项目的隔离哨兵、个人经验。随机码不会直接放入要求Agent回答该码的提示。
- 第一个新会话实际调用 projects → context → write → update，答复包含从MCP获取的Alpha随机码。独立HTTP检查新增记录只有一条、版本为2，总记录数为4。
- 第二个新会话仅拿到新记录标题，调用search并准确复述纠正后的随机码和ID；没有写入或纠正调用。
- 初始化事件列出五工具；服务计数显示五工具各成功一次。两个会话均未出现Beta项目哨兵，也没有调用MCP以外的工具。
- builtin工具列表为空、strict MCP仅连接隔离库、只放行五个v1工具、禁用hooks和自动记忆、不保存会话。测试服务由脚本启动并在finally结束；CLI超时清理本次进程树。
- 请求模型别名为sonnet，但代理响应自报 `DeepSeek-V4.1-Flash-B200`。这是Claude Code宿主通过，不能写成Anthropic Sonnet模型验证；底层路由身份未独立认证。

本次本地原始证据目录：`artifacts/host-acceptance/362dba9e61f34e32a1cb562ec580ee82`。其中有两个不同session_id的JSONL、服务日志及report.json；忽略目录不提交原始宿主日志。复跑会产生新的目录，不复用旧库。

样本经过明确的任务提示和使用约定引导。它证明工具链和本次结果采用，**不证明所有日常任务均会自主检索、写入或正确判断知识价值**，也不证明用户正在运行的桌面已升级。

## Codex审批与当前会话边界

首次新进程只读测试使用 `approval_policy=never`，两个调用均返回 `MCP tool call requires approval, but approval policy is never`，服务调用计数为空。模型没有编造验证码。这是宿主审批限制，不是服务检索失败。

只读复测限定 `enabled_tools=["memory_projects","memory_search"]`，仅对这两个工具设置临时 `tools.<tool>.approval_mode="approve"`，服务指向新的合成库；其他工具保持不可用，不更改用户配置。参数依据[官方MCP配置说明](https://learn.chatgpt.com/docs/extend/mcp)。不要把测试中的只读授权扩大为全工具放行；已有组织策略仍由宿主执行。

只读复测成功：新进程exit0，memory_projects与memory_search各成功一次；最终回答准确包含仅存于合成记录中的随机验证码。独立HTTP确认库内仍只有种子记录，没有写入或更改。原始本地证据为 `artifacts/host-acceptance/codex-1f0aec3f1dc7424f9a0ca9f58247ed07`；首次审批失败证据保留在 `codex-464a4f2cc0b9490e8dbc56f7fd747970`。

两次均观察到WebSocket连接重试后宿主回退HTTPS；此网络现象不影响第二次只读结果，但属于本机网络/宿主运行条件。现有用户配置还报告了过期字段警告，本轮没有修改它们。

当前桌面对话只有旧工具时应重载MCP或开启新会话，再检查真实工具列表。旧工具不得作为新库失败时的回退；不要为清除旧会话状态而删除真实记忆。

## Hermes握手证据

临时HERMES_HOME只放入合成v1配置，服务使用独立目录。真实宿主输出 `Connected (1703ms)` 和 `Tools discovered: 5`，列出全部五个v1工具。未运行模型任务，因此不标记读写/结果采用通过。

原始本地日志：`artifacts/host-acceptance/hermes-275cf4e4909e44cdb3b780f900df4403/host.log`。首次收集器向GBK控制台打印对勾时发生编码错误；原始UTF-8日志完整，已重新读取核对，不把打印失败误报为MCP失败。隔离服务已停止。

## 验收组件与后续边界

候选二进制来自已通过Windows CI的代码44130cd，用户日常安装未替换。SHA-256：

| 文件 | SHA-256 |
|---|---|
| mcp_server.exe | 0cd594b5e9cdb26b62a578b4b3e82e3debef774f4814bc1d2ab250b636a5f7fe |
| graph-memory-service.exe | 1cb74d87fee142a851dd8bf455765d8a4652deabc907f5bb831b217d2aeb62cd |

L1-07的最低门槛是分层矩阵加至少一个真实新会话闭环，不是所有宿主全部认证。后续需补Codex写入/纠正、桌面重载与Hermes模型闭环；不同宿主版本或权限策略仍需重新验收。下一主线为L2数据导出、备份及恢复。
