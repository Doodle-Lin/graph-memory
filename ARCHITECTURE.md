# Graph Memory 架构入口

当前默认产品为 Avalonia 桌面 + v1 MCP + 本机 Rust 服务，共用独立工作库。桌面负责接入与人工核查，Agent 通过五个 v1 工具检索、沉淀和纠正。

- [使用与构建](README.md)
- [服务协议、实例发现与数据边界](docs/HEADLESS_SERVICE.md)
- [桌面行为与当前限制](desktop/README.md)
- [长期规划](docs/design/LONG_TERM_PLAN.md)与[实际验收台账](docs/design/EXECUTION_LEDGER.md)

原 Tauri、向量图谱及七工具架构见[旧架构归档](docs/legacy/ARCHITECTURE.md)。旧架构的优化成果不代表 v1 已实现；两种数据模型不自动互通。
