// Graph Memory — LLM 知识提炼器(Rust 版)
//
// 镜像 Python graph_memory/llm_extract.py 的逻辑:
// 用 OpenAI 兼容 API 从原始对话/记忆文本中提取结构化知识节点 + 关系,
// 写入图引擎(走 add_node:含 embedding 三层去重 + auto_link)。
//
// 配置:GM_LLM_API_KEY / GM_LLM_BASE_URL / GM_LLM_MODEL 环境变量(见 .env.example)。
// 不配置则返回 error:no_api_key,不影响检索/写入。

use crate::engine::GraphEngine;
use anyhow::{Context, Result};
use serde_json::Value;

const EXTRACT_PROMPT: &str = r#"你是一个知识提炼专家。从下面的对话/记忆文本中提取有价值的知识节点和它们之间的关系。

## 提取规则

1. 只提取有信息量的知识 — 跳过寒暄、命令、碎片
2. 每条知识应能独立成立 — 不依赖对话上下文也能理解
3. 合并重复信息 — 同一个知识点在对话中反复出现,只提取一次
4. 保留技术细节 — 服务器地址、端口号、命令、路径、配置参数都是有价值的
5. 标注节点类型(只能从以下 6 种中选择):
   - knowledge: 技术原理/概念
   - preference: 用户偏好/习惯
   - project: 项目信息
   - fact: 环境/配置事实
   - skill: 技能/工具用法
   - reference: 参考资料/路径/经验教训
   注意:不要使用 session/history/feedback/user 等类型,只有上述 6 种。

6. 标注关系 — 如果两个节点有明显关联,标注 relation:
   related_to / depends_on / part_of / derived_from / same_topic

## 输出格式(严格 JSON,不要 markdown 代码块)

{"nodes": [{"title": "简短标题", "content": "完整知识内容", "type": "knowledge"}],
 "edges": [{"source_title": "节点A标题", "target_title": "节点B标题", "relation": "related_to"}]}

如果文本没有有价值的知识,返回 {"nodes": [], "edges": []}

## 待提炼文本

来源: {source}
"#;

pub struct LlmConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

fn load_llm_config() -> Option<LlmConfig> {
    let k = std::env::var("GM_LLM_API_KEY").ok().filter(|s| !s.is_empty());
    let b = std::env::var("GM_LLM_BASE_URL").ok().filter(|s| !s.is_empty());
    let m = std::env::var("GM_LLM_MODEL").ok().filter(|s| !s.is_empty());
    match (k, b, m) {
        (Some(k), Some(b), Some(m)) => Some(LlmConfig { api_key: k, base_url: b, model: m }),
        _ => None,
    }
}

/// 公开接口:加载 LLM 配置(供 static_server 调用)
pub fn load_config() -> Option<LlmConfig> {
    load_llm_config()
}

const REFINE_PROMPT: &str = r#"你是一个知识提炼专家。将下面的原始记忆文本提炼成一个简洁的知识节点。

## 规则
1. title: 用 5-20 字概括核心知识点(不是截断原文,是提炼!)
   - 例如:"vLLM 部署偏好" 而非 "用户说要用vLLM来部署..."
2. content: 保留完整信息,去掉对话噪音(寒暄、命令、碎片),保留技术细节
3. 不要用会话名/文件名作为标题,要从内容本身提炼
4. 只输出两行,严格格式:
   TITLE: 标题
   CONTENT: 内容

## 原始文本(来源: {source})"#;

/// 用 LLM 提炼单个节点的标题和内容。返回 (new_title, new_content)
pub fn refine_node(content: &str, source: &str, cfg: &LlmConfig) -> Result<(String, String)> {
    let prompt = REFINE_PROMPT.replace("{source}", source);
    let user_content = format!("{}\n{}", prompt, &content[..content.len().min(4000)]);

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": "你是知识提炼专家。只输出 TITLE: 和 CONTENT: 两行。"},
            {"role": "user", "content": user_content}
        ],
        "max_tokens": 2048,
        "temperature": 0.3
    });

    let resp = client
        .post(format!("{}/chat/completions", cfg.base_url))
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .json(&body)
        .send()?;

    let json: Value = resp.json()?;
    let text = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let text = if text.is_empty() {
        json["choices"][0]["message"]["reasoning_content"]
            .as_str()
            .unwrap_or("")
            .to_string()
    } else { text };

    // 解析 TITLE: 和 CONTENT:
    let mut title = String::new();
    let mut content_out = String::new();
    for line in text.lines() {
        if let Some(t) = line.strip_prefix("TITLE:") {
            title = t.trim().to_string();
        } else if let Some(c) = line.strip_prefix("CONTENT:") {
            content_out = c.trim().to_string();
        }
    }

    if title.is_empty() {
        anyhow::bail!("no TITLE in response: {}", &text[..text.len().min(100)]);
    }
    if content_out.is_empty() {
        content_out = content.chars().take(200).collect();
    }

    Ok((title, content_out))
}

