# P2 发布就绪与桌面数据管理契约（2026-11-04）

基线：`435894f`（P0+P1 已提交，未推送）。本文件是 L2-05 批次唯一契约。目标：普通用户不碰命令行就能备份/校验/恢复；项目能以**一致的版本**打包成可分发产物并自证可用。

## 为什么要做

- P0 提供了恢复 CLI，P1 提供了撤回；但**桌面没有数据管理入口**，用户要自己敲 `graph-memory-backup`，这是目前最大的"不可用"点。
- 发布侧：目前只有 `artifacts/desktop`（debug 二进制拼装），没有 release 构建、没有版本一致性校验、没有校验和、没有可解压即用的分发物。`Cargo.toml` 是 0.2.0，桌面 csproj 没有版本号，MCP `serverInfo.version` 也是 0.2.0——三处必须一致且可验证。

## P2-A 桌面数据管理面板（owner: desktop-ux）

在主窗口新增"数据"入口（建议 `DataPanelButton` 切换 `DataPanel` 可见性），面板内容：

1. **现状**：显示工作库目录与计数（`GET /api/service` 的 `data_dir`；计数可用 `POST /api/v1/search` 的 `total` 或直接展示服务返回信息），并明确提示"导出可在服务运行中执行；恢复只写入空目录，绝不覆盖当前工作库"。
2. **导出备份**：`BackupPathInput`（TextBox）+ `PickBackupPathClick`（文件选择器只负责填入路径）+ `ExportBackupClick`。
   - 调用同目录 `graph-memory-backup.exe export --data-dir <当前库> --output <路径>`；参数用 `ArgumentList`（空格分隔），**禁止** shell 拼接或 `--input=` 等号形式。
   - 目标文件已存在：**UI 自己先检测**并显示冲突提示，不执行导出（CLI 也会拒绝，两者都不允许覆盖）。
   - 成功显示 `status`/`operation`/`format_version`/`counts`；失败显示退出码 + stderr 原文，不静默。
3. **校验备份**：`VerifyPathInput` + `PickVerifyPathClick` + `VerifyBackupClick` → `verify <file>`；显示结果或失败原因。
4. **恢复到新目录**：`RestoreInputPath` + `RestoreTargetDirInput` + `PickRestoreTargetClick` + `RestoreDryRunClick` + `RestoreApplyClick`。
   - `RestoreDryRunClick` → `restore --input F --data-dir D --dry-run`，展示将恢复的 counts。
   - `RestoreApplyClick` → 必须**二次确认**（明确"只写入空目录、不覆盖现有工作库、原库保留"）；确认后执行不带 `--dry-run` 的 restore，显示 JSON 与目标目录。
   - 目标非空或备份损坏：显示 CLI 错误原文，目标目录内容必须保持不变。
5. 服务未连接或当前库目录未知时，相关按钮禁用并给出原因。

硬约束：不重命名既有控件/方法（ClearEditor/ApplyMemory/HasDraft/SwitchScope/SaveClick/RefreshClick/SearchClick/RetractClick/RestoreClick/EvidenceInput 等）；既有 Setup 102 与 Editor 86 检查必须继续通过；`restore` 只允许空目标目录，任何"覆盖/合并/删除当前库"的实现都属违约。

## P2-B 数据管理 headless 测试（owner: desktop-ux，追加式）

`tests/Desktop.Editor.Tests/Program.cs` 允许**追加**新段落（不得修改既有断言）；如控件较多可另建 `tests/Desktop.Data.Tests`，但需先在共享任务里说明。必测（真实 backup CLI + 临时目录 + 真实服务）：

- 导出到新文件：文件存在、`verify` 退出码 0、counts 与库内计数一致；
- 导出到已存在文件：UI 显示冲突且**不覆盖**（文件大小/mtime 前后不变），CLI 未被调用或调用后退出码非 0 且未改动文件；
- 恢复到空目录：退出码 0、目标出现 `work-memory.db`；对恢复目录再 export→verify 计数一致；
- 恢复到非空目录：拒绝，且目标目录原有文件内容不变；
- 损坏备份（截断文件）：verify 失败且 UI 显示错误；
- dry-run：不产生任何文件；
- 空路径/未选文件：给出提示且不崩溃。

