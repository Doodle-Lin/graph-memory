// 极简静态文件服务器 + 引擎 HTTP API + 模型下载状态/进度
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::engine::GraphEngine;

// ───────────────────────── 本机协议资源上限 (L5-02) ─────────────────────────
// 服务只监听 127.0.0.1,但同机任意进程都能连:没有上限时一个脚本就能让服务
// 分配无界内存或堆积线程。默认值全部集中在这里,并有单元测试与真实套接字断言。

/// 请求行 + 全部头部字节数上限。超过 → 431(超限时只读到上限+1 就返回,不累积)。
const MAX_HEADER_BYTES: usize = 32 * 1024;
/// 正文上限。超过 → 413;声明就超限直接拒,实际读取时边读边判。
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// 单个连接读取/写入空闲超时。超时 → 408(读超时)。
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
/// 同时处理的连接数上限。超出立即 503,不排队、不阻塞 accept 循环。
const MAX_CONCURRENT_CONNECTIONS: usize = 64;
/// 写入超时(与读取同值即可;大量写阻塞交给 OS 缓冲,不需要更久)。
const WRITE_TIMEOUT: Duration = Duration::from_secs(15);
/// 请求行长度上限(单行,含方法/目标/版本)。
const MAX_REQUEST_LINE_BYTES: usize = 8 * 1024;

/// 当前正在处理的连接数 + 累计拒绝计数(拒绝只记原因与计数,绝不记正文)。
static ACTIVE_CONNECTIONS: AtomicUsize = AtomicUsize::new(0);
static REJECTIONS_HEADER_TOO_LARGE: AtomicU64 = AtomicU64::new(0);
static REJECTIONS_BODY_TOO_LARGE: AtomicU64 = AtomicU64::new(0);
static REJECTIONS_MALFORMED: AtomicU64 = AtomicU64::new(0);
static REJECTIONS_METHOD_NOT_ALLOWED: AtomicU64 = AtomicU64::new(0);
static REJECTIONS_TIMEOUT: AtomicU64 = AtomicU64::new(0);
static REJECTIONS_TOO_MANY_CONNECTIONS: AtomicU64 = AtomicU64::new(0);

/// 拒绝日志:原因 + 累计计数,不含正文(只记方法/路径等元数据)。
fn log_rejection(counter: &AtomicU64, status: u16, reason: &str, detail: &str) {
    let total = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if detail.is_empty() {
        log::warn!("rejected request status={} reason={} count={}", status, reason, total);
    } else {
        log::warn!(
            "rejected request status={} reason={} count={} detail={}",
            status, reason, total, detail
        );
    }
}

