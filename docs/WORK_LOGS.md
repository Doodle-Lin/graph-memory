# 跨工具工作日志（首版，2026-11-17）

目标：多轮协作产生实际进展后，由Agent主动沉淀有日期、项目与细节的工作日志；任何连接同一工作库的新会话，可按天、周或任意日期范围读取材料并回答。

## 使用前提

各宿主都应连接同一个新版Graph Memory服务，重载MCP工具定义，并采用[Agent指令](../desktop/AGENT_INSTRUCTIONS.md)中的“自动工作日志与跨工具复盘”。接入MCP本身不授予服务观察完整聊天的能力；自动调用取决于宿主是否遵守指令、允许工具写入。没有调用证据时不能说“已自动记录”。不扫描历史聊天，不改写宿主配置，不默默把原始对话存入库。

先在桌面创建项目，再用memory_projects确认ID。现有MCP仍只有五工具，尚不能自动创建项目。跨工具需共享工作库，远程机器指向不同本地目录不会自动同步。

## 写入与纠正

memory_write示例（project_id用真实ID）：

```json
{
  "memory_scope": "project",
  "project_id": "实际项目ID",
  "event_id": "会话稳定标识/任务标识/2026-11-17/阶段1",
  "kind": "work_log",
  "title": "解决部署失败并完成隔离回归",
  "content": "排查到配置路径错误；修正路径并重建候选包；完成隔离回归。生产环境尚未部署。具体文件、命令结果和产物路径应按实际情况补充。",
  "source": "实际宿主与会话/提交/报告位置",
  "evidence_type": "tool_result",
  "work_log": {
    "date": "2026-11-17",
    "utc_offset": "+08:00",
    "status": "in_progress",
    "outcome": "隔离回归通过，生产部署待执行",
    "next_steps": ["核对部署窗口"]
  }
}
```

字段均必填；日期为严格YYYY-MM-DD，表示记录者所在地的工作日，精度为天。utc_offset记录该日偏移，不是IANA时区；跨时区汇总按每条声明的当地工作日分组，不自动将整日记录转换成另一时区的精确时间。status可选in_progress/completed/blocked/planned；outcome不得为空。项目由已有project_id关联，content保留具体行动细节，source保留可追溯依据。

同一事项当天继续推进：memory_update携带id、expected_version、完整content；省略work_log保留原值，提供时必须传完整对象。跨天工作追加日志。纠正后历史版本仍保留；撤回默认不再出现在搜索中。不能用后来“完成”状态改写前一天真实的阻塞事实。

## 日报、周报与任意日期问答

memory_search示例：

```json
{"scope":"all","kind":"work_log","date_field":"work_date","from":"2026-11-16","to":"2026-11-23","limit":100,"offset":0}
```

区间左闭右开；示例包含16日至22日。单项目用scope=project+project_id；个人用personal。query可省略避免漏掉不同措辞的工作。按next_offset翻页至null，核对total并按ID去重。无query时按工作日期倒序；有query时相关度优先、工作日期次之。分页不是固定快照，持续写入时需说明可能变化。

工作日期与created_at/updated_at独立：旧日工作今天补记，或今天补充细节，仍可查到原工作日期。旧记录没有work_log时视为日期未知，不凭入库日补造。date_field=work_date只接受日期字符串；其他字段继续使用带时区RFC3339。

报告输出按日期、项目列出具体行动、结果、状态、下一步和ID/版本/来源；计划与已完成分开，未知日期/无记录期间说明覆盖缺口。返回最新纠正版本，尚非“当时看到的历史快照”。不能从没有日志推断没有工作。

## 兼容与验收边界

元数据存入现有body JSON，不新增业务表；新版保留旧记录，备份/恢复校验并原样保留元数据。使用日志后应统一升级服务与MCP，避免旧服务在更新时丢弃不认识的字段；现有用户安装未在本批自动升级。桌面可查看/编辑正文并保留元数据，专门的日志表单与日期筛选尚未提供。

真实MCP新进程测试见tests/test_work_logs.py，覆盖日期区间、闰日、迟到纠正、未知日期、分页、个人/项目范围、输入拒绝及备份恢复。另通过真实Claude多轮主动写入、新Claude会话与Codex跨宿主期间检索样本，详见[分层宿主证据](HOST_COMPATIBILITY.md)。该样本不代表所有模型必然主动调用，Codex写入与Hermes采用尚未验证。完整事件变更时间线、稳定快照分页、项目自动匹配/创建、跨设备同步仍是后续工作。