/// 调用 LLM 提炼知识,返回 {nodes, edges} 或 {error}
fn call_llm(text: &str, source: &str, cfg: &LlmConfig) -> Value {
    let prompt = EXTRACT_PROMPT.replace("{source}", source);
    let user_content = format!("{}\n{}", prompt, &text[..text.len().min(8000)]);

    let client = match reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build() {
        Ok(c) => c,
        Err(e) => return serde_json::json!({"error": e.to_string(), "nodes": [], "edges": []}),
    };

    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": "你是知识提炼专家,只输出JSON。"},
            {"role": "user", "content": user_content}
        ],
        "max_tokens": 4096,
        "temperature": 0.3
    });

    let resp = match client
        .post(format!("{}/chat/completions", cfg.base_url))
        .header("Authorization", format!("Bearer {}", cfg.api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
    {
        Ok(r) => r,
        Err(e) => return serde_json::json!({"error": e.to_string(), "nodes": [], "edges": []}),
    };

    let json: Value = match resp.json() {
        Ok(j) => j,
        Err(e) => return serde_json::json!({"error": e.to_string(), "nodes": [], "edges": []}),
    };

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // 兜底:某些推理模型把正文留在 reasoning_content
    let content = if content.is_empty() {
        json["choices"][0]["message"]["reasoning_content"]
            .as_str()
            .unwrap_or("")
            .to_string()
    } else { content };

    if content.is_empty() {
        return serde_json::json!({"error": "empty_response", "nodes": [], "edges": []});
    }

    // 去 ```json 包裹
    let mut c = content.trim().to_string();
    if c.starts_with("```json") { c = c.trim_start_matches("```json").trim().to_string(); }
    if c.starts_with("```") { c = c.trim_start_matches("```").trim().to_string(); }
    if c.ends_with("```") { c = c.trim_end_matches("```").trim().to_string(); }

    // 截取第一个 { 到最后一个 }
    let start = c.find('{');
    let end = c.rfind('}');
    match (start, end) {
        (Some(s), Some(e)) if e > s => {
            let json_str = &c[s..=e];
            match serde_json::from_str::<Value>(json_str) {
                Ok(v) => v,
                Err(err) => serde_json::json!({
                    "error": format!("json parse: {}", err),
                    "nodes": [], "edges": [],
                    "raw": &c[..c.len().min(200)]
                }),
            }
        }
        _ => serde_json::json!({
            "error": "no_json_in_response",
            "nodes": [], "edges": [],
            "raw": &c[..c.len().min(200)]
        }),
    }
}

/// 提炼一段文本并导入图引擎。返回汇总。
pub fn extract_and_import(engine: &mut GraphEngine, text: &str, source: &str, max_links: usize) -> Value {
    let cfg = match load_llm_config() {
        Some(c) => c,
        None => return serde_json::json!({
            "extracted": {"error": "no_api_key", "nodes": [], "edges": []},
            "imported": {"nodes_created": 0, "edges_created": 0},
            "error": "未配置 GM_LLM_* 环境变量,无法提炼。检索/写入不受影响。"
        }),
    };

    let extracted = call_llm(text, source, &cfg);
    if extracted.get("error").is_some() {
        return serde_json::json!({
            "extracted": extracted,
            "imported": {"nodes_created": 0, "edges_created": 0}
        });
    }

    let mut title_to_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut nodes_created = 0;
    let mut edges_created = 0;

    if let Some(nodes) = extracted["nodes"].as_array() {
        for n in nodes {
            let title = n.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let content = n.get("content").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let ntype = n.get("type").and_then(|v| v.as_str()).unwrap_or("knowledge");
            if content.len() < 10 { continue; }
            let src = format!("llm_extract:{}", source);
            match engine.add_node(&content, &title, ntype, &src, "{}") {
                Ok(node) => {
                    title_to_id.insert(title, node.id.clone());
                    let links = engine.auto_link(&node.id, max_links).unwrap_or_default();
                    nodes_created += 1;
                    edges_created += links.len();
                }
                Err(e) => log::warn!("add_node failed: {}", e),
            }
        }
    }

    // 写入 LLM 标注的关系边
    if let Some(edges) = extracted["edges"].as_array() {
        for e in edges {
            let src_title = e.get("source_title").and_then(|v| v.as_str()).unwrap_or("");
            let tgt_title = e.get("target_title").and_then(|v| v.as_str()).unwrap_or("");
            let relation = e.get("relation").and_then(|v| v.as_str()).unwrap_or("related_to");
            if let (Some(src_id), Some(tgt_id)) = (title_to_id.get(src_title), title_to_id.get(tgt_title)) {
                if let Ok(_) = engine.add_edge(src_id, tgt_id, relation, 0.8, "{}") {
                    edges_created += 1;
                }
            }
        }
    }

    serde_json::json!({
        "extracted": extracted,
        "imported": {"nodes_created": nodes_created, "edges_created": edges_created}
    })
}
