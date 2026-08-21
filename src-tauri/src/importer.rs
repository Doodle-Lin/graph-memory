// Graph Memory — 记忆导入器(Rust 版)
//
// 镜像 Python graph_memory/importer.py 的逻辑,支持三源:
//   - hermes  : MEMORY.md / USER.md(§ 分隔)+ skills 目录
//   - claude  : ~/.claude/projects/*/memory/*.md(YAML frontmatter)
//   - codex   : ~/.codex/history.jsonl(质量过滤)
//
// 路径解析(与 Python config.py 对齐,多路径回退):
//   HERMES_HOME 环境变量 → ~/.hermes → Windows LOCALAPPDATA/Hermes Agent CN Desktop/...
//   CLAUDE_HOME 环境变量 → ~/.claude
//   CODEX_HOME  环境变量 → ~/.codex

use crate::engine::GraphEngine;
use std::path::PathBuf;

pub struct ImportResult {
    pub source: String,
    pub total: usize,
    pub errors: Vec<String>,
}

fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// 解析 Hermes 记忆根目录:HERMES_HOME → ~/.hermes → Windows 桌面端默认路径
fn hermes_home() -> PathBuf {
    if let Ok(h) = std::env::var("HERMES_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let h = home_dir();
    let dot_hermes = h.join(".hermes");
    if dot_hermes.join("memories").exists() || dot_hermes.join("config.yaml").exists() {
        return dot_hermes;
    }
    // Windows 桌面端默认安装路径(Hermes Agent CN Desktop 的标准数据位置)
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let desktop = PathBuf::from(local).join("Hermes Agent CN Desktop/data/hermes-home");
        if desktop.join("memories").exists() {
            return desktop;
        }
    }
    dot_hermes
}

fn claude_home() -> PathBuf {
    std::env::var("CLAUDE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".claude"))
}

fn codex_home() -> PathBuf {
    std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".codex"))
}

/// 质量过滤:太短 / 纯语气词 / 斜杠指令 → 噪声
fn is_noise(text: &str, min_len: usize) -> bool {
    let t = text.trim();
    if t.chars().count() < min_len {
        return true;
    }
    let lower = t.to_lowercase();
    let noise_words = [
        "ok", "okay", "好的", "嗯", "收到", "明白", "了解", "继续", "跳过",
        "不懂", "不会", "在吗", "在的", "zaima", "zenme", "111", "222", "333",
    ];
    if noise_words.iter().any(|w| lower == *w || lower.starts_with(w) && lower.chars().count() < 8) {
        return true;
    }
    if t.starts_with('/') && t.chars().count() < 20 {
        return true; // 斜杠指令
    }
    let noise_prefixes = [
        "Launching skill:", "Exit code", "Base directory",
        "Model metadata for", "UserWarning:", "[TOOL]", "run_command",
    ];
    if noise_prefixes.iter().any(|p| t.starts_with(p)) {
        return true;
    }
    false
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ── Hermes ────────────────────────────────────────────────

/// MEMORY.md / USER.md:§ 分隔条目
fn parse_hermes_md(content: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for part in content.split('\u{a7}') {
        let part = part.trim();
        if part.is_empty() || part.chars().count() < 5 {
            continue;
        }
        let title: String = if let Some(c) = part.find(|c: char| c == ':' || c == '\u{ff1a}') {
            part[..c].trim().to_string()
        } else {
            part.chars().take(40).collect()
        };
        entries.push((title, part.to_string()));
    }
    entries
}

pub fn import_hermes(engine: &mut GraphEngine) -> ImportResult {
    let mut total = 0;
    let mut errors = Vec::new();
    let mem_dir = hermes_home().join("memories");

    if mem_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&mem_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let is_user = name.contains("USER");
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(e) => { errors.push(format!("{}: {}", name, e)); continue; }
                };
                let node_type = if is_user { "preference" } else { "knowledge" };
                let source_tag = if is_user { "hermes_user" } else { "hermes" };
                for (title, body) in parse_hermes_md(&content) {
                    if is_noise(&body, 15) {
                        continue;
                    }
                    let t = if title.is_empty() {
                        path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                    } else { title };
                    match engine.add_node_raw(&body, &t, node_type, source_tag) {
                        Ok(_) => total += 1,
                        Err(e) => errors.push(format!("{}: {}", name, e)),
                    }
                }
            }
        }
    } else {
        errors.push(format!("memories dir not found: {}", mem_dir.display()));
    }

    // skills 目录
    let skills_dir = hermes_home().join("skills");
    if skills_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let skill_md = entry.path().join("SKILL.md");
                if !skill_md.exists() {
                    continue;
                }
                let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                let name = entry.file_name().to_string_lossy().to_string();
                let title = content.lines()
                    .find(|l| l.starts_with('#'))
                    .map(|l| l.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| name.clone());
                let snippet: String = content.chars().take(500).collect();
                if snippet.chars().count() > 30 {
                    match engine.add_node_raw(&snippet, &title, "skill", "hermes_skill") {
                        Ok(_) => total += 1,
                        Err(e) => errors.push(format!("skill {}: {}", name, e)),
                    }
                }
            }
        }
    }

    ImportResult { source: "hermes".into(), total, errors }
}

