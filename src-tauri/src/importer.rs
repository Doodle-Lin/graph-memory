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

/// 解析 Hermes 记忆根目录:HERMES_HOME → ~/.hermes(有 memories) →
/// Windows %LOCALAPPDATA%\hermes(NousResearch CLI)→ Hermes Agent CN Desktop → 回退 ~/.hermes
fn hermes_home() -> PathBuf {
    if let Ok(h) = std::env::var("HERMES_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let h = home_dir();
    let dot_hermes = h.join(".hermes");
    // POSIX 上 ~/.hermes 是标准位置;Windows 上 ~/.hermes 可能是骨架,只在真有 memories 时才认
    if dot_hermes.join("memories").exists() {
        return dot_hermes;
    }
    // Windows:NousResearch hermes-agent CLI,记忆在 %LOCALAPPDATA%\hermes\memories
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let cli = PathBuf::from(&local).join("hermes");
        if cli.join("memories").exists() {
            return cli;
        }
        // Hermes Agent CN Desktop 桌面端默认路径
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
    import_hermes_from(engine, &hermes_home())
}

fn import_hermes_from(engine: &mut GraphEngine, root: &std::path::Path) -> ImportResult {
    let mut total = 0;
    let mut errors = Vec::new();
    let mem_dir = root.join("memories");

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
    let skills_dir = root.join("skills");
    if skills_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&skills_dir) {
            for entry in entries.flatten() {
                let skill_md = entry.path().join("SKILL.md");
                if !skill_md.exists() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                // 跳过 graph-memory 自己的 SKILL.md(它是行为指令,不是知识)
                if name.contains("graph-memory") || name.contains("graph_memory") {
                    continue;
                }
                let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                let title = content.lines()
                    .find(|l| l.starts_with('#'))
                    .map(|l| l.trim_start_matches('#').trim().to_string())
                    .unwrap_or_else(|| name.clone());
                // SKILL.md 可能很长,取前 2000 字符(原来是 500,太短会截断有用信息)
                let snippet: String = content.chars().take(2000).collect();
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
    import_claude_from(engine, &claude_home())
}

fn import_claude_from(engine: &mut GraphEngine, root: &std::path::Path) -> ImportResult {
    let mut total = 0;
    let mut errors = Vec::new();
    let projects_dir = root.join("projects");
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
    import_codex_from(engine, &codex_home())
}

fn import_codex_from(engine: &mut GraphEngine, root: &std::path::Path) -> ImportResult {
    let mut total = 0;
    let mut errors = Vec::new();
    let hist_file = root.join("history.jsonl");
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
    import_all_from(engine, enrich, &hermes_home(), &claude_home(), &codex_home())
}

fn import_all_from(engine: &mut GraphEngine, enrich: bool, hermes: &std::path::Path, claude: &std::path::Path, codex: &std::path::Path) -> serde_json::Value {
    let t_start = std::time::Instant::now();
    let mut grand_total = 0;
    let mut all_errors: Vec<String> = Vec::new();
    let mut sources = serde_json::json!({});

    let t_hermes = std::time::Instant::now();
    let r_h = import_hermes_from(engine, hermes);
    log::info!("import_all[perf] hermes: {} nodes, {} errors in {:?}", r_h.total, r_h.errors.len(), t_hermes.elapsed());
    grand_total += r_h.total;
    all_errors.extend(r_h.errors.into_iter().take(20).map(|e| format!("hermes: {}", e)));
    sources["hermes"] = serde_json::json!({ "nodes": r_h.total });

    let t_claude = std::time::Instant::now();
    let r_c = import_claude_from(engine, claude);
    log::info!("import_all[perf] claude: {} nodes, {} errors in {:?}", r_c.total, r_c.errors.len(), t_claude.elapsed());
    grand_total += r_c.total;
    all_errors.extend(r_c.errors.into_iter().take(20).map(|e| format!("claude: {}", e)));
    sources["claude"] = serde_json::json!({ "nodes": r_c.total });

    let t_codex = std::time::Instant::now();
    let r_x = import_codex_from(engine, codex);
    log::info!("import_all[perf] codex: {} nodes, {} errors in {:?}", r_x.total, r_x.errors.len(), t_codex.elapsed());
    grand_total += r_x.total;
    all_errors.extend(r_x.errors.into_iter().take(20).map(|e| format!("codex: {}", e)));
    sources["codex"] = serde_json::json!({ "nodes": r_x.total });

    let mut enriched = serde_json::json!({"embeddings": 0, "edges": 0});
    if enrich {
        let t_enrich = std::time::Instant::now();
        match engine.enrich_all() {
            Ok((embs, edges)) => {
                enriched = serde_json::json!({"embeddings": embs, "edges": edges});
                log::info!("import_all[perf] enrich_all: {} embs, {} edges in {:?}", embs, edges, t_enrich.elapsed());
            }
            Err(e) => all_errors.push(format!("enrich: {}", e)),
        }
    }

    log::info!("import_all[perf] total: {} nodes in {:?}", grand_total, t_start.elapsed());
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

}

#[cfg(test)]
mod integration {
    use super::*;
    use std::path::{Path, PathBuf};

    /// 每个测试独立的临时夹具:唯一目录 + 自动清理。
    /// 注意:engine 必须是测试里的局部变量(不要放进 Fixture),
    /// 这样它会先于 Fixture::drop 释放,Windows 上才能删掉 sqlite 文件。
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "gm_fixture_{}_{}_{}",
                tag,
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&root).expect("create fixture root");
            Fixture { root }
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.root.join(rel)
        }

        fn write(&self, rel: &str, body: &str) {
            let p = self.path(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).expect("create fixture subdir");
            }
            std::fs::write(&p, body).expect("write fixture file");
        }

        fn engine(&self) -> crate::engine::GraphEngine {
            let db = self.path("graph.db");
            crate::engine::GraphEngine::new(db.to_str().expect("db path utf8")).expect("open engine")
        }

        fn root(&self) -> &Path {
            self.root.as_path()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // engine 此时应当已被释放(局部变量逆序析构 + 显式 drop)
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn errors_of(result: &serde_json::Value) -> Vec<String> {
        result["errors"]
            .as_array()
            .expect("errors array")
            .iter()
            .map(|v| v.as_str().unwrap_or_default().to_string())
            .collect()
    }

    #[test]
    fn parse_hermes_reads_memory_md_from_fixture_disk() {
        let fx = Fixture::new("hermes_disk");
        fx.write(
            "memories/MEMORY.md",
            "Disk entry one: read back from the fixture directory\n\u{a7}\nDisk entry two: also read back from disk",
        );

        let content = std::fs::read_to_string(fx.path("memories/MEMORY.md")).expect("read MEMORY.md");
        let entries = parse_hermes_md(&content);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, "Disk entry one");
        assert_eq!(entries[1].0, "Disk entry two");
        assert!(entries[1].1.contains("Disk entry two"));

        let mut eng = fx.engine();
        let r = import_hermes_from(&mut eng, fx.root());
        drop(eng);

        assert_eq!(r.total, 2, "fixture MEMORY.md 应产出 2 个节点");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn hermes_fixture_imports_memory_user_and_skips_self_skills() {
        let fx = Fixture::new("hermes");
        fx.write(
            "memories/MEMORY.md",
            "Memory one: deterministic hermes body alpha\n\u{a7}\nMemory two: deterministic hermes body beta",
        );
        fx.write(
            "memories/USER.md",
            "User preference: prefers deterministic fixtures over machine state",
        );
        fx.write(
            "skills/good-skill/SKILL.md",
            "# Good Skill\n\nA deterministic skill body long enough to be imported.",
        );
        // 自身 skill 必须跳过(两种命名都要覆盖)
        fx.write(
            "skills/graph-memory/SKILL.md",
            "# Graph Memory\n\nSelf skill that must be skipped by the importer.",
        );
        fx.write(
            "skills/graph_memory/SKILL.md",
            "# Graph Memory Underscore\n\nAlso skipped because of the name match.",
        );

        let mut eng = fx.engine();
        let r = import_hermes_from(&mut eng, fx.root());
        drop(eng);

        // 2 (MEMORY.md) + 1 (USER.md) + 1 (good-skill),自身 skill 不计入
        assert_eq!(r.total, 4, "hermes 夹具应产出 4 个节点");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn claude_fixture_imports_valid_frontmatter_and_skips_noise() {
        let fx = Fixture::new("claude");
        fx.write(
            "projects/demo/memory/one.md",
            "---\nname: claude-node-one\ntype: knowledge\n---\nDeterministic claude body that is comfortably long enough.",
        );
        fx.write(
            "projects/demo/memory/two.md",
            "---\nname: claude-node-two\ntype: reference\n---\nAnother deterministic claude body that is long enough to import.",
        );
        // 正文过短 → 噪声,跳过
        fx.write(
            "projects/demo/memory/noise.md",
            "---\nname: claude-noise\ntype: knowledge\n---\nshort",
        );
        // 无 frontmatter → parse_claude_md 直接返回 None,既不入库也不报错
        fx.write(
            "projects/demo/memory/no-frontmatter.md",
            "Just plain markdown without frontmatter, long enough though.",
        );

        let mut eng = fx.engine();
        let r = import_claude_from(&mut eng, fx.root());
        drop(eng);

        assert_eq!(r.total, 2, "claude 夹具应产出 2 个节点");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn codex_fixture_imports_long_text_and_skips_noise_and_invalid_json() {
        let long_one =
            "Deterministic codex entry number one with enough characters to pass the fifty char filter";
        let long_two =
            "Deterministic codex entry number two with enough characters to pass the fifty char filter";
        let lines = [
            format!("{{\"text\":\"{}\"}}", long_one),
            "{}".to_string(),
            "{\"text\":\"ok\"}".to_string(),
            "not-json-at-all".to_string(),
            "{\"text\":\"\"}".to_string(),
            format!("{{\"text\":\"{}\"}}", long_two),
        ];

        let fx = Fixture::new("codex");
        fx.write("history.jsonl", &lines.join("\n"));

        let mut eng = fx.engine();
        let r = import_codex_from(&mut eng, fx.root());
        drop(eng);

        assert_eq!(r.total, 2, "codex 夹具应产出 2 个节点");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn import_all_from_fixture_totals_and_sources() {
        let fx = Fixture::new("all");
        fx.write(
            "memories/MEMORY.md",
            "Single hermes entry: deterministic body long enough to import",
        );
        fx.write(
            "projects/demo/memory/only.md",
            "---\nname: claude-only\ntype: knowledge\n---\nDeterministic claude body that is long enough to import.",
        );
        fx.write(
            "history.jsonl",
            "{\"text\":\"Deterministic codex entry with enough characters to pass the fifty char filter\"}",
        );

        let mut eng = fx.engine();
        let result = import_all_from(&mut eng, false, fx.root(), fx.root(), fx.root());
        drop(eng);
        let reopened = fx.engine();
        let persisted = reopened.stats();
        assert_eq!(persisted["node_count"].as_u64(), Some(3));
        for source in ["hermes", "claude", "codex"] {
            assert_eq!(persisted["source_counts"][source].as_u64(), Some(1));
        }
        drop(reopened);

        assert_eq!(result["total_nodes"].as_u64(), Some(3));
        assert_eq!(result["sources"]["hermes"]["nodes"].as_u64(), Some(1));
        assert_eq!(result["sources"]["claude"]["nodes"].as_u64(), Some(1));
        assert_eq!(result["sources"]["codex"]["nodes"].as_u64(), Some(1));
        let errors = errors_of(&result);
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn import_all_from_missing_sources_reports_errors_and_zero_totals() {
        let fx = Fixture::new("missing");

        let mut eng = fx.engine();
        let result = import_all_from(&mut eng, false, fx.root(), fx.root(), fx.root());
        drop(eng);

        assert_eq!(result["total_nodes"].as_u64(), Some(0));
        assert_eq!(result["sources"]["hermes"]["nodes"].as_u64(), Some(0));
        assert_eq!(result["sources"]["claude"]["nodes"].as_u64(), Some(0));
        assert_eq!(result["sources"]["codex"]["nodes"].as_u64(), Some(0));

        let errors = errors_of(&result);
        assert_eq!(errors.len(), 3, "errors: {:?}", errors);
        assert!(
            errors.iter().any(|e| e.starts_with("hermes:") && e.contains("memories dir not found")),
            "errors: {:?}",
            errors
        );
        assert!(
            errors.iter().any(|e| e.starts_with("claude:") && e.contains("not found")),
            "errors: {:?}",
            errors
        );
        assert!(
            errors.iter().any(|e| e.starts_with("codex:") && e.contains("not found")),
            "errors: {:?}",
            errors
        );
    }
}