#[derive(Clone)]
pub struct ModelState {
    pub memory_store: Option<Arc<Mutex<crate::memory_store::MemoryStore>>>,
    pub service_info: Option<Arc<serde_json::Value>>,
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
            service_info: None,
            memory_store: None,
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
    // charset 必须显式声明: 正文是 UTF-8 中文,而 HTTP 默认字符集是 ISO-8859-1,
    // 省略会让严格客户端(如 .NET HttpClient 的默认 JSON 读取)把中文解码成乱码。
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// 统一的错误响应:**所有**状态码都经由同一个构造函数,保证
/// (1) Content-Type 恒为 `application/json; charset=utf-8`,
/// (2) 一定带 Content-Length(不允许只发头),
/// (3) 额外头(如 405 的 `Allow`)也在这里拼装,不另起分支。
/// 返回写入是否成功,便于调用方在连接已断开时跳过后续处理。
fn send_error(stream: &mut TcpStream, status: u16, extra_headers: &[(&str, &str)], msg: &str) -> bool {
    let body = serde_json::json!({"error": msg}).to_string();
    let mut header = format!("HTTP/1.1 {} {}\r\n", status, reason_phrase(status));
    header.push_str("Content-Type: application/json; charset=utf-8\r\n");
    for (name, value) in extra_headers {
        header.push_str(&format!("{}: {}\r\n", name, value));
    }
    header.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(header.as_bytes()).is_ok() && stream.write_all(body.as_bytes()).is_ok()
}

/// 既有签名(无额外头)的薄封装:历史调用点语义与输出保持不变。
fn send_err(stream: &mut TcpStream, status: u16, msg: &str) {
    send_error(stream, status, &[], msg);
}

/// 写完拒绝响应后,让对端能完整读到它,再关闭连接(P3-F)。
///
/// Windows 上如果关闭时接收缓冲里仍有未读数据,内核发的是 RST 而不是 FIN,
/// RST 会丢弃对端接收队列里**已经写好的**错误响应:实测 2 MiB/8 MiB 超限正文
/// 只有部分轮次能读到 413,其余只剩 `ECONNRESET`(竞态,同一二进制同一轮内时好时坏)。
///
/// 因此关闭顺序固定为:
/// 1. `shutdown(Write)` 半关闭:发 FIN,对端立刻能读到完整响应并看到 EOF,不再等我们;
/// 2. 排空直到 **EOF**(对端发完 FIN)/ 空闲超时 / 总量上限;
/// 3. 再 `shutdown(Both)` 关闭。
///
/// 上限与超时都必须保留:谎报 `Content-Length: 999999999` 只发几字节的客户端
/// 不能让我们无限读(它不发 FIN 时由 250 ms 空闲超时结束)。
fn drain_before_close(stream: &mut TcpStream) {
    const IDLE_TIMEOUT: Duration = Duration::from_millis(250);
    const MAX_DRAIN_BYTES: usize = 8 * 1024 * 1024;

    // 半关闭必须先做:响应已 flush,对端读走响应后就能看到 EOF 并发回 FIN。
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(IDLE_TIMEOUT));
    let mut scratch = [0u8; 64 * 1024];
    let mut drained = 0usize;
    while drained < MAX_DRAIN_BYTES {
        match stream.read(&mut scratch) {
            Ok(0) => break,
            Ok(n) => drained += n,
            // 空闲超时/连接已断:没有更多在途数据,可以安全关闭。
            Err(_) => break,
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// 解析请求正文 JSON(正文已由 read_full_request 按 Content-Length 精确切出)。
/// 宽松:空正文或解析失败都给空对象(与原行为一致,错误由路由层报)。
fn parse_json_body(body: &str) -> serde_json::Value {
    if body.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(body).unwrap_or_else(|e| {
        log::warn!("JSON body parse failed: {}", e);
        serde_json::json!({})
    })
}

/// 已成功解析的请求:头部原文、正文原文、方法、原始目标、路径、查询串。
struct HttpRequest {
    header_text: String,
    body: String,
    method: String,
    raw_target: String,
    path: String,
    query: String,
}

/// 协议层拒绝原因。每个 variant 对应一个固定状态码,便于断言与日志。
#[derive(Debug, PartialEq, Eq)]
enum Reject {
    /// 头部超过 MAX_HEADER_BYTES,或单行超过 MAX_REQUEST_LINE_BYTES → 431
    HeaderTooLarge,
    /// 声明的或实际的正文超过 MAX_BODY_BYTES → 413
    BodyTooLarge,
    /// 请求行/头部畸形(无空格、无路径、非法字节、非 UTF-8 路径、缺 Host 之类的空行) → 400
    Malformed(&'static str),
    /// 连接读取超时 → 408
    Timeout,
}

impl Reject {
    fn status(&self) -> u16 {
        match self {
            Reject::HeaderTooLarge => 431,
            Reject::BodyTooLarge => 413,
            Reject::Malformed(_) => 400,
            Reject::Timeout => 408,
        }
    }
    fn reason(&self) -> &'static str {
        match self {
            Reject::HeaderTooLarge => "header_too_large",
            Reject::BodyTooLarge => "body_too_large",
            Reject::Malformed(what) => what,
            Reject::Timeout => "read_timeout",
        }
    }
    /// 回给客户端的可读消息:不含正文,只描述协议问题。
    fn message(&self) -> String {
        match self {
            Reject::HeaderTooLarge => format!(
                "request line and headers must not exceed {} bytes",
                MAX_HEADER_BYTES
            ),
            Reject::BodyTooLarge => format!("request body must not exceed {} bytes", MAX_BODY_BYTES),
            Reject::Malformed(what) => format!("malformed request: {}", what),
            Reject::Timeout => format!(
                "request timed out after {}s; send the full request before the deadline",
                CONNECTION_TIMEOUT.as_secs()
            ),
        }
    }
}

/// 解析请求行:`METHOD SP request-target SP HTTP/x.y`。
/// 任何不满足形状的输入都返回 400,绝不对输入做裸切片(消除 `&req[..find(' ')]` 崩溃点)。
fn parse_request_line(line: &str) -> Result<(String, String, String), Reject> {
    if line.is_empty() {
        return Err(Reject::Malformed("empty request line"));
    }
    if line.len() > MAX_REQUEST_LINE_BYTES {
        return Err(Reject::HeaderTooLarge);
    }
    let mut parts = line.split(' ');
    let method = parts
        .next()
        .filter(|m| !m.is_empty())
        .ok_or(Reject::Malformed("missing method"))?;
    let target = parts
        .next()
        .filter(|t| !t.is_empty())
        .ok_or(Reject::Malformed("missing request target"))?;
    let version = parts
        .next()
        .filter(|v| !v.is_empty())
        .ok_or(Reject::Malformed("missing HTTP version"))?;
    if parts.next().is_some() {
        return Err(Reject::Malformed("request line has extra fields"));
    }
    if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(Reject::Malformed("request line is not HTTP/1.0 or HTTP/1.1"));
    }
    if !target.starts_with('/') && target != "*" {
        return Err(Reject::Malformed("request target must be an absolute path"));
    }
    // 控制字符(含 CR/LF、NUL)一律拒绝:避免 header/请求行走私与日志注入。
    if !method.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Reject::Malformed("method contains non-token bytes"));
    }
    if target.bytes().any(|b| b.is_ascii_control()) {
        return Err(Reject::Malformed("request target contains control bytes"));
    }
    Ok((method.to_string(), target.to_string(), version.to_string()))
}