// ── Claude Code ────────────────────────────────────────────

/// 解析 YAML frontmatter + markdown body
fn parse_claude_md(content: &str) -> Option<(String, String, String)> {
    if !content.starts_with("---") {
        return None;
    }
    let parts: Vec<&str> = content.splitn(3, "---").collect();
    if parts.len() < 3 {
        return None;
    }
    let fm = parts[1];
    let body = parts[2].trim();
    let mut name = String::new();
    let mut raw_type = "knowledge".to_string();
    for line in fm.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("name:") {
            name = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("type:") {
            raw_type = v.trim().to_string();
        }
    }
    // 旧类型映射到 6 类
    let mapped = match raw_type.as_str() {
        "user" | "feedback" => "reference",
        "memory" | "entity" | "history" | "session" => "knowledge",
        other => other,
    };
    let node_type = if is_valid_type(mapped) { mapped } else { "knowledge" };
    let title = if name.is_empty() { body.chars().take(40).collect() } else { name };
    Some((title, body.to_string(), node_type.to_string()))
}

pub fn import_claude(engine: &mut GraphEngine) -> ImportResult {
    let mut total = 0;
    let mut errors = Vec::new();
    let projects_dir = claude_home().join("projects");
    if !projects_dir.exists() {
        return ImportResult { source: "claude".into(), total, errors: vec![format!("not found: {}", projects_dir.display())] };
    }

    let mut md_files: Vec<PathBuf> = Vec::new();
    collect_md_recursive(&projects_dir, &mut md_files);

    for md_file in md_files {
        let content = match std::fs::read_to_string(&md_file) {
            Ok(c) => c,
            Err(e) => { errors.push(format!("{}: {}", md_file.display(), e)); continue; }
        };
        if let Some((title, body, node_type)) = parse_claude_md(&content) {
            if is_noise(&body, 15) {
                continue;
            }
            let fname = md_file.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            match engine.add_node_raw(&body, &title, &node_type, "claude") {
                Ok(_) => total += 1,
                Err(e) => errors.push(format!("{}: {}", fname, e)),
            }
        }
    }

    ImportResult { source: "claude".into(), total, errors }
}

fn collect_md_recursive(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // 只进 memory 子目录或继续递归找 .md
                collect_md_recursive(&path, out);
            } else if path.extension().map(|e| e == "md").unwrap_or(false) {
                out.push(path);
            }
        }
    }
}

// ── Codex ─────────────────────────────────────────────────

pub fn import_codex(engine: &mut GraphEngine) -> ImportResult {
    let mut total = 0;
    let mut errors = Vec::new();
    let hist_file = codex_home().join("history.jsonl");
    if !hist_file.exists() {
        return ImportResult { source: "codex".into(), total, errors: vec![format!("not found: {}", hist_file.display())] };
    }
    let content = match std::fs::read_to_string(&hist_file) {
        Ok(c) => c,
        Err(e) => return ImportResult { source: "codex".into(), total, errors: vec![e.to_string()] },
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let obj: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let text = obj.get("text").and_then(|t| t.as_str()).unwrap_or("");
        if is_noise(text, 50) {
            continue;
        }
        let title: String = text.chars().take(40).collect();
        match engine.add_node_raw(text, &title, "knowledge", "codex") {
            Ok(_) => total += 1,
            Err(e) => errors.push(e),
        }
    }
    ImportResult { source: "codex".into(), total, errors }
}

pub fn import_all(engine: &mut GraphEngine) -> serde_json::Value {
    import_all_with_opts(engine, false)
}

/// 导入所有来源。enrich=true 时,导入后立即补 embedding + auto_link 建边。
/// (模型未就绪时 enrich 无效,节点先 raw 入库,待模型就绪后由 enrich_all 补全)
pub fn import_all_with_opts(engine: &mut GraphEngine, enrich: bool) -> serde_json::Value {
    let mut grand_total = 0;
    let mut all_errors: Vec<String> = Vec::new();
    let mut sources = serde_json::json!({});

    for r in [import_hermes(engine), import_claude(engine), import_codex(engine)] {
        grand_total += r.total;
        all_errors.extend(r.errors.into_iter().take(20).map(|e| format!("{}: {}", r.source, e)));
        sources[r.source] = serde_json::json!({ "nodes": r.total });
    }

    let mut enriched = serde_json::json!({"embeddings": 0, "edges": 0});
    if enrich {
        match engine.enrich_all() {
            Ok((embs, edges)) => {
                enriched = serde_json::json!({"embeddings": embs, "edges": edges});
            }
            Err(e) => all_errors.push(format!("enrich: {}", e)),
        }
    }

    serde_json::json!({
        "total_nodes": grand_total,
        "sources": sources,
        "enrich": enriched,
        "errors": all_errors,
        "message": format!("导入 {} 个节点", grand_total),
    })
}

