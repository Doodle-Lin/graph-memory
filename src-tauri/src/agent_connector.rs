//! Agent 接入器:检测本机安装的 AI agent,读写它们的 MCP 配置。
//!
//! 支持: Claude Code (~/.claude.json), Codex (~/.codex/config.json),
//! Hermes (hermes config). 不修改用户私人配置——只写 mcpServers 节。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct AgentInfo {
    pub id: String,       // "claude" | "codex" | "hermes"
    pub name: String,     // 显示名
    pub installed: bool,  // 是否检测到
    pub config_path: Option<String>, // 配置文件路径(检测到的)
    pub connected: bool,  // 是否已写入 graph-memory MCP 配置
    pub exe_path: Option<String>, // mcp_server.exe 路径(已写入配置的)
}

/// 检测本机装了哪些 agent,以及它们的 MCP 配置状态
pub fn detect_agents() -> Vec<AgentInfo> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();

    let mut agents = Vec::new();

    // Claude Code: ~/.claude.json (mcpServers 在顶层)
    let claude_path = PathBuf::from(&home).join(".claude.json");
    let claude_installed = claude_path.exists();
    let claude_connected = if claude_installed {
        is_connected(&claude_path, "graph-memory")
    } else {
        false
    };
    agents.push(AgentInfo {
        id: "claude".into(),
        name: "Claude Code".into(),
        installed: claude_installed,
        config_path: if claude_installed { Some(claude_path.to_string_lossy().into()) } else { None },
        connected: claude_connected,
        exe_path: None,
    });

    // Codex: ~/.codex/config.json
    let codex_path = PathBuf::from(&home).join(".codex/config.json");
    let codex_installed = codex_path.exists();
    let codex_connected = if codex_installed {
        is_connected(&codex_path, "graph-memory")
    } else {
        false
    };
    agents.push(AgentInfo {
        id: "codex".into(),
        name: "OpenAI Codex".into(),
        installed: codex_installed,
        config_path: if codex_installed { Some(codex_path.to_string_lossy().into()) } else { None },
        connected: codex_connected,
        exe_path: None,
    });

    // Hermes: 检测 hermes CLI 是否在 PATH 里
    let hermes_installed = which_hermes().is_some();
    agents.push(AgentInfo {
        id: "hermes".into(),
        name: "Hermes Agent".into(),
        installed: hermes_installed,
        config_path: None, // Hermes MCP 配置位置待定
        connected: false,
        exe_path: None,
    });

    agents
}

/// 把 graph-memory MCP server 写入指定 agent 的配置
pub fn connect_agent(agent_id: &str, exe_path: &str) -> Result<(), String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "no home dir")?;

    let config_path = match agent_id {
        "claude" => PathBuf::from(&home).join(".claude.json"),
        "codex" => PathBuf::from(&home).join(".codex/config.json"),
        _ => return Err(format!("unknown agent: {}", agent_id)),
    };

    if !config_path.exists() {
        return Err(format!("config not found: {}", config_path.display()));
    }

    // 读现有配置
    let raw = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("read failed: {}", e))?;
    let mut config: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse failed: {}", e))?;

    // 确保 mcpServers 存在
    if config.get("mcpServers").is_none() {
        config["mcpServers"] = serde_json::json!({});
    }
    let mcp = config
        .get_mut("mcpServers")
        .and_then(|v| v.as_object_mut())
        .ok_or("mcpServers is not an object")?;

    // 写入 graph-memory 配置
    mcp.insert(
        "graph-memory".into(),
        serde_json::json!({
            "command": exe_path,
            "env": {
                "GM_API_URL": "http://127.0.0.1:9121"
            }
        }),
    );

    // 写回
    let out = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("serialize failed: {}", e))?;
    std::fs::write(&config_path, out)
        .map_err(|e| format!("write failed: {}", e))?;

    log::info!("Connected agent '{}' to graph-memory MCP (config: {})", agent_id, config_path.display());
    Ok(())
}

/// 断开:从 agent 配置中移除 graph-memory 条目
pub fn disconnect_agent(agent_id: &str) -> Result<(), String> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map_err(|_| "no home dir")?;

    let config_path = match agent_id {
        "claude" => PathBuf::from(&home).join(".claude.json"),
        "codex" => PathBuf::from(&home).join(".codex/config.json"),
        _ => return Err(format!("unknown agent: {}", agent_id)),
    };

    if !config_path.exists() {
        return Err(format!("config not found: {}", config_path.display()));
    }

    let raw = std::fs::read_to_string(&config_path)
        .map_err(|e| format!("read failed: {}", e))?;
    let mut config: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("parse failed: {}", e))?;

    if let Some(mcp) = config.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        mcp.remove("graph-memory");
    }

    let out = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("serialize failed: {}", e))?;
    std::fs::write(&config_path, out)
        .map_err(|e| format!("write failed: {}", e))?;

    log::info!("Disconnected agent '{}' from graph-memory", agent_id);
    Ok(())
}

/// 检查配置文件中是否已有 graph-memory 条目
fn is_connected(config_path: &std::path::Path, key: &str) -> bool {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let config: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(_) => return false,
    };
    config
        .get("mcpServers")
        .and_then(|v| v.get(key))
        .is_some()
}

/// 检测 hermes CLI 是否在 PATH 中
fn which_hermes() -> Option<String> {
    let out = std::process::Command::new("where")
        .arg("hermes")
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        let line = s.lines().next()?;
        if !line.trim().is_empty() {
            return Some(line.trim().into());
        }
    }
    None
}