/// 校验请求头(Content-Length 已在读取阶段限额并校验):每一行必须是 `name: value`。
/// 注意用 `\r\n` 精确切分:裸 `\n` 行尾在解析阶段已被归一化,这里再遇到说明头块本身畸形。
fn validate_headers(header_block: &str) -> Result<(), Reject> {
    for line in header_block.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(Reject::Malformed("header continuation line is not allowed"));
        }
        let colon = line
            .find(':')
            .ok_or(Reject::Malformed("header line has no colon"))?;
        let name = &line[..colon];
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_graphic() && b != b':') {
            return Err(Reject::Malformed("invalid header name"));
        }
    }
    Ok(())
}

/// 把已限量的头部原文与正文组装成 `HttpRequest`,顺便做协议层校验。
fn build_request(header_block: &str, body: &[u8]) -> Result<HttpRequest, Reject> {
    let request_line = header_block.split("\r\n").next().unwrap_or_default();
    let (method, raw_target, _version) = parse_request_line(request_line)?;
    validate_headers(header_block)?;
    let declared = declared_content_length(header_block)?;
    if declared > MAX_BODY_BYTES {
        return Err(Reject::BodyTooLarge);
    }
    if body.len() > declared {
        // 发送的字节多于声明:多余字节可能是下一个请求(HTTP 流水线),协议上不接受。
        return Err(Reject::Malformed("body exceeds the declared content-length"));
    }
    let (path, query) = match raw_target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (raw_target.clone(), String::new()),
    };
    Ok(HttpRequest {
        header_text: header_block.to_string(),
        body: String::from_utf8_lossy(body).into_owned(),
        method,
        raw_target,
        path,
        query,
    })
}

