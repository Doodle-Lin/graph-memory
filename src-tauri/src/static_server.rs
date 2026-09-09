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
    // 不发 Access-Control-Allow-Origin: webview 同源(127.0.0.1:9121),
    // MCP server 用 reqwest(非浏览器),都不需要 CORS。放开 = 让任意网站可读写图谱。
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn send_err(stream: &mut TcpStream, status: u16, msg: &str) {
    let body = serde_json::json!({"error": msg}).to_string();
    let header = format!(
        "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        status, body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// 从请求里解析 JSON body(找 \r\n\r\n 之后的内容)。
/// 返回 400 风格的 None 时调用方自行处理;此处仍宽松:解析失败给空 {}。
fn parse_json_body(req: &str) -> serde_json::Value {
    if let Some(pos) = req.find("\r\n\r\n") {
        let body = &req[pos + 4..];
        if body.is_empty() { return serde_json::json!({}); }
        return serde_json::from_str(body).unwrap_or_else(|e| {
            log::warn!("JSON body parse failed: {}", e);
            serde_json::json!({})
        });
    }
    serde_json::json!({})
}

/// 健壮地读取完整 HTTP 请求:先读到 headers 结束(\r\n\r\n),
/// 再按 Content-Length 读完整 body。避免单次 read() 截断大 body。
fn read_full_request(stream: &mut TcpStream) -> Option<String> {    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];
    // 1. 读到包含 \r\n\r\n (headers 结束)
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 { break; }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") { break; }
        if buf.len() > 50_000_000 { log::warn!("request headers too large"); return None; }
    }
    if buf.is_empty() { return None; }
    let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4).unwrap_or(buf.len());
    // 2. 解析 Content-Length,按需补读 body
    let headers_str = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = extract_header(&headers_str, "content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_have = buf.len() - header_end;
    let mut body = buf[header_end..].to_vec();
    if content_length > body_have {
        let mut remaining = content_length - body_have;
        while remaining > 0 {
            let n = stream.read(&mut tmp).ok()?;
            if n == 0 { break; }
            let take = n.min(remaining);
            body.extend_from_slice(&tmp[..take]);
            remaining -= take;
        }
    }
    // 3. 重组完整请求字符串(headers + body),供后续 parse_json_body 复用
    let mut full = headers_str.into_owned();
    full.push_str(&String::from_utf8_lossy(&body));
    Some(full)
}

/// 从 headers 文本里取某个 header 值(大小写不敏感)
fn extract_header(headers: &str, name: &str) -> Option<String> {
    for line in headers.lines() {
        let line = line.trim();
        if let Some(colon) = line.find(':') {
            if line[..colon].eq_ignore_ascii_case(name) {
                return Some(line[colon + 1..].to_string());
            }
        }
    }
    None
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

/// 安全截断(按字符),供 SSE 事件 JSON 用
fn trunc_str(s: impl AsRef<str>, n: usize) -> String {
    s.as_ref().chars().take(n).collect()
}

/// HTML 转义(防 XSS:搜索结果 innerHTML 拼接用户可控内容)
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;")
}