fn is_valid_type(t: &str) -> bool {
    matches!(t, "knowledge" | "preference" | "project" | "fact" | "skill" | "reference")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn parse_hermes_md_splits_on_section() {
        let content = "Title1: body one\n§\nTitle2: body two longer than five chars";
        let entries = parse_hermes_md(content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "Title1");
        assert!(entries[1].1.contains("body two"));
    }

    #[test]
    fn parse_hermes_md_skips_short() {
        let content = "ab\n§\nlong enough body here yes";
        let entries = parse_hermes_md(content);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn parse_claude_md_extracts_frontmatter() {
        let content = "---\nname: my-node\ntype: reference\n---\nbody content here";
        let (title, body, nt) = parse_claude_md(content).unwrap();
        assert_eq!(title, "my-node");
        assert_eq!(nt, "reference");
        assert!(body.contains("body content"));
    }

    #[test]
    fn parse_claude_md_maps_legacy_types() {
        let content = "---\nname: n\ntype: user\n---\nsome body content";
        let (_, _, nt) = parse_claude_md(content).unwrap();
        assert_eq!(nt, "reference");
    }

    #[test]
    fn is_noise_filters_short_and_phrases() {
        assert!(is_noise("ok", 15));
        assert!(is_noise("好的", 15));
        assert!(is_noise("zaima", 15));
        assert!(is_noise("/help", 15));
        assert!(!is_noise("这是一段足够长的有意义技术内容描述", 15));
    }

    #[test]
    fn import_hermes_reads_real_memory_md() {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let mem = std::path::PathBuf::from(&local).join("Hermes Agent CN Desktop/data/hermes-home/memories/MEMORY.md");
        if !mem.exists() { eprintln!("skip: no MEMORY.md"); return; }
        let content = fs::read_to_string(&mem).unwrap();
        let entries = parse_hermes_md(&content);
        assert!(!entries.is_empty());
        eprintln!("MEMORY.md -> {} entries", entries.len());
    }
}

#[cfg(test)]
mod integration {
    use super::*;
    use std::path::PathBuf;

    fn tmp_db(tag: &str) -> (PathBuf, crate::engine::GraphEngine) {
        let p = std::env::temp_dir().join(format!("gm_e2e_{}_{}.db", tag, std::process::id()));
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(format!("{}-wal", p.display()));
        let _ = std::fs::remove_file(format!("{}-shm", p.display()));
        let eng = crate::engine::GraphEngine::new(p.to_str().unwrap()).unwrap();
        (p, eng)
    }

    #[test]
    fn import_all_writes_real_data() {
        let (db, mut eng) = tmp_db("all");
        let result = import_all(&mut eng);
        let total = result["total_nodes"].as_u64().unwrap_or(0);
        eprintln!("import_all total={} sources={}", total, result["sources"]);
        // 本机三个源都在,至少应导入若干节点。宽松断言 > 0,避免无记忆机器上 CI 失败
        // 但本机确实有数据,这里若 0 就是 bug。
        assert!(total > 0, "import_all 应导入 >0 个节点,实际 {}", total);
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn import_hermes_writes_real_memory_md() {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        let mem = PathBuf::from(&local).join("Hermes Agent CN Desktop/data/hermes-home/memories/MEMORY.md");
        if !mem.exists() { eprintln!("skip"); return; }
        let (db, mut eng) = tmp_db("hermes");
        let r = import_hermes(&mut eng);
        eprintln!("hermes total={} errors={}", r.total, r.errors.len());
        assert!(r.total > 0, "hermes 应导入 MEMORY.md 条目");
        let _ = std::fs::remove_file(&db);
    }

    #[test]
    fn import_claude_writes_real_md() {
        let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
        let cp = PathBuf::from(&home).join(".claude/projects");
        if !cp.exists() { eprintln!("skip"); return; }
        let (db, mut eng) = tmp_db("claude");
        let r = import_claude(&mut eng);
        eprintln!("claude total={} errors={}", r.total, r.errors.len());
        assert!(r.total > 0, "claude 应导入 memory/*.md");
        let _ = std::fs::remove_file(&db);
    }
}