/// 读取完整 HTTP 请求:先按行读到空行(头部结束),再**边读边判**地读正文。
///
/// 资源边界:
/// - 头部/请求行字节数在累计过程中检查,超过 MAX_HEADER_BYTES 立即 431(只多读 1 字节探测);
/// - 正文声明超过 MAX_BODY_BYTES 立即 413(不读正文);
/// - 实际读取时每次读取前先检查剩余额度,绝不会把无界数据读进内存;
/// - 读超时(socket read timeout,15s)→ 408;
/// - 畸形请求行/头(无空格、无路径、非 UTF-8、无冒号等)→ 400,且不 panic。
fn read_full_request(stream: &mut TcpStream) -> Result<HttpRequest, Reject> {
    let mut reader = BufReader::with_capacity(8 * 1024, stream.try_clone().map_err(|_| Reject::Timeout)?);
    let mut header_bytes: usize = 0;
    let mut header_text = String::new();
    loop {
        let available = MAX_HEADER_BYTES.saturating_sub(header_bytes);
        // 每次最多读一个限额片段,便于准确判断"是否越界"。
        let mut limited = (&mut reader).take((available as u64) + 1);
        let mut line = Vec::new();
        let read = limited.read_until(b'\n', &mut line);
        match read {
            Ok(0) => {
                if header_text.is_empty() {
                    return Err(Reject::Malformed("connection closed before any request bytes"));
                }
                return Err(Reject::Malformed("connection closed inside the header block"));
            }
            Ok(n) => {
                header_bytes += n;
                if header_bytes > MAX_HEADER_BYTES {
                    return Err(Reject::HeaderTooLarge);
                }
                let text = String::from_utf8(line)
                    .map_err(|_| Reject::Malformed("header block is not valid UTF-8"))?;
                let terminated = text.ends_with('\n');
                header_text.push_str(text.trim_end_matches(['\r', '\n']));
                if terminated {
                    header_text.push_str("\r\n");
                }
                if !terminated {
                    // read_until 只在 EOF 或限额耗尽时返回不带换行的片段。
                    return Err(if header_bytes > MAX_HEADER_BYTES {
                        Reject::HeaderTooLarge
                    } else {
                        Reject::Malformed("header line is not terminated")
                    });
                }
                if text == "\r\n" || text == "\n" {
                    break;
                }
                if text.trim_end_matches(['\r', '\n']).len() > MAX_REQUEST_LINE_BYTES {
                    return Err(Reject::HeaderTooLarge);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut => return Err(Reject::Timeout),
            Err(_) => return Err(Reject::Malformed("header read failed")),
        }
    }

    // 头部已完整:先解析/校验 Content-Length,再决定是否读正文。
    // 声明就超限 → 直接 413,一个正文字节都不读(欺骗请求在此被拦)。
    let declared = declared_content_length(&header_text)?;
    if declared > MAX_BODY_BYTES {
        return Err(Reject::BodyTooLarge);
    }
    let mut body = Vec::with_capacity(declared.min(64 * 1024));
    // 无论声明多少,累计正文永远不超过 MAX_BODY_BYTES(声明与实际不一致时也受此约束)。
    let cap = declared.min(MAX_BODY_BYTES);
    while body.len() < declared {
        let want = (declared - body.len()).min(64 * 1024);
        let mut chunk = vec![0u8; want];
        match reader.read(&mut chunk) {
            Ok(0) => return Err(Reject::Malformed("connection closed before the body was complete")),
            Ok(n) => {
                body.extend_from_slice(&chunk[..n]);
                if body.len() > cap || body.len() > MAX_BODY_BYTES {
                    return Err(Reject::BodyTooLarge);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
                || error.kind() == std::io::ErrorKind::TimedOut => return Err(Reject::Timeout),
            Err(_) => return Err(Reject::Malformed("body read failed")),
        }
    }
    build_request(&header_text, &body)
}

/// 从头部原文里取 Content-Length(缺失 = 0;非数字 = 400)。
fn declared_content_length(header_text: &str) -> Result<usize, Reject> {
    match extract_header(header_text, "content-length") {
        None => Ok(0),
        Some(value) => value
            .trim()
            .parse::<usize>()
            .map_err(|_| Reject::Malformed("content-length is not a number")),
    }
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

/// /api/import:只做 raw 入库(毫秒级),enrich 异步触发。
/// 修前:import 在 HTTP handler 内同步 enrich_all,大图上持单 Mutex 10 分钟,
/// 饿死所有 agent API(/api/health /write /retrieve 全超时)。
/// 修后:import 只持锁跑三个 importer(写 SQLite+FTS5,实测 ~250ms),
/// 释放锁后立即返回;若 embedder 就绪,后台线程异步 enrich_all 补边+去重。
/// 不撤销 cb5b7a5 的去重意图 —— 去重仍在 enrich_all 里跑,只是不在 import 请求内同步跑。
/// 这也回到 ARCHITECTURE §2.3 的两阶段导入本意:导入立刻可见,模型就绪后后台补全。
fn handle_import(
    stream: &mut TcpStream, raw_query: &str, _body: serde_json::Value, eng: &EngineHandle,
) -> bool {
    let source = parse_query_str(raw_query, "source", "all").to_string();
    let enrich_ready;
    let result = {
        let mut e = eng.engine.lock().unwrap();
        enrich_ready = e.embedder_ready();
        match source.as_str() {
            "all" => crate::importer::import_all_with_opts(&mut e, false),
            "hermes" => {
                let r = crate::importer::import_hermes(&mut e);
                serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "sources": serde_json::json!({"hermes": r.total}),
                    "message": format!("导入 {} 个节点", r.total)})
            }
            "claude" => {
                let r = crate::importer::import_claude(&mut e);
                serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "sources": serde_json::json!({"claude": r.total}),
                    "message": format!("导入 {} 个节点", r.total)})
            }
            "codex" => {
                let r = crate::importer::import_codex(&mut e);
                serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "sources": serde_json::json!({"codex": r.total}),
                    "message": format!("导入 {} 个节点", r.total)})
            }
            other => {
                send_err(stream, 400, &format!("unknown source: {} (支持 all/hermes/claude/codex)", other));
                return true;
            }
        }
    }; // 锁释放 —— import 请求毫秒级返回

    // embedder 就绪:后台异步 enrich(不持请求锁,不阻塞 agent API)。
    // 失败只记日志,enrich 结果前端可通过 /api/stats 轮询 edge_count 观察。
    if enrich_ready {
        let eng = eng.clone();
        std::thread::spawn(move || {
            log::info!("[import] async enrich started after import");
            let mut e = eng.engine.lock().unwrap();
            match e.enrich_all() {
                Ok((embs, edges)) => log::info!("[import] async enrich done: {} embs, {} edges", embs, edges),
                Err(e) => log::warn!("[import] async enrich failed: {}", e),
            }
        });
    } else {
        log::info!("[import] embedder not ready, enrich deferred to model-load");
    }

    send_json(stream, result);
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

    // /api/import 特殊处理:import 本身(三个 importer 写 SQLite+FTS5)是毫秒级,
    // 但紧随其后的 enrich_all(embed+auto_link+O(N²) dedup)在大图上是分钟级,
    // 全程持有单 Mutex 会饿死所有 agent API(实测一次 import 持锁 10 分钟,
    // /api/health /write /retrieve 全部超时)。所以 import 只在锁内做 raw 入库,
    // 锁释放后异步触发 enrich,请求立刻返回。
    if clean == "/api/import" && method == "POST" {
        return handle_import(stream, raw_query, body, eng);
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
        let snapshot = e.graph_snapshot();
        drop(e);
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
        let min_score = parse_query_str(raw_query, "min_score", "0.0").parse::<f32>().unwrap_or(0.0);
        match e.retrieve(&q, Some(top_k), false, min_score) {
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
        let min_score = body.get("min_score").and_then(|v| v.as_f64()).map(|x| x as f32).unwrap_or(0.0);
        match e.retrieve(&query, top_k, spread.unwrap_or(true), min_score) {
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
            match e.retrieve(&query, Some(1), false, 0.0) {
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
        // 现由 handle_import 提前处理(见函数入口),这里不会到达。
        // 保留分支以防逻辑改动后回到这里 —— 但 enrich 不再在请求内同步跑。
        let source = parse_query_str(raw_query, "source", "all").to_string();
        let result = match source.as_str() {
            "all" => crate::importer::import_all_with_opts(&mut e, false),
            "hermes" => {
                let r = crate::importer::import_hermes(&mut e);
                serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "message": format!("导入 {} 个节点", r.total)})
            }
            "claude" => {
                let r = crate::importer::import_claude(&mut e);
                serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "message": format!("导入 {} 个节点", r.total)})
            }
            "codex" => {
                let r = crate::importer::import_codex(&mut e);
                serde_json::json!({"total_nodes": r.total, "errors": r.errors,
                    "message": format!("导入 {} 个节点", r.total)})
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

/// `/api/v1/*` 允许的方法白名单:与 `memory_store::MemoryStore::route` 的实际能力一致。
/// 这里只做协议层快速拒绝(405 + Allow),不是路由表本身。
const API_V1_ALLOWED_METHODS: &[&str] = &["GET", "POST"];
/// 405 响应里 `Allow` 头的取值。
const API_V1_ALLOW: &str = "GET, POST";

/// 连接并发闸门:超过 MAX_CONCURRENT_CONNECTIONS 时**立即** 503,
/// 不排队、不阻塞 accept 循环(排队等于变相把资源上限变成延迟上限)。
struct ConnectionGate;

impl ConnectionGate {
    /// 尝试占用一个连接名额;超限时返回 None(并在内部完成 503 响应与计数)。
    fn acquire() -> Option<ConnectionPermit> {
        let previous = ACTIVE_CONNECTIONS.fetch_add(1, Ordering::AcqRel);
        if previous >= MAX_CONCURRENT_CONNECTIONS {
            ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(ConnectionPermit { released: false })
    }
}

/// RAII 名额:线程 panic 或提前 return 都会归还计数。
struct ConnectionPermit {
    released: bool,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        if !self.released {
            ACTIVE_CONNECTIONS.fetch_sub(1, Ordering::AcqRel);
            self.released = true;
        }
    }
}

/// 收下一个连接(或拒收)。返回 None 表示已按资源上限拒绝并写回响应。
fn admit_connection(mut stream: TcpStream) -> Option<(TcpStream, ConnectionPermit)> {
    // 超时必须在读取之前设置:15s 无数据 → 读超时 → 408。
    if stream.set_read_timeout(Some(CONNECTION_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
        log::warn!("failed to configure socket timeouts; connection dropped");
        return None;
    }
    match ConnectionGate::acquire() {
        Some(permit) => Some((stream, permit)),
        None => {
            log_rejection(
                &REJECTIONS_TOO_MANY_CONNECTIONS,
                503,
                "too_many_connections",
                &format!("active>={}", MAX_CONCURRENT_CONNECTIONS),
            );
            let _ = send_error(
                &mut stream,
                503,
                &[("Connection", "close")],
                &format!(
                    "too many concurrent connections (limit {})",
                    MAX_CONCURRENT_CONNECTIONS
                ),
            );
            let _ = stream.flush();
            // 名额拒绝时对端还没发过任何请求字节(连接是空闲占满的),因此直接
            // 半关闭写端即可:拿走响应和 FIN,再关闭。若这里改用排空,64 个空闲
            // 连接会因为对端不关闭而各占用 250 ms 空闲超时。
            let _ = stream.shutdown(std::net::Shutdown::Write);
            let _ = stream.shutdown(std::net::Shutdown::Both);
            None
        }
    }
}

/// `/api/v1/*` 的方法白名单检查:不在白名单 → 405 + Allow,且**不执行任何路由**。
/// 返回 true 表示已响应并应结束该连接的处理。
fn reject_disallowed_method(stream: &mut TcpStream, method: &str, clean: &str) -> bool {
    if !clean.starts_with("/api/v1/") || API_V1_ALLOWED_METHODS.contains(&method) {
        return false;
    }
    log_rejection(
        &REJECTIONS_METHOD_NOT_ALLOWED,
        405,
        "method_not_allowed",
        &format!("{} {}", method, clean),
    );
    let _ = send_error(
        stream,
        405,
        &[("Allow", API_V1_ALLOW)],
        &format!("method {} is not allowed on {}", method, clean),
    );
    true
}

fn handle(mut stream: TcpStream, root: Option<&Path>, state: &ModelState, eng: Option<&EngineHandle>) {
    // 资源上限:头部/正文超限、畸形、超时都在这里统一拒绝并回写 JSON 错误。
    let req = match read_full_request(&mut stream) {
        Ok(req) => req,
        Err(reject) => {
            let counter = match &reject {
                Reject::HeaderTooLarge => &REJECTIONS_HEADER_TOO_LARGE,
                Reject::BodyTooLarge => &REJECTIONS_BODY_TOO_LARGE,
                Reject::Malformed(_) => &REJECTIONS_MALFORMED,
                Reject::Timeout => &REJECTIONS_TIMEOUT,
            };
            log_rejection(counter, reject.status(), reject.reason(), "");
            let _ = send_error(&mut stream, reject.status(), &[], &reject.message());
            let _ = stream.flush();
            // 必须"半关闭 + 排空"再关闭:否则 Windows 会发 RST,把刚写出的
            // 4xx 响应丢掉。413 时客户端可能正在推送大量正文字节,排空会一直
            // 读到对端 FIN 或 250 ms 空闲超时(谎报 Content-Length 时无人发送,
            // 立即超时返回,不会变慢)。
            drain_before_close(&mut stream);
            return;
        }
    };
    // Native-only service: browsers must not mutate the local database via cross-origin forms.
    if root.is_none() && req.header_text.lines().take_while(|line| !line.is_empty())
        .any(|line| line.to_ascii_lowercase().starts_with("origin:")) {
        send_err(&mut stream, 403, "browser origins are not accepted by the native service");
        return;
    }
    let method = req.method.as_str();
    let raw_path = req.raw_target.as_str();
    let clean = req.path.as_str();
    let query = req.query.as_str();
    // L5-02: `/api/v1/*` 只允许 GET/POST;其余方法(favicon.ico、浏览器/脚本探测)一律 405。
    if reject_disallowed_method(&mut stream, method, clean) {
        return;
    }
    if clean == "/api/service" && method == "GET" {
        if let Some(info) = &state.service_info {
            send_json(&mut stream, (**info).clone());
            return;
        }
    }
    if clean.starts_with("/api/v1/") {
        if let Some(store) = &state.memory_store {
            match store.lock().unwrap().route(method, clean, parse_json_body(&req.body)) {
                Ok(value) => send_json(&mut stream, value),
                Err(error) => send_err(&mut stream, error.0, &error.1),
            }
            return;
        }
    }
    if root.is_none() && !matches!((method, clean),
        ("GET", "/api/health" | "/api/stats" | "/api/graph" | "/api/recent" | "/api/model_status") |
        ("POST", "/api/write" | "/api/retrieve")) {
        send_err(&mut stream, 404, "endpoint not enabled in experimental service");
        return;
    }
    log::info!("HTTP {} {}", method, raw_path);

    // ── 前端日志上报(诊断用) ──
    if clean == "/api/clientlog" && method == "POST" {
        let body = parse_json_body(&req.body);
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
        let body = parse_json_body(&req.body);
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
        let body = parse_json_body(&req.body);
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
        let body = parse_json_body(&req.body);
        let agent_id = body.get("agent").and_then(|v| v.as_str()).unwrap_or("");
        match crate::agent_connector::disconnect_agent(agent_id) {
            Ok(()) => send_json(&mut stream, serde_json::json!({"ok": true})),
            Err(e) => send_err(&mut stream, 500, &e),
        }
        return;
    }

    // ── 引擎 HTTP API(供 MCP server / 外部 agent 调用) ──
    if clean.starts_with("/api/") && eng.is_some() && clean != "/api/model_status" && clean != "/api/start_download" {
        let body = parse_json_body(&req.body);
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
    let Some(root) = root else {
        send_err(&mut stream, 404, "API-only service has no frontend");
        return;
    };
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
                    // 资源上限在接连接时统一执行(超限立即 503,不进入处理线程)。
                    let Some((stream, permit)) = admit_connection(s) else {
                        continue;
                    };
                    let r = root.clone();
                    let st = state.clone();
                    let eng = engine.clone();
                    std::thread::spawn(move || {
                        let _permit = permit;
                        handle(stream, Some(&r), &st, eng.as_ref());
                    });
                }
                Err(e) => log::warn!("accept error: {}", e),
            }
        }
    });
    Some(port)
}