/// /api/refine 独立处理:LLM 调用不全程持有 Mutex
/// 只在读取候选列表和写回结果时短暂加锁
/// A5 修复:用 refine_in_progress 标志防止手动 refine 和 auto-refine 并发
fn handle_refine(stream: &mut TcpStream, eng: &EngineHandle) -> bool {
    let cfg = crate::llm_extract::load_config();
    if cfg.is_none() {
        send_err(stream, 400, "LLM not configured (need GM_LLM_API_KEY / GM_LLM_BASE_URL / GM_LLM_MODEL)");
        return true;
    }
    let cfg = cfg.unwrap();

    // 短暂加锁:读候选列表(克隆数据,释放锁后 LLM 调用不持锁)
    let (to_refine, total, skipped) = {
        let e = eng.engine.lock().unwrap();
        let nodes = e.all_nodes_raw();
        let to_refine: Vec<(String, String, String, String)> = nodes.iter()
            .filter(|(id, title, content, _)| {
                (title.len() >= 30 || content.len() >= 200) && !e.is_refined(id)
            })
            .cloned()
            .collect();
        let total = to_refine.len();
        let skipped = nodes.len() - total;
        (to_refine, total, skipped)
    }; // 锁释放

    // SSE headers
    let sse_header = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
    let _ = stream.write_all(sse_header);
    let start_event = format!("event: start\ndata: {}\n\n",
        serde_json::json!({"total": total, "skipped": skipped}));
    let _ = stream.write_all(start_event.as_bytes());

    let mut refined = 0;
    let mut errors = 0;
    for (i, (id, title, content, source)) in to_refine.iter().enumerate() {
        // LLM 调用(不持锁,可能 10-60s)
        match crate::llm_extract::refine_node(content, source, &cfg) {
            Ok((new_title, new_content)) => {
                // 短暂加锁写回
                let write_result = {
                    let mut e = eng.engine.lock().unwrap();
                    let r = e.update_node_text(id, &new_title, &new_content);
                    if r.is_ok() { e.mark_refined(id); }
                    r
                }; // 锁释放
                match write_result {
                    Ok(_) => {
                        refined += 1;
                        let ev = format!("event: progress\ndata: {}\n\n",
                            serde_json::json!({"index": i+1, "total": total, "id": id,
                                "old_title": trunc_str(title, 60), "new_title": trunc_str(new_title, 60),
                                "old_len": content.chars().count(), "new_len": new_content.chars().count()}));
                        let _ = stream.write_all(ev.as_bytes());
                    }
                    Err(err) => {
                        log::warn!("update_node_text failed for {}: {}", id, err);
                        errors += 1;
                        let ev = format!("event: error\ndata: {}\n\n",
                            serde_json::json!({"index": i+1, "total": total, "id": id,
                                "old_title": trunc_str(title, 60), "error": err.to_string()}));
                        let _ = stream.write_all(ev.as_bytes());
                    }
                }
            }
            Err(err) => {
                log::warn!("refine failed for {}: {}", id, err);
                errors += 1;
                let ev = format!("event: error\ndata: {}\n\n",
                    serde_json::json!({"index": i+1, "total": total, "id": id,
                        "old_title": trunc_str(title, 60), "error": trunc_str(err.to_string(), 120)}));
                let _ = stream.write_all(ev.as_bytes());
            }
        }
    }
    let done_event = format!("event: done\ndata: {}\n\n",
        serde_json::json!({"total": total, "refined": refined, "errors": errors,
            "message": format!("提炼 {} / {} 个节点 ({} 错误)", refined, total, errors)}));
    let _ = stream.write_all(done_event.as_bytes());
    true
}

