// 极简静态文件服务器 + 引擎 HTTP API + 模型下载状态/进度
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::engine::GraphEngine;

#[derive(Clone)]
pub struct ModelState {
    pub downloading: Arc<AtomicBool>,
    pub done: Arc<AtomicBool>,
    pub error: Arc<Mutex<Option<String>>>,
    pub embedder_ready: Arc<AtomicBool>,
    pub downloaded_bytes: Arc<AtomicU64>,
    pub total_bytes: Arc<AtomicU64>,
    pub current_file: Arc<Mutex<String>>,
}

impl ModelState {
    pub fn new() -> Self {
        Self {
            downloading: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
            error: Arc::new(Mutex::new(None)),
            embedder_ready: Arc::new(AtomicBool::new(false)),
            downloaded_bytes: Arc::new(AtomicU64::new(0)),
            total_bytes: Arc::new(AtomicU64::new(0)),
            current_file: Arc::new(Mutex::new(String::new())),
        }
    }
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

pub fn check_model_cached_internal() -> bool {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let cache = std::path::PathBuf::from(&home)
        .join(".cache/fastembed").join(MODEL_DIR_NAME);
    // 检查 onnx blob 文件存在且 > 50MB
    let onnx_blob = cache.join("blobs/3280a4617d739df620c32616908400ea249a34f1");
    onnx_blob.exists() && std::fs::metadata(&onnx_blob).map(|m| m.len() > 50_000_000).unwrap_or(false)
}

/// fastembed 缓存目录 (HF hub 格式)
fn fastembed_cache_dir() -> PathBuf {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    PathBuf::from(home).join(".cache/fastembed").join(MODEL_DIR_NAME)
}

/// 需要下载的文件列表: (local_path, remote_path, blob_hash)
/// BAAI/bge-small-zh-v1.5 via Xenova/bge-small-zh-v1.5
const MODEL_FILES: &[(&str, &str, &str)] = &[
    ("config.json", "config.json", "d590a6643f5dcdd0cfb0477e3c291488c16fc2d7"),
    ("tokenizer_config.json", "tokenizer_config.json", "3a59388f0fd1bd22dec2ce7902c1be8e1fb84107"),
    ("special_tokens_map.json", "special_tokens_map.json", "a8b3208c2884c4efb86e49300fdd3dc877220cdf"),
    ("tokenizer.json", "tokenizer.json", "cdb3043fc938fc918c06e66cf704c2ba58f88747"),
    ("onnx/model.onnx", "onnx/model.onnx", "3280a4617d739df620c32616908400ea249a34f1"),
];

const COMMIT_HASH: &str = "75c43b069aac4d136ba6bc1122f995fedcfd2781";
const HF_MIRROR: &str = "https://hf-mirror.com/Xenova/bge-small-zh-v1.5/resolve/main";
const MODEL_DIR_NAME: &str = "models--Xenova--bge-small-zh-v1.5";

/// 手动下载模型文件到 hf-hub 缓存目录格式(blobs + snapshots symlink)
pub fn download_model(state: &ModelState) {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    let cache = PathBuf::from(home).join(".cache/fastembed").join(MODEL_DIR_NAME);
    let blobs = cache.join("blobs");
    let snapshots = cache.join("snapshots").join(COMMIT_HASH);

    std::fs::create_dir_all(&blobs).ok();
    std::fs::create_dir_all(&snapshots).ok();
    std::fs::create_dir_all(snapshots.join("onnx")).ok();
    // 写 refs/main
    let refs = cache.join("refs");
    std::fs::create_dir_all(&refs).ok();
    std::fs::write(refs.join("main"), COMMIT_HASH).ok();

    let mut downloaded: u64 = 0;
    for (local, remote, blob_hash) in MODEL_FILES {
        let blob_path = blobs.join(blob_hash);
        let snap_path = snapshots.join(local);

        // 检查 blob 是否已存在
        let blob_exists = blob_path.exists() && std::fs::metadata(&blob_path).map(|m| m.len() > 0).unwrap_or(false);
        if !blob_exists {
            *state.current_file.lock().unwrap() = remote.to_string();
            let url = format!("{}/{}", HF_MIRROR, remote);

            // 流式下载到 blob 文件
            match reqwest::blocking::get(&url) {
                Ok(resp) => {
                    let f = match std::fs::File::create(&blob_path) {
                        Ok(f) => f,
                        Err(e) => {
                            *state.error.lock().unwrap() = Some(format!("Create blob {} failed: {}", blob_hash, e));
                            state.downloading.store(false, Ordering::Relaxed);
                            return;
                        }
                    };
                    let mut writer = std::io::BufWriter::new(f);
                    let mut reader = std::io::BufReader::new(resp);
                    let mut buf = [0u8; 262144];
                    let mut file_size: u64 = 0;
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                if std::io::Write::write_all(&mut writer, &buf[..n]).is_err() {
                                    *state.error.lock().unwrap() = Some(format!("Write blob {} failed", blob_hash));
                                    state.downloading.store(false, Ordering::Relaxed);
                                    return;
                                }
                                file_size += n as u64;
                                downloaded += n as u64;
                                state.downloaded_bytes.store(downloaded, Ordering::Relaxed);
                            }
                            Err(e) => {
                                *state.error.lock().unwrap() = Some(format!("Read {} failed: {}", remote, e));
                                state.downloading.store(false, Ordering::Relaxed);
                                return;
                            }
                        }
                    }
                    drop(writer);
                    log::info!("Downloaded {} -> blob {} ({} MB)", remote, &blob_hash[..8], file_size / 1024 / 1024);
                }
                Err(e) => {
                    *state.error.lock().unwrap() = Some(format!("Download {} failed: {}", remote, e));
                    state.downloading.store(false, Ordering::Relaxed);
                    return;
                }
            }
        } else {
            // blob 已存在,跳过
            if let Ok(meta) = std::fs::metadata(&blob_path) {
                downloaded += meta.len();
                state.downloaded_bytes.store(downloaded, Ordering::Relaxed);
            }
        }

        // 创建 symlink (Windows: 用 copy 代替 symlink)
        let parent = snap_path.parent().unwrap();
        std::fs::create_dir_all(parent).ok();
        if !snap_path.exists() {
            // Windows 不支持普通用户创建 symlink,直接复制
            std::fs::copy(&blob_path, &snap_path).ok();
        }
    }

    log::info!("All model files downloaded to hf-hub cache format!");
    state.done.store(true, Ordering::Relaxed);
}