## P2-C 发布打包与版本一致性（owner: release-eng）

新增 `scripts/package-release.ps1`：

- release 构建：`cargo build --release --no-default-features`（产出 service/mcp_server/backup 三个 release exe）+ `dotnet publish desktop/GraphMemory.Desktop -c Release -r win-x64 --self-contained true`；支持 `-SkipRust` 复用已有 release 二进制（用于快速重打包）。
- 产物目录 `artifacts/release/graph-memory-<version>-win-x64/`，内含 `GraphMemory.Desktop.exe`、`graph-memory-service.exe`、`mcp_server.exe`、`graph-memory-backup.exe`、`README.md`（从仓库 README 复制或生成的精简版）、`VERSION.txt`、`SHA256SUMS.txt`（对目录内主要文件列出 sha256）。
- 同名 ZIP：`graph-memory-<version>-win-x64.zip`。
- 版本来源单一：`src-tauri/Cargo.toml` 的 `version`；打包脚本必须校验 `desktop/GraphMemory.Desktop/GraphMemory.Desktop.csproj` 的 `<Version>` 与之一致（csproj 需补 `<Version>`），不一致直接失败并报出两处值。

新增 `scripts/verify-release.ps1`：

- 校验产物目录完整性（4 个 exe + VERSION.txt + SHA256SUMS.txt）、`VERSION.txt` 与 Cargo.toml 一致、`SHA256SUMS.txt` 逐项哈希可验证；
- 把 ZIP 解压到全新临时目录，用解压后的 `GraphMemory.Desktop.exe --smoke-test --data-dir <new temp>` 断言 exit 0；
- 用解压后的二进制跑一次真实往返：启动 service → 建项目/写记录/撤回 → `graph-memory-backup export` → `verify` → `restore` 到空目录 → 计数一致（证明发布物本身可用，而非只证明 debug 二进制）；
- 输出 JSON 结论 + 退出码（0 全通过）。

## P2-D 独立验收（owner: verifier2，只读产品代码）

在 `scripts/verify-usability.ps1` 追加 `P2-*` 断言并实跑：

1. **发布物**：`scripts/package-release.ps1` + `scripts/verify-release.ps1` 端到端成功；版本三处一致（Cargo.toml/csproj/VERSION.txt），MCP `serverInfo.version` 与之一致；SHA256SUMS 校验通过；ZIP 解压后 smoke exit 0。
2. **发布物功能**：用解压目录的二进制跑 P0/P1 核心断言（中文检索命中、context 分组、retract/restore、export→verify→restore 往返、旧包兼容）。
3. **数据管理 CLI 契约**（UI 依赖的机制，可黑盒验证）：export 到已存在文件被拒且原文件未改；restore 到非空目录被拒且目标未改；损坏包 verify 失败且退出码非 0；`--dry-run` 不产生文件；含空格与中文的路径可用；服务运行中导出成功且导出后库内容不变。
4. **对抗**：导出过程中并发写入仍产出可 verify 的一致包；恢复后的库再次导出 `payload_sha256` 与源包一致；恢复目标目录里出现无关文件时不覆盖它们（应直接拒绝）。
5. **回归**：Rust、Setup 102、Editor（86+新增）、Python 两套件、smoke 全绿；`scripts/verify-usability.ps1 -Full` 退出码 0。

## 明确不做

自动更新/升级器、安装程序（MSI/Inno）、代码签名、云端同步、跨平台打包、合并式恢复（把备份并回现有库）——后者仍是 L2-03 剩余项，恢复语义保持"只写空目录"。

## 环境

cargo 在 PATH；.NET 10 SDK 在 `%USERPROFILE%\.dotnet`（不在 PATH，需 `DOTNET_ROOT`+`PATH`）；PowerShell 5.1（写 `.ps1` 必须带 UTF-8 BOM；向子进程 stdin 发中文前设 `[Console]::InputEncoding`，或只用 ASCII 载荷）；端口随机、跑完 `netstat` 确认无 LISTENING；测试服务进程必须清理。
