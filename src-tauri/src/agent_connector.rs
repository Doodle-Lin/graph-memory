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

    // Hermes: 检测 hermes CLI 是否在 PATH 里,配置走 YAML(非 ~/.hermes 而是 %LOCALAPPDATA%\hermes)
    let hermes_installed = which_hermes().is_some();
    let hermes_cfg = hermes_config_path();
    let hermes_connected = hermes_cfg
        .as_ref()
        .map(|p| is_hermes_connected(p))
        .unwrap_or(false);
    agents.push(AgentInfo {
        id: "hermes".into(),
        name: "Hermes Agent".into(),
        installed: hermes_installed,
        config_path: hermes_cfg.as_ref().map(|p| p.to_string_lossy().into()),
        connected: hermes_connected,
        exe_path: None,
    });

    agents
}

/// 把 graph-memory MCP server 写入指定 agent 的配置
pub fn connect_agent(agent_id: &str, exe_path: &str) -> Result<(), String> {
    // Hermes 用 YAML 配置,走独立路径
    if agent_id == "hermes" {
        return connect_hermes(exe_path);
    }

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
    // Hermes 用 YAML 配置,走独立路径
    if agent_id == "hermes" {
        return disconnect_hermes();
    }

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

// ── Hermes(YAML 配置)专用逻辑 ──────────────────────────────────

/// 解析 Hermes 配置文件路径。
/// 顺序:HERMES_HOME env → 平台默认(Win=%LOCALAPPDATA%\hermes, POSIX=~/.hermes)→ hermes config path 子进程。
fn hermes_config_path() -> Option<PathBuf> {
    // 1. HERMES_HOME 环境变量覆盖
    if let Ok(hh) = std::env::var("HERMES_HOME") {
        if !hh.trim().is_empty() {
            let p = PathBuf::from(&hh).join("config.yaml");
            if p.exists() {
                return Some(p);
            }
        }
    }
    // 2. 平台默认
    let default = if cfg!(target_os = "windows") {
        std::env::var("LOCALAPPDATA").ok().map(|d| PathBuf::from(d).join("hermes").join("config.yaml"))
    } else {
        std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".hermes").join("config.yaml"))
    };
    if let Some(p) = default {
        if p.exists() {
            return Some(p);
        }
    }
    // 3. 回退:问 hermes 自己
    if let Ok(out) = std::process::Command::new("hermes").arg("config").arg("path").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                let p = PathBuf::from(&s);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// 检查 Hermes YAML 配置里是否已有 mcp_servers.graph-memory
fn is_hermes_connected(config_path: &std::path::Path) -> bool {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(r) => r,
        Err(_) => return false,
    };
    let config: serde_json::Value = match serde_yaml::from_str(&raw) {
        Ok(c) => c,
        Err(_) => return false,
    };
    config.get("mcp_servers")
        .and_then(|v| v.get("graph-memory"))
        .is_some()
}

/// 读 YAML 配置为 serde_json::Value(空文件视为空 map)
fn load_hermes_yaml(config_path: &std::path::Path) -> Result<serde_json::Value, String> {
    let raw = std::fs::read_to_string(config_path)
        .map_err(|e| format!("read failed: {}", e))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_yaml::from_str::<serde_json::Value>(&raw)
        .map_err(|e| format!("yaml parse failed: {}", e))
}

/// 写前备份 + 序列化写回
fn write_hermes_yaml(config_path: &std::path::Path, config: &serde_json::Value) -> Result<(), String> {
    // 备份原文件(覆盖已有 .bak)
    let bak = config_path.with_extension("yaml.bak");
    if config_path.exists() {
        std::fs::copy(config_path, &bak).ok();
    }
    let out = serde_yaml::to_string(config)
        .map_err(|e| format!("yaml serialize failed: {}", e))?;
    std::fs::write(config_path, out)
        .map_err(|e| format!("write failed: {}", e))?;
    Ok(())
}

/// 接入 Hermes:在 YAML 的 mcp_servers.graph-memory 写入 stdio 条目
fn connect_hermes(exe_path: &str) -> Result<(), String> {
    let config_path = hermes_config_path()
        .ok_or_else(|| "hermes config.yaml not found (设置 HERMES_HOME 或安装 hermes)".to_string())?;
    let mut config = load_hermes_yaml(&config_path)?;
    // 确保 mcp_servers 存在且是 map
    if config.get("mcp_servers").is_none() {
        config["mcp_servers"] = serde_json::json!({});
    }
    let mcp = config
        .get_mut("mcp_servers")
        .and_then(|v| v.as_object_mut())
        .ok_or("mcp_servers is not an object")?;
    mcp.insert(
        "graph-memory".into(),
        serde_json::json!({
            "command": exe_path,
            "args": [],
            "env": {
                "GM_API_URL": "http://127.0.0.1:9121"
            },
            "enabled": true
        }),
    );
    write_hermes_yaml(&config_path, &config)?;
    log::info!("Connected hermes to graph-memory MCP (config: {})", config_path.display());
    Ok(())
}

/// 断开 Hermes:从 YAML 移除 mcp_servers.graph-memory
fn disconnect_hermes() -> Result<(), String> {
    let config_path = hermes_config_path()
        .ok_or_else(|| "hermes config.yaml not found".to_string())?;
    let mut config = load_hermes_yaml(&config_path)?;
    if let Some(mcp) = config.get_mut("mcp_servers").and_then(|v| v.as_object_mut()) {
        mcp.remove("graph-memory");
    }
    write_hermes_yaml(&config_path, &config)?;
    log::info!("Disconnected hermes from graph-memory");
    Ok(())
}