/// 引擎句柄:HTTP server 线程持有 GraphEngine,通过 /api/* 暴露给 MCP server / 前端
#[derive(Clone)]
pub struct EngineHandle {
    pub engine: Arc<Mutex<GraphEngine>>,
}

fn send_json(stream: &mut TcpStream, json: serde_json::Value) {
    let body = json.to_string();
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn send_err(stream: &mut TcpStream, status: u16, msg: &str) {
    let body = serde_json::json!({"error": msg}).to_string();
    let header = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
        status, body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// 从请求里解析 JSON body(找 \r\n\r\n 之后的内容)
fn parse_json_body(req: &str) -> serde_json::Value {
    if let Some(pos) = req.find("\r\n\r\n") {
        let body = &req[pos + 4..];
        return serde_json::from_str(body).unwrap_or(serde_json::json!({}));
    }
    serde_json::json!({})
}

fn parse_query_int(q: &str, key: &str, default: usize) -> usize {
    for pair in q.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            return kv.next().and_then(|v| v.parse().ok()).unwrap_or(default);
        }
    }
    default
}

fn parse_query_str<'a>(q: &'a str, key: &str, default: &'a str) -> &'a str {
    for pair in q.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            return kv.next().unwrap_or(default);
        }
    }
    default
}

/// 处理引擎 API。返回 true 表示命中并已响应,false 表示不是引擎 API(走静态文件)。
fn handle_engine_api(
    stream: &mut TcpStream, method: &str, clean: &str, raw_query: &str,
    body: serde_json::Value, eng: &EngineHandle,
) -> bool {
    let mut e = eng.engine.lock().unwrap();

    if clean == "/api/health" && method == "GET" {
        send_json(stream, serde_json::json!({
            "status": "ok",
            "nodes": e.stats()["node_count"]
        }));
        return true;
    }
    if clean == "/api/stats" && method == "GET" {
        send_json(stream, e.stats());
        return true;
    }
    if clean == "/api/graph" && method == "GET" {
        send_json(stream, e.graph_snapshot());
        return true;
    }
    if clean == "/api/recent" && method == "GET" {
        let limit = parse_query_int(raw_query, "limit", 20);
        send_json(stream, serde_json::to_value(e.recent(limit)).unwrap());
        return true;
    }
    // GET /api/search?q=...&top_k=...  (检索的 GET 版,spread=false,语义近邻)
    if clean == "/api/search" && method == "GET" {
        let q = parse_query_str(raw_query, "q", "").to_string();
        let top_k = parse_query_int(raw_query, "top_k", 15);
        match e.retrieve(&q, Some(top_k), false) {
            Ok(r) => send_json(stream, serde_json::to_value(r).unwrap()),
            Err(err) => send_err(stream, 500, &err.to_string()),
        }
        return true;
    }
    // DELETE /api/nodes/{id}
    if clean.starts_with("/api/nodes/") && method == "DELETE" {
        let id = clean.trim_start_matches("/api/nodes/");
        if e.delete_node(id) {
            send_json(stream, serde_json::json!({"ok": true}));
        } else {
            send_err(stream, 404, &format!("node not found: {}", id));
        }
        return true;
    }
    // GET /api/neighbors/{id}?depth=N  (BFS 找邻居,供前端展开)
    if clean.starts_with("/api/neighbors/") && method == "GET" {
        let id = clean.trim_start_matches("/api/neighbors/");
        let depth = parse_query_int(raw_query, "depth", 1);
        send_json(stream, e.neighbors(id, depth));
        return true;
    }
    if clean == "/api/retrieve" && method == "POST" {
        let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let top_k = body.get("top_k").and_then(|v| v.as_u64()).map(|x| x as usize);
        let spread = body.get("spread").and_then(|v| v.as_bool());
        match e.retrieve(&query, top_k, spread.unwrap_or(true)) {
            Ok(r) => send_json(stream, serde_json::to_value(r).unwrap()),
            Err(e) => send_err(stream, 500, &e.to_string()),
        }
        return true;
    }
    if clean == "/api/write" && method == "POST" {
        let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let title = body.get("title").and_then(|v| v.as_str()).unwrap_or("");
        let nt = body.get("node_type").and_then(|v| v.as_str()).unwrap_or("knowledge");
        let source = body.get("source").and_then(|v| v.as_str()).unwrap_or("agent");
        let auto_link = body.get("auto_link").and_then(|v| v.as_bool()).unwrap_or(true);
        let max_links = body.get("max_links").and_then(|v| v.as_u64()).map(|x| x as usize).unwrap_or(5);
        match e.add_node(content, title, nt, source, "{}") {
            Ok(node) => {
                let links = if auto_link { e.auto_link(&node.id, max_links).unwrap_or_default() } else { Vec::new() };
                send_json(stream, serde_json::json!({
                    "node": node,
                    "auto_links": serde_json::to_value(links).unwrap(),
                }));
            }
            Err(e) => send_err(stream, 500, &e.to_string()),
        }
        return true;
    }
    if clean == "/api/import" && method == "POST" {
        let source = parse_query_str(raw_query, "source", "all").to_string();
        let enrich = e.embedder_ready();
        let result = match source.as_str() {
            "all" => crate::importer::import_all_with_opts(&mut e, enrich),
            "hermes" => {
                let r = crate::importer::import_hermes(&mut e);
                let mut j = serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "message": format!("导入 {} 个节点", r.total)});
                if enrich {
                    if let Ok((embs, edges)) = e.enrich_all() {
                        j["enrich"] = serde_json::json!({"embeddings": embs, "edges": edges});
                    }
                }
                j
            }
            "claude" => {
                let r = crate::importer::import_claude(&mut e);
                let mut j = serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "message": format!("导入 {} 个节点", r.total)});
                if enrich {
                    if let Ok((embs, edges)) = e.enrich_all() {
                        j["enrich"] = serde_json::json!({"embeddings": embs, "edges": edges});
                    }
                }
                j
            }
            "codex" => {
                let r = crate::importer::import_codex(&mut e);
                let mut j = serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "message": format!("导入 {} 个节点", r.total)});
                if enrich {
                    if let Ok((embs, edges)) = e.enrich_all() {
                        j["enrich"] = serde_json::json!({"embeddings": embs, "edges": edges});
                    }
                }
                j
            }
            other => {
                send_err(stream, 400, &format!("unknown source: {}", other));
                return true;
            }
        };
        send_json(stream, result);
        return true;
    }
    // POST /api/enrich —— 手动触发补全 embedding + auto_link(模型就绪后调用)
    if clean == "/api/enrich" && method == "POST" {
        match e.enrich_all() {
            Ok((embs, edges)) => send_json(stream, serde_json::json!({
                "embeddings": embs, "edges": edges,
                "message": format!("补全 {} 个 embedding, 建了 {} 条边", embs, edges),
            })),
            Err(err) => send_err(stream, 500, &err.to_string()),
        }
        return true;
    }
    // POST /api/refine —— 用 LLM 批量重提炼已有节点的标题+内容
    if clean == "/api/refine" && method == "POST" {
        let cfg = crate::llm_extract::load_config();
        if cfg.is_none() {
            send_err(stream, 400, "LLM not configured (need GM_LLM_API_KEY / GM_LLM_BASE_URL / GM_LLM_MODEL)");
            return true;
        }
        let nodes = e.all_nodes_raw();
        let total = nodes.len();
        let mut refined = 0;
        let mut errors = 0;
        for (id, title, content, source) in &nodes {
            // 跳过已经很短(像标题)的节点
            if title.len() < 30 && content.len() < 200 {
                continue;
            }
            match crate::llm_extract::refine_node(content, source, cfg.as_ref().unwrap()) {
                Ok((new_title, new_content)) => {
                    if let Err(err) = e.update_node_text(id, &new_title, &new_content) {
                        log::warn!("update_node_text failed for {}: {}", id, err);
                        errors += 1;
                    } else {
                        refined += 1;
                    }
                }
                Err(err) => {
                    log::warn!("refine failed for {}: {}", id, err);
                    errors += 1;
                }
            }
        }
        send_json(stream, serde_json::json!({
            "total": total, "refined": refined, "errors": errors,
            "message": format!("提炼 {} / {} 个节点 ({} 错误)", refined, total, errors),
        }));
        return true;
    }

    // POST /api/extract —— 用 LLM 从一段文本提炼知识并导入图
    if clean == "/api/extract" && method == "POST" {
        let text = body.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let source = body.get("source").and_then(|v| v.as_str()).unwrap_or("manual").to_string();
        let max_links = body.get("max_links").and_then(|v| v.as_u64()).map(|x| x as usize).unwrap_or(5);
        if text.is_empty() {
            send_err(stream, 400, "text is required");
            return true;
        }
        let result = crate::llm_extract::extract_and_import(&mut e, &text, &source, max_links);
        send_json(stream, result);
        return true;
    }
    // GET /api/llm/status —— 检查 LLM 配置状态
    if clean == "/api/llm/status" && method == "GET" {
        let configured = std::env::var("GM_LLM_API_KEY").map(|s| !s.is_empty()).unwrap_or(false);
        send_json(stream, serde_json::json!({
            "configured": configured,
            "model": std::env::var("GM_LLM_MODEL").unwrap_or_default(),
            "base_url": std::env::var("GM_LLM_BASE_URL").unwrap_or_default(),
        }));
        return true;
    }
    false
}

