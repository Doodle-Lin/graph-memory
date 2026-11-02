# 无窗口记忆服务

Avalonia 与 MCP 共用此服务，使用方法及边界见 [桌面说明](../desktop/README.md)。当前为可运行开发版，不表示产品设计全部验收完成。

```powershell
cargo build --manifest-path src-tauri/Cargo.toml --no-default-features --bin graph-memory-service --bin mcp_server
./src-tauri/target/debug/graph-memory-service.exe --data-dir "$env:TEMP/graph-memory-demo"
```
必须指定目录；默认随机本机端口，可传 --port。SQLite 独占锁避免同目录多实例，崩溃后自动释放。不下载模型、不注册自启、不导入个人文件、不自动遗忘。

service.json 提供实例、协议、PID、URL、目录，客户端通过 GET /api/service 握手。MCP 设置 GM_MEMORY_V1=1、GM_SERVICE_DIR、GM_SERVICE_EXE 后使用新工具，并按需启动服务；GM_API_URL 保留显式地址方式。

work-memory.db 保存项目、project/personal 范围记忆、修订历史和幂等事件。v1 接口包括 projects、memories、search、context、memories/update、memories/undo、memories/history。更新校验 expected_version，创建要求 event_id。重复事件同内容重放原结果，不同内容报冲突。

无项目只返回个人经验；有项目只返回本项目与个人经验。上下文保留来源、条件、版本、原案例引用。复制时重新取最新数据。

旧兼容接口使用 service-graph.db，不接触旧 graph.db 或旧全局端口文件。只开启必要的 health/stats/graph/recent/model_status/write/retrieve。新旧模型不自动互通。静态页面及未开放旧接口返回404。

只绑定127.0.0.1，拒绝浏览器Origin请求，尚无完整本机进程身份鉴权。关键词检索，不加载embedding。

## MCP 主流程与调用证据

新 MCP 工具通过 /api/v1/agent/call 分发，/api/v1/agent/status 返回每种工具的累计调用、成功次数、最近结果与时间。桌面普通浏览不计为 MCP 调用；调用统计不代表宿主身份或 Agent 采用结果。初始化响应携带记忆使用约定，宿主行为仍需配置指令。失败调用不会增加成功次数；幂等重放算一次成功调用，不等于新增记忆。

上下文最多返回最近 40 条项目记录与 20 条个人经验，超限显式返回 truncated 和总数，正文提示需补充检索。列表搜索目前仍为全表关键词检索，尚未实现大数据量分页。
