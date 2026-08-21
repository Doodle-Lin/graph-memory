// Graph Memory — MCP stdio server (Rust 版)
//
// 独立二进制:agent 通过 stdio 拉起它,它把请求 HTTP 代理到常驻的桌面引擎进程。
// 镜像 Python graph_memory/mcp_server.py 的设计:
//   - 不持有图,只做 HTTP 转发(避免与桌面进程持有两份不一致的图数据)
//   - 5 个工具: retrieve / write / extract / update / recent
//
// 端口发现:读 ~/.graph-memory/port 文件(桌面进程启动时写入)。
// 也可用 GM_API_URL 环境变量直接指定 http://host:port。
//
// 配置进 agent(以 Claude Code / Codex 为例):
//   mcp_servers:
//     graph-memory:
//       command: "graph-memory-mcp"
//       args: []
//       env:
//         GM_API_URL: "http://127.0.0.1:9121"
//
// JSON-RPC 2.0 over stdio,兼容 MCP 协议。

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const PROTOCOL_VERSION: &str = "2024-11-05";

fn api_base() -> String {
    if let Ok(u) = std::env::var("GM_API_URL") {
        if !u.is_empty() { return u.trim_end_matches('/').to_string(); }
    }
    // 读端口发现文件
    if let Some(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok() {
        let port_file = std::path::PathBuf::from(&home).join(".graph-memory/port");
        if let Ok(port) = std::fs::read_to_string(&port_file) {
            let port = port.trim();
            if !port.is_empty() {
                return format!("http://127.0.0.1:{}", port);
            }
        }
    }
    "http://127.0.0.1:9121".to_string()
}

fn http_post_json(path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", api_base(), path);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build().map_err(|e| e.to_string())?;
    let resp = client.post(&url)
        .header("Content-Type", "application/json")
        .json(body)
        .send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status, &text[..text.len().min(200)]));
    }
    serde_json::from_str(&text).map_err(|e| format!("json: {} :: {}", e, &text[..text.len().min(200)]))
}

fn http_get(path: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", api_base(), path);
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build().map_err(|e| e.to_string())?;
    let resp = client.get(&url).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    let text = resp.text().map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {}: {}", status, &text[..text.len().min(200)]));
    }
    serde_json::from_str(&text).map_err(|e| format!("json: {} :: {}", e, &text[..text.len().min(200)]))
}

fn tools_list() -> serde_json::Value {
    serde_json::json!([{
        "name": "retrieve",
        "description": "检索知识图谱中的关联知识。输入关键词或问题,返回通过 embedding 语义匹配 + Personalized PageRank 图扩散找到的关联知识节点。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "检索关键词或问题"},
                "top_k": {"type": "integer", "description": "返回结果数(默认5)", "default": 5}
            },
            "required": ["query"]
        }
    }, {
        "name": "write",
        "description": "将新知识写入知识图谱。自动找已有知识中的关联节点并建边。三层去重:完全相同→合并,embedding相似度>0.85→合并,否则新建。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "content": {"type": "string", "description": "知识内容(保留技术细节)"},
                "title": {"type": "string", "description": "简短标题(可选)"},
                "node_type": {"type": "string", "description": "knowledge/preference/project/fact/skill/reference", "default": "knowledge"},
                "source": {"type": "string", "description": "来源标识", "default": "agent"}
            },
            "required": ["content"]
        }
    }, {
        "name": "extract",
        "description": "用 LLM 从一段对话/文本中提炼知识并写入图。自动过滤噪声,提取结构化知识节点+关系。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "待提炼的文本(对话/记忆)"},
                "source": {"type": "string", "description": "来源标识", "default": "agent"}
            },
            "required": ["text"]
        }
    }, {
        "name": "update",
        "description": "修正/更新已有知识节点。通过关键词找到匹配节点并更新;找不到则创建新节点。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "用来查找要更新的节点的关键词"},
                "new_content": {"type": "string", "description": "更新后的完整内容"},
                "new_title": {"type": "string", "description": "更新后的标题(可选)"},
                "node_type": {"type": "string", "description": "knowledge/preference/project/fact/skill/reference"}
            },
            "required": ["query", "new_content"]
        }
    }, {
        "name": "recent",
        "description": "获取最近添加的知识节点。",
        "inputSchema": {
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "description": "返回条数(默认20)", "default": 20}
            }
        }
    }])
}