fn handle(mut stream: TcpStream, root: &Path, state: &ModelState, eng: Option<&EngineHandle>) {
    let mut buf = [0u8; 65536];
    let n = match stream.read(&mut buf) {
        Ok(n) if n > 0 => n,
        _ => return,
    };
    let req = String::from_utf8_lossy(&buf[..n]);
    let first_line = req.lines().next().unwrap_or("");
    let mut parts = first_line.split_whitespace();
    let method = parts.next().unwrap_or("GET");
    let raw_path = parts.next().unwrap_or("/");
    let (clean, query) = match raw_path.split_once('?') {
        Some((c, q)) => (c, q),
        None => (raw_path, ""),
    };
    log::info!("HTTP {} {}", method, raw_path);

    // ── 前端日志上报(诊断用) ──
    if clean == "/api/clientlog" && method == "POST" {
        let body = parse_json_body(&req);
        let kind = body.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let msg = body.get("msg").and_then(|v| v.as_str()).unwrap_or("");
        let src = body.get("src").and_then(|v| v.as_str()).unwrap_or("");
        let line = body.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
        if kind == "error" || kind == "reject" {
            log::error!("JS[{}] {} @{}:{}", kind, msg, src, line);
        } else {
            log::info!("JS[{}] {}", kind, msg);
        }
        send_json(&mut stream, serde_json::json!({"ok": true}));
        return;
    }

    // ── Agent 接入器(不依赖引擎) ──
    if clean == "/api/agents" && method == "GET" {
        let agents = crate::agent_connector::detect_agents();
        send_json(&mut stream, serde_json::json!({"agents": agents}));
        return;
    }
    if clean == "/api/agents/connect" && method == "POST" {
        let body = parse_json_body(&req);
        let agent_id = body.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        let exe = body.get("exe").and_then(|v| v.as_str()).unwrap_or("");
        // 如果没传 exe,用当前进程目录下的 mcp_server.exe
        let exe_path = if exe.is_empty() {
            let cur = std::env::current_exe().unwrap_or_default();
            let dir = cur.parent().unwrap_or(std::path::Path::new("."));
            dir.join("mcp_server.exe").to_string_lossy().into()
        } else {
            exe.to_string()
        };
        match crate::agent_connector::connect_agent(agent_id, &exe_path) {
            Ok(()) => send_json(&mut stream, serde_json::json!({"ok": true, "exe": exe_path})),
            Err(e) => send_err(&mut stream, 500, &e),
        }
        return;
    }
    if clean == "/api/agents/disconnect" && method == "POST" {
        let body = parse_json_body(&req);
        let agent_id = body.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        match crate::agent_connector::disconnect_agent(agent_id) {
            Ok(()) => send_json(&mut stream, serde_json::json!({"ok": true})),
            Err(e) => send_err(&mut stream, 500, &e),
        }
        return;
    }

    // ── 引擎 HTTP API(供 MCP server / 外部 agent 调用) ──
    if clean.starts_with("/api/") && eng.is_some() && clean != "/api/model_status" && clean != "/api/start_download" {
        let body = parse_json_body(&req);
        if handle_engine_api(&mut stream, method, clean, query, body, eng.unwrap()) {
            return;
        }
    }

    // ── API: 模型状态 + 进度 ──
    if clean == "/api/model_status" && method == "GET" {
        let cached = check_model_cached_internal();
        let downloading = state.downloading.load(Ordering::Relaxed);
        let done = state.done.load(Ordering::Relaxed);
        let ready = state.embedder_ready.load(Ordering::Relaxed);
        let err = state.error.lock().unwrap().clone();
        let dl = state.downloaded_bytes.load(Ordering::Relaxed);
        let total = state.total_bytes.load(Ordering::Relaxed);
        let cur_file = state.current_file.lock().unwrap().clone();
        let pct = if total > 0 { (dl * 100 / total) as u32 } else { 0 };
        let json = serde_json::json!({
            "cached": cached,
            "downloading": downloading,
            "done": done,
            "embedder_ready": ready,
            "error": err,
            "downloaded_bytes": dl,
            "total_bytes": total,
            "progress_pct": pct,
            "current_file": cur_file,
        });
        let body = json.to_string();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        return;
    }

    if clean == "/api/start_download" && method == "POST" {
        if state.downloading.load(Ordering::Relaxed) || state.done.load(Ordering::Relaxed) {
            let body = r#"{"status":"already_downloading"}"#;
            let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 26\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            return;
        }
        state.downloading.store(true, Ordering::Relaxed);
        let body = r#"{"status":"download_started"}"#;
        let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 27\r\nAccess-Control-Allow-Origin: *\r\n\r\n";
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        return;
    }

    // ── 静态文件 ──
    let rel = if clean == "/" { "index.html" } else { clean.trim_start_matches('/') };
    if rel.contains("..") {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
        return;
    }
    let full: PathBuf = root.join(rel);
    match std::fs::read(&full) {
        Ok(body) => {
            let ct = content_type(&full);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nAccess-Control-Allow-Origin: *\r\n\r\n",
                ct, body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
        }
        Err(_) => {
            let msg = format!("Not Found: {}", rel);
            let header = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
                msg.len()
            );
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(msg.as_bytes());
        }
    }
}