/// API-only listener. Caller retains the data-directory lock.
pub fn serve_api(listener: TcpListener, state: ModelState, engine: EngineHandle) -> std::io::Result<()> {
    for connection in listener.incoming() {
        let stream = connection?;
        // 资源上限:超时由 admit_connection 设置,并发超限立即 503 且不排队。
        let Some((stream, permit)) = admit_connection(stream) else {
            continue;
        };
        let state = state.clone();
        let engine = engine.clone();
        std::thread::spawn(move || {
            let _permit = permit;
            handle(stream, None, &state, Some(&engine));
        });
    }
    Ok(())
}

#[cfg(test)]
mod limits_tests {
    use super::*;
    use std::io::Read;

    /// 并发计数是**进程级**静态量:测试必须串行,否则一个测试占满名额会污染其他测试。
    static TEST_GATE: Mutex<()> = Mutex::new(());

    fn test_gate() -> std::sync::MutexGuard<'static, ()> {
        TEST_GATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// 起一个线程跑 API-only 监听器;返回端口。线程随测试结束由进程回收。
    fn spawn_listener() -> u16 {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        let port = listener.local_addr().expect("local addr").port();
        std::thread::spawn(move || {
            let state = ModelState::new();
            let engine = EngineHandle {
                engine: Arc::new(Mutex::new(
                    crate::engine::GraphEngine::new(":memory:").expect("in-memory engine"),
                )),
            };
            let _ = serve_api(listener, state, engine);
        });
        port
    }