fn handle_tool_call(name: &str, args: &serde_json::Value) -> Result<Vec<serde_json::Value>, String> {
    let text_content = |t: String| vec![serde_json::json!({"type": "text", "text": t})];

    match name {
        "retrieve" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let top_k = args.get("top_k").and_then(|v| v.as_u64()).unwrap_or(5);
            let result = http_post_json("/api/retrieve", &serde_json::json!({
                "query": query, "top_k": top_k, "spread": true
            }))?;
            let arr = result.as_array().cloned().unwrap_or_default();
            let mut lines = vec![format!("检索 '{}' 返回 {} 条关联知识:\n", query, arr.len())];
            for (i, r) in arr.iter().enumerate() {
                lines.push(format!("--- {} ---", i + 1));
                lines.push(format!("标题: {}", r.get("title").and_then(|v| v.as_str()).unwrap_or("?")));
                let score = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
                lines.push(format!("类型: {}  分数: {:.3}",
                    r.get("node_type").and_then(|v| v.as_str()).unwrap_or("?"), score));
                lines.push(format!("内容: {}", r.get("content").and_then(|v| v.as_str()).unwrap_or("")));
                lines.push(String::new());
            }
            Ok(text_content(lines.join("\n")))
        }
        "write" => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() { return Err("content 不能为空".into()); }
            let result = http_post_json("/api/write", &serde_json::json!({
                "content": content,
                "title": args.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                "node_type": args.get("node_type").and_then(|v| v.as_str()).unwrap_or("knowledge"),
                "source": args.get("source").and_then(|v| v.as_str()).unwrap_or("agent"),
                "auto_link": true, "max_links": 5
            }))?;
            let title = result["node"]["title"].as_str().unwrap_or("?");
            let links = result["auto_links"].as_array().map(|a| a.len()).unwrap_or(0);
            Ok(text_content(format!("已写入知识节点: {}\n自动建立 {} 条关联", title, links)))
        }
        "extract" => {
            let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
            if text.is_empty() { return Err("text 不能为空".into()); }
            let result = http_post_json("/api/extract", &serde_json::json!({
                "text": text, "source": args.get("source").and_then(|v| v.as_str()).unwrap_or("agent"), "max_links": 5
            }))?;
            let nodes = result["imported"]["nodes_created"].as_u64().unwrap_or(0);
            let edges = result["imported"]["edges_created"].as_u64().unwrap_or(0);
            let err = result["extracted"]["error"].as_str();
            let msg = if let Some(e) = err { format!("LLM 提炼失败: {}", e) }
                else { format!("LLM 提炼完成: {} 个知识节点, {} 条关系", nodes, edges) };
            Ok(text_content(msg))
        }
        "update" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let new_content = args.get("new_content").and_then(|v| v.as_str()).unwrap_or("");
            if query.is_empty() || new_content.is_empty() { return Err("query 和 new_content 不能为空".into()); }
            let result = http_post_json("/api/update", &serde_json::json!({
                "query": query,
                "new_content": new_content,
                "new_title": args.get("new_title").and_then(|v| v.as_str()).unwrap_or(""),
                "node_type": args.get("node_type").and_then(|v| v.as_str()).unwrap_or("")
            }))?;
            let action = result.get("action").and_then(|v| v.as_str()).unwrap_or("?");
            let msg = result.get("message").and_then(|v| v.as_str()).unwrap_or("");
            Ok(text_content(format!("已{}知识节点: {}", action, msg)))
        }
        "recent" => {
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20);
            let result = http_get(&format!("/api/recent?limit={}", limit))?;
            let arr = result.as_array().cloned().unwrap_or_default();
            let mut lines = vec![format!("最近 {} 条知识:\n", arr.len())];
            for n in arr {
                lines.push(format!("[{}] {}", n.get("node_type").and_then(|v| v.as_str()).unwrap_or("?"),
                    n.get("title").and_then(|v| v.as_str()).unwrap_or("?")));
                let content = n.get("content").and_then(|v| v.as_str()).unwrap_or("");
                lines.push(format!("  {}", &content[..content.len().min(100)]));
                lines.push(String::new());
            }
            Ok(text_content(lines.join("\n")))
        }
        other => Err(format!("未知工具: {}", other)),
    }
}

fn make_result(id: &serde_json::Value, result: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

fn make_error(id: &serde_json::Value, code: i64, message: &str) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn main() {
    let _ = env_logger::try_init();
    log::info!("Graph Memory MCP Server starting, api_base={}", api_base());
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let initialized = Arc::new(AtomicBool::new(false));

    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = msg.get("method").and_then(|v| v.as_str()).unwrap_or("");

        let out = match method {
            "initialize" => {
                initialized.store(true, Ordering::Relaxed);
                make_result(&id, serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "graph-memory", "version": "0.2.0"}
                }))
            }
            "initialized" | "notifications/initialized" => { continue; }
            "tools/list" => make_result(&id, serde_json::json!({"tools": tools_list()})),
            "tools/call" => {
                let name = msg.get("params").and_then(|p| p.get("name")).and_then(|v| v.as_str()).unwrap_or("");
                let args = msg.get("params").and_then(|p| p.get("arguments")).cloned().unwrap_or(serde_json::json!({}));
                match handle_tool_call(name, &args) {
                    Ok(content) => make_result(&id, serde_json::json!({"content": content})),
                    Err(e) => make_result(&id, serde_json::json!({
                        "content": [{"type": "text", "text": format!("错误: {}", e)}],
                        "isError": true
                    })),
                }
            }
            "ping" => make_result(&id, serde_json::json!({})),
            _ => make_error(&id, -32601, &format!("method not found: {}", method)),
        };

        if writeln!(stdout, "{}", out).is_err() { break; }
        let _ = stdout.flush();
    }
    log::info!("Graph Memory MCP Server exiting");
}