/// 启动静态服务器,返回实际监听端口
pub fn start(root: PathBuf, state: ModelState) -> Option<u16> {
    start_with_engine(root, state, None)
}

/// 启动静态 + 引擎 API 服务器,返回实际监听端口。
/// engine 非 None 时,/api/* 同进程直连 GraphEngine,供 MCP server / agent 调用。
/// 端口:优先 GM_PORT 环境变量,默认 9121(与 Python 后端一致,便于 MCP 复用)。
/// 9121 被占用时回退到系统分配端口,并把实际端口写入 ~/.graph-memory/port 文件供发现。
pub fn start_with_engine(root: PathBuf, state: ModelState, engine: Option<EngineHandle>) -> Option<u16> {
    if !root.exists() {
        log::error!("frontend dir not found: {}", root.display());
        return None;
    }
    let preferred = std::env::var("GM_PORT").ok()
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(9121);
    // 先试首选端口,占用则回退到 0(系统分配)
    let listener = TcpListener::bind(("127.0.0.1", preferred))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .ok()?;
    let port = listener.local_addr().ok()?.port();
    log::info!("static server on 127.0.0.1:{} root={} (preferred={})", port, root.display(), preferred);

    // 写端口发现文件,供 MCP server / 外部 agent 找到引擎进程
    if let Some(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok() {
        let dir = std::path::PathBuf::from(&home).join(".graph-memory");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("port"), port.to_string());
    }

    std::thread::spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(s) => {
                    let r = root.clone();
                    let st = state.clone();
                    let eng = engine.clone();
                    std::thread::spawn(move || handle(s, &r, &st, eng.as_ref()));
                }
                Err(e) => log::warn!("accept error: {}", e),
            }
        }
    });
    Some(port)
}
