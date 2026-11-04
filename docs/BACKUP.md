# 工作库备份与校验

开发版提供 `graph-memory-backup` 命令行工具。服务运行中也可导出整个 v1 工作库；不需要关闭桌面或 Agent。桌面端【数据备份与恢复】面板提供同一套导出/校验/恢复操作（界面在目标文件已存在时先拒绝，恢复需二次确认），无需使用命令行。已支持恢复到**空目录**（不覆盖现有工作库）；校验通过不等于已完成恢复演练。

## 使用

从源码构建（已安装 Rust）：

```powershell
cargo build --manifest-path src-tauri/Cargo.toml --locked --no-default-features --bin graph-memory-backup
```

以下命令从仓库根目录执行。将工作库路径换成要备份的实际目录；输出父目录必须已存在，文件名必须是新的：

```powershell
./src-tauri/target/debug/graph-memory-backup.exe export --data-dir "E:/my-memory/workspace" --output "E:/backups/workspace-2026-11-02.gmbackup"
./src-tauri/target/debug/graph-memory-backup.exe verify "E:/backups/workspace-2026-11-02.gmbackup"
./src-tauri/target/debug/graph-memory-backup.exe restore --input "E:/backups/workspace-2026-11-02.gmbackup" --data-dir "E:/my-memory/restored" --dry-run
./src-tauri/target/debug/graph-memory-backup.exe restore --input "E:/backups/workspace-2026-11-02.gmbackup" --data-dir "E:/my-memory/restored"
```

参数用空格分隔；`--input=FILE` 这类等号形式会被拒绝（退出码 2）。

`scripts/build-desktop.ps1` 生成的新版候选包也包含 `graph-memory-backup.exe`，可从包目录调用。旧安装包不会自动增加此文件。

成功输出 JSON，包括操作、格式版本和项目/记录/修订/创建事件数量；不输出正文。退出码 0 表示成功，1 表示读取、校验或输出失败，2 表示参数错误。没有默认工作库、覆盖开关或自动创建输出父目录的行为。

## 包含什么

备份包含指定工作库的全部项目、个人经验、项目记录、完整修订历史，以及用于创建幂等性的原始请求与响应。ID、版本、来源、适用条件和原案例引用保留；不是当前搜索结果或日期筛选的导出。

不包含旧兼容图数据库、宿主配置/凭据文件、服务发现文件、调用统计、日志或内存中的未保存草稿。正文中原先写入的私人内容仍会原样保留。文件为明文 JSON，未加密、不上传网络；SHA-256 用于检测内容损坏，不证明来源可信。

当前格式上限为 64 MiB（最终编码文件），完整数据在内存中校验。超限明确失败，不截断。格式与字段约束见[格式 v1](design/BACKUP_FORMAT_V1.md)。

## 恢复到新目录

```powershell
./src-tauri/target/debug/graph-memory-backup.exe restore --input "E:/backups/workspace-2026-11-02.gmbackup" --data-dir "E:/my-memory/restored"
```

- `--data-dir` 必须是**已存在且为空**（不含 `work-memory.db`）的目录；目标已有工作库时拒绝，绝不覆盖。原库始终保留，可继续使用。
- 恢复先完整校验备份包（格式版本、数量、摘要、排序、引用关系），校验失败不创建任何文件、不污染目标目录。
- 写入方式：同盘 UUID 暂存目录内建库并同步，再用硬链接无覆盖发布为 `work-memory.db`；失败路径清理暂存文件，目标目录保持原样。
- `--dry-run` 只校验并输出将恢复的计数，不写任何文件；可用于恢复前预演。
- 成功输出 JSON：`{"status":"ok","operation":"restore","format_version":1,"counts":{...},"dry_run":false}`。恢复后可用同一目录启动桌面或服务；也可再 `export` 核对 `payload_sha256` 与原包一致。
- 恢复不是合并：重复导入、把恢复库并回原库、冲突预览仍属后续任务；需要合并时请人工核对记录 ID 与版本。

## 一致性与失败行为

- 通过独立只读 SQLite 连接，在同一个读事务里检查结构并读取四张业务表，包含 WAL 内已经提交的数据；不会把活动中的主数据库文件直接复制成备份。
- 导出反映读取事务建立的快照。之后提交的记录进入下次备份；manifest 的 `exported_at` 是编码时间，不是严格的业务时间截止点。
- 不创建或迁移源数据库，不改变业务记录。SQLite 可能访问 WAL/SHM 支持文件。未知数据库版本、业务表/列、损坏或不完整引用会被拒绝，不静默遗漏未知数据。
- 输出先写入同目录唯一临时文件并同步，再通过硬链接发布新文件。已有文件或发布时出现的同名文件均不覆盖。输出文件系统必须支持硬链接（例如 NTFS）；不支持时安全失败，没有覆盖式回退。
- 普通错误会清理本次临时文件；进程被强制终止可能留下 `.graph-memory-*.tmp`。正式路径不会暴露部分写入文件。不承诺断电后的目录项持久性；未做真实断电或磁盘耗尽实验。
- `verify` 只读取备份，检查格式版本、数量、摘要、排序、完整历史及引用关系，不打开工作库，也不写入或恢复数据。验证失败的输入不会被修改。

恢复到新目录（`restore`）已实现：目标目录必须为空，校验失败不写文件，原库不被改动。冲突预览、把恢复库合并回原库、恢复后的幂等重放仍属后续 L2-03。恢复演练通过前，不要凭校验成功删除原库。