    fn connect(port: u16) -> TcpStream {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .expect("read timeout");
        stream
    }

    /// 读到 headers 结束(或对端关闭),返回头部原文。
    fn read_head(stream: &mut TcpStream) -> String {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            match stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn status_line(head: &str) -> &str {
        head.split("\r\n").next().unwrap_or("")
    }

    /// 断言错误响应统一:JSON + charset + 有 Content-Length。
    fn assert_json_error(head: &str, expected_status: u16) {
        let status = format!("HTTP/1.1 {}", expected_status);
        assert!(
            status_line(head).starts_with(&status),
            "expected {} but got {:?}",
            status,
            status_line(head)
        );
        let lower = head.to_ascii_lowercase();
        assert!(
            lower.contains("content-type: application/json; charset=utf-8"),
            "missing JSON content-type in {:?}",
            head
        );
        assert!(
            lower.contains("content-length:"),
            "missing content-length in {:?}",
            head
        );
    }

    #[test]
    fn oversized_headers_are_rejected_with_431() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        // 头部超过 32 KiB:一个 40 KiB 的自定义头。
        let filler = "x".repeat(40 * 1024);
        let request = format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Filler: {}\r\n\r\n", filler);
        let _ = stream.write_all(request.as_bytes());
        let head = read_head(&mut stream);
        assert_json_error(&head, 431);
    }

