# 工作库导出格式 v1

状态：L2-01纯序列化契约。实现为 `src-tauri/src/backup_format.rs`。此模块不读取数据库、不写文件、不恢复数据；一致性快照/CLI属于L2-02，副本恢复属于L2-03。禁止仅复制活动中的SQLite主文件作为完成的备份。

## 范围与保留原则

本格式面向整个v1工作库的逻辑数据：项目、当前记录、全部修订、创建幂等事件。记录ID、版本、项目归属、来源、适用条件、原案例引用、创建/更新时间全部保留，不重新生成。未来导出调用方必须保证来自同一个数据库快照，序列化器本身不能证明数据库采集的一致性或完整性。

仅包含四张业务表的明确字段。排除 `agent_activity` 调用统计、service.json实例发现、PID/端口、锁文件、宿主MCP配置、登录凭据、环境变量、旧图谱库、模型缓存及未保存桌面草稿。项目directory是元数据，可能是旧机器路径；恢复不能自动创建该目录或导入其内容。

**敏感内容边界：**不读取凭据文件不等于导出包没有秘密。用户或Agent曾写入正文、source、conditions、目录或请求额外字段的私人内容仍会原样包含。不能为了脱敏悄悄改写可恢复备份；脱敏分享应另设格式。本格式为明文，不提供加密、签名或来源认证，不上传网络。

## 文件结构

建议扩展名 `.gmbackup`，UTF-8 JSON。顶层仅有manifest和payload，缺少必需字段、重复已知JSON字段、未知外层/行字段或未知format/version都拒绝，不静默降级。body/request/response是原始JSON文本字符串，其内容另行校验但不重写。

| manifest字段 | 类型 / 语义 |
|---|---|
| format | 固定 `graph-memory-workspace` |
| format_version | 固定整数1；未知版本拒绝，未来迁移须显式实现 |
| exported_at | 带时区RFC3339，采集端提供，不是记忆事件发生时间 |
| counts | projects、memories、revisions、events四个非负整数，必须与数组长度一致 |
| payload_sha256 | 64位小写十六进制SHA-256；只覆盖payload规范序列化字节 |

| payload数组 | 字段（序列化声明顺序） | 对应表 / 主键 |
|---|---|---|
| projects | id、name、directory、created_at（均字符串） | work_projects / id |
| memories | id、memory_scope、project_id（字符串或null）、body（字符串） | work_memories / id |
| revisions | memory_id（字符串）、version（正整数）、body（字符串） | work_revisions / memory_id+version |
| events | event_id、request、response（均字符串） | work_events / event_id |

payload自身的字段顺序固定为projects、memories、revisions、events。原始行文本中的未知附加字段仍保留，不因当前类型未识别而丢失；已知记忆字段按当前Memory结构检查类型。请求中服务原先接受但未采用的附加字段也保留，因为创建重试比较完整请求字符串。

## 排序、摘要和容量

编码前按项目ID、记录ID、(memory_id, version数值)、event_id分别排序。字符串按UTF-8字典序，不做大小写、Unicode或路径归一化。解码要求数组已严格排序，重复主键拒绝。

摘要算法：先以表中声明的字段顺序紧凑序列化已排序的Snapshot，按serde_json的字符串转义规则输出UTF-8、不添加空格，然后SHA-256取小写hex。不是RFC8785通用JSON规范化；其他语言实现需要对照格式样本。原始body/request/response字符串内部的空白、键顺序和字节内容不能改写。外层JSON缩进/键顺序改变不影响摘要；exported_at不在摘要覆盖范围内。

空payload的摘要输入为：

```json
{"projects":[],"memories":[],"revisions":[],"events":[]}
```

上述空payload的摘要为 `2041f17a0ed4b990bda9b60cbc85960201f42f87dead4eb3d28aebb24f6dcada`，测试中的固定值由独立Python hashlib计算。

解码和编码后的完整文件上限均为64MiB，超限明确报错，不截断。当前实现内存中处理完整文件，不承诺流式或超大库；后续扩容需单独的资源边界验证。

SHA-256用于检测意外损坏，不能阻止攻击者修改数据后重算摘要。解码仍必须执行以下关系校验；即便校验通过，也不代表正文是真实事实或可信指令。

## 必须成立的关系

- 项目ID非空且唯一，非空directory唯一，名称非空，项目和记忆时间为RFC3339。保留原始时间，不按当前时区转换；不假设系统时钟永远单调。
- 当前行索引的id/scope/project_id与body一致。个人记录没有project_id，项目记录引用存在的项目。非空origin_memory_id必须引用包内存在的记录。
- 修订必须属于存在的记录，行memory_id/version与body一致；版本为可存入SQLite INTEGER的正整数。
- 每条记录的历史恰好覆盖1..当前版本，不能有缺口、重复或未来版本。最新修订与当前body的JSON值相等。
- 修订的项目归属、创建时间、原案例引用和依据类型与当前记录保持一致；正文、标题、类型、来源、条件和更新时间可随版本变化。
- 每条记录恰有一个创建事件。event_id与原始请求的event_id去两侧空白后相等；response.memory必须等于版本1，不能误用最新版本。
- 原请求scope、project_id、去两侧空白后的content必须对应版本1。事件引用不存在的记录、重复创建事件、缺失事件均拒绝。

校验错误返回类别，不回显正文或原始请求。包内文本始终作为数据，不执行其中的命令或指令。

## 接口与验收

`encode(Snapshot, exported_at) -> Result<Vec<u8>, String>`：校验、排序并生成完整包。

`decode(&[u8]) -> Result<Snapshot, String>`：检查大小、结构、版本、摘要、顺序及关系，返回无损行数据。L2-02已提供[导出与校验CLI](../BACKUP.md)，恢复CLI仍待L2-03。

独立测试从真实MemoryStore API创建合成SQLite：中文及emoji长文、空kind、自定义kind、项目原案例、纠正和撤销至v3、原请求未知字段及带空格event_id。提取测试行后编码/解码，检查字段和事件字符串逐字节一致；这不是生产数据库快照或恢复实现。

损坏测试覆盖未知版本、未知字段、错数量、坏摘要、截断、超限、乱序、重复主键、缺项目、缺历史、缺事件、索引与body不一致；关系错误即便重算正确摘要也拒绝。活动写入一致性与输出失败保护由L2-02验证；完整SQL恢复后幂等重放及恢复回滚留待L2-03验收。