/// 处理引擎 API。返回 true 表示命中并已响应,false 表示不是引擎 API(走静态文件)。
fn handle_engine_api(
    stream: &mut TcpStream, method: &str, clean: &str, raw_query: &str,
    body: serde_json::Value, eng: &EngineHandle,
) -> bool {

    // /api/refine 特殊处理:LLM 调用周期长,不能全程持有锁
    // 提前返回,在函数体内自行管理锁的获取/释放
    if clean == "/api/refine" && method == "POST" {
        return handle_refine(stream, eng);
    }

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
    // A4 修复:/api/graph 快照序列化(200KB+)不持锁
    // 先 clone 数据再释放锁,序列化在锁外做
    if clean == "/api/graph" && method == "GET" {
        let snapshot = {
            let e = eng.engine.lock().unwrap();
            e.graph_snapshot()
        }; // 锁释放
        send_json(stream, snapshot);
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
        // embedder 未就绪时:add_node 走 raw 降级(hash 去重),auto_link 跳过(无 embedding 无法建边)
        match e.add_node(content, title, nt, source, "{}") {
            Ok(node) => {
                let links = if auto_link && e.embedder_ready() { e.auto_link(&node.id, max_links).unwrap_or_default() } else { Vec::new() };
                send_json(stream, serde_json::json!({
                    "node": node,
                    "auto_links": serde_json::to_value(links).unwrap(),
                }));
            }
            Err(e) => send_err(stream, 500, &e.to_string()),
        }
        return true;
    }
    // POST /api/update —— 按 node_id 精确更新,或按 query fuzzy 检索后更新
    // 优先用 node_id(精确,不会改错);无 node_id 时用 query(模糊,有改错风险)
    if clean == "/api/update" && method == "POST" {
        let node_id = body.get("node_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let query = body.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let new_content = body.get("new_content").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let new_title = body.get("new_title").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let node_type = body.get("node_type").and_then(|v| v.as_str()).unwrap_or("knowledge").to_string();
        if new_content.is_empty() {
            send_err(stream, 400, "new_content is required");
            return true;
        }
        if node_id.is_empty() && query.is_empty() {
            send_err(stream, 400, "node_id or query is required");
            return true;
        }

        // 确定 target_id:优先 node_id 精确匹配,fallback 用 query fuzzy 检索
        let target_id: Option<String> = if !node_id.is_empty() {
            // 精确:检查 node 是否存在
            if e.node_exists(&node_id) { Some(node_id.clone()) }
            else { None }
        } else {
            // 模糊:retrieve top-1
            match e.retrieve(&query, Some(1), false) {
                Ok(results) => results.first().map(|r| r.id.clone()),
                Err(_) => None,
            }
        };

        if let Some(id) = target_id {
            let title = if new_title.is_empty() {
                // 不改标题时保留原标题
                e.get_node_title(&id).unwrap_or_default()
            } else { new_title.clone() };
            match e.update_node_text(&id, &title, &new_content) {
                Ok(_) => send_json(stream, serde_json::json!({
                    "action": "updated", "node_id": id, "message": "已更新已有节点"
                })),
                Err(err) => send_err(stream, 500, &err.to_string()),
            }
        } else {
            // 未命中:创建新节点
            match e.add_node(&new_content, &new_title, &node_type, "agent:update", "{}") {
                Ok(node) => {
                    let links = e.auto_link(&node.id, 5).unwrap_or_default();
                    send_json(stream, serde_json::json!({
                        "action": "created", "node": node,
                        "auto_links": serde_json::to_value(links).unwrap(),
                        "message": "未找到匹配节点,已创建新节点"
                    }));
                }
                Err(err) => send_err(stream, 500, &err.to_string()),
            }
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
    // POST /api/refine 已提前到 handle_refine 处理(不持有全程锁)

    // GET /api/dedup/scan —— 近似对候选(只读,零 LLM)
    if clean == "/api/dedup/scan" && method == "GET" {
        let pairs = e.dedup_scan(0.7, 0.85, 50);
        send_json(stream, serde_json::json!({"pairs": pairs, "count": pairs.len()}));
        return true;
    }

    // POST /api/consolidate —— 合并两个节点(I1:维护工具)
    if clean == "/api/consolidate" && method == "POST" {
        let keep_id = body.get("keep_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let merge_id = body.get("merge_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if keep_id.is_empty() || merge_id.is_empty() {
            send_err(stream, 400, "keep_id and merge_id are required");
            return true;
        }
        match e.consolidate(&keep_id, &merge_id) {
            Ok(node) => send_json(stream, serde_json::json!({
                "ok": true, "node": node,
                "message": format!("已合并 {} 到 {}", merge_id, keep_id),
            })),
            Err(err) => send_err(stream, 500, &err.to_string()),
        }
        return true;
    }
    // POST /api/forget —— 遗忘陈旧节点(I1:维护工具)
    if clean == "/api/forget" && method == "POST" {
        let max_age = body.get("max_age_days").and_then(|v| v.as_u64()).unwrap_or(180) as i64;
        let min_ac = body.get("min_access_count").and_then(|v| v.as_u64()).unwrap_or(2) as i64;
        let (deleted, kept) = e.forget_stale(max_age, min_ac);
        send_json(stream, serde_json::json!({
            "deleted": deleted, "kept": kept,
            "message": format!("删除 {} 个陈旧节点,保留 {} 个", deleted, kept),
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

    false
}

fn handle(mut stream: TcpStream, root: &Path, state: &ModelState, eng: Option<&EngineHandle>) {
    let req = match read_full_request(&mut stream) {
        Some(r) => r,
        None => return,
    };
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
    // ── LLM 配置:读 .env + 即时设进程环境变量(不依赖引擎) ──
    if clean == "/api/llm/status" && method == "GET" {
        let configured = std::env::var("GM_LLM_API_KEY").map(|s| !s.is_empty()).unwrap_or(false);
        send_json(&mut stream, serde_json::json!({
            "configured": configured,
            "model": std::env::var("GM_LLM_MODEL").unwrap_or_default(),
            "base_url": std::env::var("GM_LLM_BASE_URL").unwrap_or_default(),
        }));
        return;
    }
    if clean == "/api/llm/config" && method == "POST" {
        let body = parse_json_body(&req);
        let api_key = body.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
        let base_url = body.get("base_url").and_then(|v| v.as_str()).unwrap_or("");
        let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
        match write_llm_env(api_key, base_url, model) {
            Ok(path) => {
                // 即时生效:设置进程环境变量,refine 立刻可用(无需重启)
                // 注意:api_key 为空时保留现有 key(前端不回显真实 key,传空表示不改)
                if !api_key.is_empty() {
                    std::env::set_var("GM_LLM_API_KEY", api_key);
                }
                std::env::set_var("GM_LLM_BASE_URL", base_url);
                std::env::set_var("GM_LLM_MODEL", model);
                log::info!("LLM config updated: base_url={}, model={}", base_url, model);
                send_json(&mut stream, serde_json::json!({"ok": true, "env_path": path}));
            }
            Err(e) => send_err(&mut stream, 500, &e),
        }
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
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        return;
    }

    if clean == "/api/start_download" && method == "POST" {
        if state.downloading.load(Ordering::Relaxed) || state.done.load(Ordering::Relaxed) {
            let body = r#"{"status":"already_downloading"}"#;
            let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 26\r\n\r\n";
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(body.as_bytes());
            return;
        }
        state.downloading.store(true, Ordering::Relaxed);
        let body = r#"{"status":"download_started"}"#;
        let header = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 27\r\n\r\n";
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
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\r\n",
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

/// 写 LLM 配置到 .env(更新已有 key 或追加,保留其他行),返回 .env 路径。
/// 路径解析与 lib.rs::load_env_file 对齐:cwd 优先,其次 exe 同目录。
fn write_llm_env(api_key: &str, base_url: &str, model: &str) -> Result<String, String> {
    let cwd_env = std::env::current_dir().map(|d| d.join(".env")).ok();
    let exe_env = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(".env")));
    let env_path = {
        let mut found: Option<std::path::PathBuf> = None;
        for p in cwd_env.as_ref().into_iter().chain(exe_env.as_ref()) {
            if p.exists() { found = Some(p.clone()); break; }
        }
        found.or_else(|| cwd_env.clone()).or_else(|| exe_env.clone())
            .unwrap_or_else(|| std::path::PathBuf::from(".env"))
    };

    // 读现有 .env(保留非 LLM 行)
    let mut lines: Vec<String> = std::fs::read_to_string(&env_path).ok()
        .map(|c| c.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();

    // 更新或追加 GM_LLM_ key(空值表示不改,保留 .env 现有行)
    let updates = [
        ("GM_LLM_API_KEY", api_key),
        ("GM_LLM_BASE_URL", base_url),
        ("GM_LLM_MODEL", model),
    ];
    for (key, val) in &updates {
        // 空 value 跳过(保留现有 .env 行,不覆盖)
        if val.is_empty() { continue; }
        let prefix = format!("{}=", key);
        if let Some(line) = lines.iter_mut().find(|l| l.trim_start().starts_with(&prefix)) {
            *line = format!("{}={}", key, val);
        } else {
            lines.push(format!("{}={}", key, val));
        }
    }

    let body = lines.join("\n");
    std::fs::write(&env_path, body).map_err(|e| format!("write .env failed: {}", e))?;
    Ok(env_path.to_string_lossy().into())
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