    #[test]
    fn oversized_body_is_rejected_with_413_while_reading() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        // 声明超过 1 MiB 的正文 → 读取阶段立即 413,不需要真的发送正文。
        let request = format!(
            "POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let _ = stream.write_all(request.as_bytes());
        let head = read_head(&mut stream);
        assert_json_error(&head, 413);
    }

    #[test]
    fn lying_content_length_is_rejected_with_413() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        // "谎报":声明巨大但只发几个字节 —— 声明阶段就直接 413,不读正文。
        let request = "POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 999999999\r\n\r\n{}";
        let _ = stream.write_all(request.as_bytes());
        let head = read_head(&mut stream);
        assert_json_error(&head, 413);
    }

    /// 声明略超上限(只比 1 MiB 多 1 字节)也必须在读取阶段拒绝,
    /// 而不是先 read_to_end 再检查。
    #[test]
    fn body_one_byte_over_limit_is_rejected_with_413() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let request = format!(
            "POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY_BYTES + 1
        );
        let _ = stream.write_all(request.as_bytes());
        let head = read_head(&mut stream);
        assert_json_error(&head, 413);
    }

    /// 响应头结束且正文字节数达到 Content-Length 时视为读完整。
    fn response_is_complete(buf: &[u8]) -> bool {
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            return false;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
        let declared = head
            .split("\r\n")
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok());
        matches!(declared, Some(len) if buf.len() >= end + 4 + len)
    }

    /// 真实客户端场景(P3-F):声明**并实际发送**远超 1 MiB 的正文,正文在独立
    /// 线程里推送。服务端在读取中途就 413,然后半关闭写端并排空;客户端必须能
    /// 读到完整 413,而不是 `ECONNRESET`。发送线程的 write 可能因为服务端停止
    /// 接收而失败,这正是不稳定行为的表现,不影响"响应是否可读"的断言。
    fn oversized_body_response(port: u16, size: usize) -> String {
        let mut stream = connect(port);
        let request = format!(
            "POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            size
        );
        let mut writer = stream.try_clone().expect("clone stream for sender");
        let sender = std::thread::spawn(move || {
            let mut payload = request.into_bytes();
            payload.resize(payload.len() + size, b'a');
            let _ = writer.write_all(&payload);
            let _ = writer.shutdown(std::net::Shutdown::Write);
        });
        let mut buf = Vec::new();
        let mut tmp = [0u8; 64 * 1024];
        loop {
            match stream.read(&mut tmp) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if response_is_complete(&buf) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = sender.join();
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// P3-F 回归:声明并实际发送 1 MiB + 3 / 2 MiB / 8 MiB 正文时,**每一轮**都必须
    /// 收到可读的 413 全文(此前同一二进制会在部分轮次只发 RST,客户端 `ECONNRESET`)。
    #[test]
    fn oversized_bodies_always_answer_with_a_readable_413() {
        let _gate = test_gate();
        let port = spawn_listener();
        let expected = format!(
            r#"{{"error":"request body must not exceed {} bytes"}}"#,
            MAX_BODY_BYTES
        );
        for size in [MAX_BODY_BYTES + 3, 2 * MAX_BODY_BYTES, 8 * MAX_BODY_BYTES] {
            for round in 1..=3 {
                let response = oversized_body_response(port, size);
                assert!(
                    response.starts_with("HTTP/1.1 413 Payload Too Large"),
                    "size={} round={} expected a readable 413, got {:?}",
                    size,
                    round,
                    &response[..response.len().min(120)]
                );
                assert!(
                    response.contains(&expected),
                    "size={} round={} missing JSON error body in {:?}",
                    size,
                    round,
                    &response[..response.len().min(200)]
                );
            }
        }
    }

    /// 同一监听端口连续多轮(等价于连接池/客户端复用服务端)都必须拿到可读的 413,
    /// 且中途插入的正常请求不受影响。
    #[test]
    fn repeated_oversized_requests_stay_readable_and_service_recovers() {
        let _gate = test_gate();
        let port = spawn_listener();
        for round in 1..=6 {
            let response = oversized_body_response(port, 2 * MAX_BODY_BYTES);
            assert!(
                response.starts_with("HTTP/1.1 413"),
                "round {} expected 413, got {:?}",
                round,
                &response[..response.len().min(120)]
            );
        }
        let mut stream = connect(port);
        let _ = stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let head = read_head(&mut stream);
        assert!(
            status_line(&head).starts_with("HTTP/1.1 200"),
            "service did not recover: {:?}",
            status_line(&head)
        );
    }

    /// 不可解析的 Content-Length → 400(不 panic,不按 0 继续处理)。
    #[test]
    fn invalid_content_length_header_is_rejected_with_400() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let request = "POST /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: not-a-number\r\n\r\n{}";
        let _ = stream.write_all(request.as_bytes());
        let head = read_head(&mut stream);
        assert_json_error(&head, 400);
    }

    /// 头部行没有冒号(畸形 header)→ 400。
    #[test]
    fn malformed_header_line_is_rejected_with_400() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let _ = stream.write_all(b"GET /api/health HTTP/1.1\r\nBroken-Header\r\n\r\n");
        let head = read_head(&mut stream);
        assert_json_error(&head, 400);
    }

    #[test]
    fn disallowed_api_method_returns_405_with_allow_header() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let _ = stream.write_all(b"DELETE /api/v1/projects HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let head = read_head(&mut stream);
        assert_json_error(&head, 405);
        assert!(
            head.to_ascii_lowercase().contains("allow: get, post"),
            "405 must carry Allow header, got {:?}",
            head
        );
    }

    #[test]
    fn garbage_request_line_returns_400_without_panicking() {
        let _gate = test_gate();
        let port = spawn_listener();
        for request in [
            "GARBAGE-NO-SPACES\r\n\r\n",
            "GET\r\n\r\n",
            "GET /ok\r\n\r\n",
            "GET  HTTP/1.1\r\n\r\n",
            "GET /x HTTP/2.0\r\n\r\n",
        ] {
            let mut stream = connect(port);
            let _ = stream.write_all(request.as_bytes());
            let head = read_head(&mut stream);
            assert_json_error(&head, 400);
        }
    }

    /// 并发闸门必须**立即** 503(不排队):先把 64 个名额全部占满(直接占闸门,
    /// 避免依赖 15s 读超时带来的时序不确定),再走真实套接字验证第 65 条连接。
    /// 释放一个名额后,同一路径必须立刻恢复 200(证明 503 只是限流,不是拒绝服务)。
    #[test]
    fn concurrency_limit_rejects_excess_connections_with_503() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut permits: Vec<ConnectionPermit> = Vec::new();
        for index in 0..MAX_CONCURRENT_CONNECTIONS {
            permits.push(
                ConnectionGate::acquire()
                    .unwrap_or_else(|| panic!("permit {} of the limit was refused", index)),
            );
        }
        assert_eq!(ACTIVE_CONNECTIONS.load(Ordering::Acquire), MAX_CONCURRENT_CONNECTIONS);
        // 名额已满:下一条真实连接必须在读取请求之前就被 503 拒绝。
        let mut refused = connect(port);
        let _ = refused.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let head = read_head(&mut refused);
        assert_json_error(&head, 503);
        drop(refused);
        // 释放一个名额:服务必须马上恢复,而不是继续拒绝。
        permits.pop();
        let mut recovered = connect(port);
        let _ = recovered.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let head = read_head(&mut recovered);
        assert!(
            status_line(&head).starts_with("HTTP/1.1 200"),
            "service did not recover after a permit was released, got {:?}",
            head
        );
        drop(recovered);
        drop(permits);
        for _ in 0..50 {
            if ACTIVE_CONNECTIONS.load(Ordering::Acquire) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            ACTIVE_CONNECTIONS.load(Ordering::Acquire),
            0,
            "connection permits were not released"
        );
    }

    #[test]
    fn request_line_over_limit_returns_431() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let path = format!("/{}", "a".repeat(MAX_REQUEST_LINE_BYTES + 100));
        let _ = stream.write_all(format!("GET {} HTTP/1.1\r\n\r\n", path).as_bytes());
        let head = read_head(&mut stream);
        assert_json_error(&head, 431);
    }

    /// 正常请求仍然工作:资源上限不能把合法流量挡掉。
    #[test]
    fn well_formed_request_still_succeeds() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let _ = stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n");
        let head = read_head(&mut stream);
        assert!(
            status_line(&head).starts_with("HTTP/1.1 200"),
            "well-formed request must still succeed, got {:?}",
            head
        );
        assert!(head.to_ascii_lowercase().contains("content-type: application/json"));
    }

    /// 连接建立后 15s 不发完整请求 → 408(真实等待,验证 socket 超时确实生效)。
    #[test]
    fn idle_connection_times_out_with_408() {
        let _gate = test_gate();
        let port = spawn_listener();
        let mut stream = connect(port);
        let _ = stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Incomplete: 1\r\n");
        let started = std::time::Instant::now();
        let head = read_head(&mut stream);
        let elapsed = started.elapsed();
        assert_json_error(&head, 408);
        assert!(
            elapsed >= CONNECTION_TIMEOUT - Duration::from_secs(1),
            "408 came back after {:?}, before the {}s deadline",
            elapsed,
            CONNECTION_TIMEOUT.as_secs()
        );
    }
}
