use crate::{config::ConfigManager, server, ssh::SshManager, DbPool};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
    time::Duration,
};
use tauri::{AppHandle, Manager};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream as TokioTcpStream},
    sync::Mutex as AsyncMutex,
};

const DISCOVERY_FILE: &str = "mcp-runtime.json";
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct BrokerDiscovery {
    version: u8,
    host: String,
    port: u16,
    token: String,
    pid: u32,
}

#[derive(Debug, Deserialize)]
struct BrokerRequest {
    token: String,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct BrokerResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct ServerSummary {
    profile_id: String,
    name: String,
    host: String,
    port: u16,
    username: String,
    auth_type: String,
    connected: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpRegistrationStatus {
    pub codex_found: bool,
    pub codex_path: String,
    pub registered: bool,
    pub current: bool,
    pub registered_path: String,
    pub version: String,
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpServerPermission {
    pub profile_id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub permission_level: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct McpAuditEntry {
    pub id: i64,
    pub created_at: String,
    pub profile_id: String,
    pub method: String,
    pub target: String,
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize)]
struct CodexMcpRegistration {
    #[serde(default = "default_true")]
    enabled: bool,
    transport: CodexMcpTransport,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize)]
struct CodexMcpTransport {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PermissionLevel {
    None,
    Read,
    Manage,
    System,
}

impl PermissionLevel {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "read" => Some(Self::Read),
            "manage" => Some(Self::Manage),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Read => "read",
            Self::Manage => "manage",
            Self::System => "system",
        }
    }
}

fn discovery_path() -> PathBuf {
    crate::db::db_dir().join(DISCOVERY_FILE)
}

pub fn start_broker(app: AppHandle) {
    let std_listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => listener,
        Err(error) => {
            log::error!("Failed to start LeePanel AI Broker: {error}");
            return;
        }
    };
    if let Err(error) = std_listener.set_nonblocking(true) {
        log::error!("Failed to configure LeePanel AI Broker: {error}");
        return;
    }
    let port = match std_listener.local_addr() {
        Ok(address) => address.port(),
        Err(error) => {
            log::error!("Failed to read LeePanel AI Broker address: {error}");
            return;
        }
    };
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let discovery = BrokerDiscovery {
        version: 1,
        host: "127.0.0.1".to_string(),
        port,
        token: token.clone(),
        pid: std::process::id(),
    };
    let _ = std::fs::remove_file(discovery_path());

    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::from_std(std_listener) {
            Ok(listener) => listener,
            Err(error) => {
                log::error!("Failed to initialize LeePanel AI Broker: {error}");
                return;
            }
        };
        if let Err(error) = write_discovery(&discovery) {
            log::error!("Failed to publish LeePanel AI Broker: {error}");
            return;
        }
        loop {
            match listener.accept().await {
                Ok((stream, peer)) if peer.ip().is_loopback() => {
                    let app = app.clone();
                    let token = token.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_broker_connection(stream, app, &token).await {
                            log::warn!("LeePanel AI Broker request failed: {error}");
                        }
                    });
                }
                Ok(_) => {}
                Err(error) => {
                    log::error!("LeePanel AI Broker accept failed: {error}");
                    break;
                }
            }
        }
    });
}

fn write_discovery(discovery: &BrokerDiscovery) -> Result<(), String> {
    let path = discovery_path();
    let content = serde_json::to_vec(discovery)
        .map_err(|e| format!("Failed to serialize LeePanel AI Broker discovery: {e}"))?;
    std::fs::write(&path, content)
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to protect {}: {e}", path.display()))?;
    }
    Ok(())
}

async fn handle_broker_connection(
    stream: TokioTcpStream,
    app: AppHandle,
    expected_token: &str,
) -> Result<(), String> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader).take(MAX_REQUEST_BYTES);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .map_err(|e| format!("Failed to read request: {e}"))?;
    let request: BrokerRequest =
        serde_json::from_str(&line).map_err(|e| format!("Invalid broker request: {e}"))?;
    let response = if request.token != expected_token {
        BrokerResponse {
            ok: false,
            result: None,
            error: Some("Unauthorized LeePanel AI Broker request".to_string()),
        }
    } else {
        let method = request.method.clone();
        let params = request.params;
        let profile_id = params
            .get("profile_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let target = audit_target(&params);
        match dispatch_broker_request(&app, &method, params).await {
            Ok(result) => BrokerResponse {
                ok: {
                    record_audit(&app, &profile_id, &method, &target, true, "success");
                    true
                },
                result: Some(result),
                error: None,
            },
            Err(error) => {
                record_audit(&app, &profile_id, &method, &target, false, &error);
                BrokerResponse {
                    ok: false,
                    result: None,
                    error: Some(error),
                }
            }
        }
    };
    let mut output = serde_json::to_vec(&response)
        .map_err(|e| format!("Failed to serialize broker response: {e}"))?;
    output.push(b'\n');
    writer
        .write_all(&output)
        .await
        .map_err(|e| format!("Failed to write broker response: {e}"))
}

fn audit_target(params: &Value) -> String {
    for key in ["container", "domain", "path", "profile_id"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            return value.chars().take(200).collect();
        }
    }
    if params.get("command").is_some() {
        return "command".to_string();
    }
    String::new()
}

fn record_audit(
    app: &AppHandle,
    profile_id: &str,
    method: &str,
    target: &str,
    success: bool,
    message: &str,
) {
    let db = app.state::<DbPool>();
    if let Ok(conn) = db.lock() {
        let safe_message: String = message.chars().take(500).collect();
        let _ = conn.execute(
            "INSERT INTO mcp_audit (profile_id, method, target, success, message) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![profile_id, method, target, if success { 1 } else { 0 }, safe_message],
        );
    };
}

fn mcp_enabled(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT value FROM settings WHERE key = 'mcp_enabled'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(false)
}

fn permission_level(conn: &rusqlite::Connection, profile_id: &str) -> PermissionLevel {
    conn.query_row(
        "SELECT permission_level FROM mcp_permissions WHERE profile_id = ?1",
        params![profile_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .and_then(|value| PermissionLevel::parse(&value))
    .unwrap_or(PermissionLevel::None)
}

fn has_access(conn: &rusqlite::Connection, profile_id: &str, required: PermissionLevel) -> bool {
    permission_level(conn, profile_id) >= required
}

async fn dispatch_broker_request(
    app: &AppHandle,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    match method {
        "list_servers" => {
            let db = app.state::<DbPool>();
            let profiles = {
                let conn = db.lock().map_err(|e| e.to_string())?;
                if !mcp_enabled(&conn) {
                    return Err(
                        "LeePanel MCP is disabled. Enable it in MCP / AI Integration settings."
                            .to_string(),
                    );
                }
                ConfigManager::list(&conn)
                    .into_iter()
                    .filter(|profile| has_access(&conn, &profile.id, PermissionLevel::Read))
                    .collect::<Vec<_>>()
            };
            let ssh_state = app.state::<Arc<AsyncMutex<SshManager>>>();
            let manager = ssh_state.lock().await;
            let servers: Vec<ServerSummary> = profiles
                .into_iter()
                .map(|profile| ServerSummary {
                    connected: manager
                        .find_session_id(&profile.host, profile.port, &profile.username)
                        .is_some(),
                    profile_id: profile.id,
                    name: profile.name,
                    host: profile.host,
                    port: profile.port,
                    username: profile.username,
                    auth_type: profile.auth_type,
                })
                .collect();
            serde_json::to_value(servers).map_err(|e| e.to_string())
        }
        "get_server_status" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let system = server::get_system_info(&session, &cache, &session_id).await?;
            let services = server::get_service_statuses(&session, &cache, &session_id).await?;
            Ok(json!({ "system": system, "services": services }))
        }
        "get_services" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let services = server::get_service_statuses(&session, &cache, &session_id).await?;
            serde_json::to_value(services).map_err(|e| e.to_string())
        }
        "get_nginx_status" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let service = server::get_service_info(&session, &cache, &session_id, "nginx").await?;
            let (config_valid, config_test_output) =
                server::test_nginx_config(&session, &cache, &session_id).await?;
            let vhosts = server::list_nginx_vhosts(&session, &cache, &session_id).await?;
            Ok(json!({
                "service": service,
                "config_valid": config_valid,
                "config_test_output": config_test_output,
                "vhosts": vhosts
            }))
        }
        "get_container_runtime" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let status = server::check_docker(&session, &cache, &session_id).await?;
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "list_containers" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let containers = server::docker_container_list(&session, &cache, &session_id).await?;
            serde_json::to_value(containers).map_err(|e| e.to_string())
        }
        "get_container_logs" => {
            let profile_id = required_string(&params, "profile_id")?;
            let requested = required_string(&params, "container")?;
            let lines = optional_usize(&params, "lines", 200, 1000)?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let containers = server::docker_container_list(&session, &cache, &session_id).await?;
            let container = containers
                .iter()
                .find(|container| container.id == requested || container.name == requested)
                .ok_or_else(|| format!("Container not found in LeePanel: {requested}"))?;
            let logs =
                server::docker_container_logs(&session, &cache, &session_id, &container.id, lines)
                    .await?;
            Ok(json!({
                "container": { "id": container.id, "name": container.name },
                "lines": lines,
                "logs": logs
            }))
        }
        "read_file" => {
            let profile_id = required_string(&params, "profile_id")?;
            let path = required_remote_path(&params)?;
            let max_bytes = optional_usize(&params, "max_bytes", 65_536, 131_072)?;
            let (session, _, _) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let content = crate::ssh::session_read_file(&session, path).await?;
            let (content, truncated) = truncate_utf8(&content, max_bytes);
            Ok(json!({ "path": path, "content": content, "truncated": truncated }))
        }
        "write_file" => {
            let profile_id = required_string(&params, "profile_id")?;
            let path = required_remote_path(&params)?;
            let content = params
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| "Missing string parameter: content".to_string())?;
            if content.len() > 131_072 {
                return Err("File content exceeds the 131072 byte MCP limit".to_string());
            }
            let (session, _, _) =
                active_session(app, profile_id, PermissionLevel::Manage).await?;
            crate::ssh::session_write_file(&session, path, content).await?;
            Ok(json!({ "path": path, "bytes_written": content.len() }))
        }
        "run_command" => {
            let profile_id = required_string(&params, "profile_id")?;
            let command = required_command(&params)?;
            reject_privilege_escalation(command)?;
            let timeout = optional_usize(&params, "timeout_seconds", 60, 300)? as u64;
            let (session, _, _) =
                active_session(app, profile_id, PermissionLevel::Manage).await?;
            let (stdout, stderr, exit_code) =
                crate::ssh::session_exec_with_output(&session, command, timeout).await?;
            Ok(command_result(stdout, stderr, exit_code))
        }
        "run_privileged_command" => {
            let profile_id = required_string(&params, "profile_id")?;
            let command = required_command(&params)?;
            let timeout = optional_usize(&params, "timeout_seconds", 60, 300)? as u64;
            let (session, _, _) =
                active_session(app, profile_id, PermissionLevel::System).await?;
            let quoted = shell_single_quote(command);
            let elevated = format!(
                "if [ \"$(id -u)\" -eq 0 ]; then exec sh -lc {quoted}; else exec sudo -n -- sh -lc {quoted}; fi"
            );
            let (stdout, stderr, exit_code) =
                crate::ssh::session_exec_with_output(&session, &elevated, timeout).await?;
            Ok(command_result(stdout, stderr, exit_code))
        }
        "run_container_action" => {
            let profile_id = required_string(&params, "profile_id")?;
            let requested = required_string(&params, "container")?;
            let action = required_container_action(&params)?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Manage).await?;
            let containers = server::docker_container_list(&session, &cache, &session_id).await?;
            let container = containers
                .iter()
                .find(|container| container.id == requested || container.name == requested)
                .ok_or_else(|| format!("Container not found in LeePanel: {requested}"))?;
            let container_id = container.id.clone();
            let container_name = container.name.clone();
            let before = json!({ "state": container.state, "status": container.status });
            let message = server::docker_container_action(
                &session,
                &cache,
                &session_id,
                &container_id,
                action,
            )
            .await?;
            let after = server::docker_container_list(&session, &cache, &session_id)
                .await?
                .into_iter()
                .find(|container| container.id == container_id)
                .map(|container| json!({ "state": container.state, "status": container.status }));
            Ok(json!({
                "container": { "id": container_id, "name": container_name },
                "action": action,
                "message": message,
                "before": before,
                "after": after
            }))
        }
        "list_images" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let images = server::docker_image_list(&session, &cache, &session_id).await?;
            serde_json::to_value(images).map_err(|e| e.to_string())
        }
        "list_sites" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Read).await?;
            let sites = server::list_sites(&session, &cache, &session_id).await?;
            serde_json::to_value(sites).map_err(|e| e.to_string())
        }
        "set_site_enabled" => {
            let profile_id = required_string(&params, "profile_id")?;
            let domain = required_string(&params, "domain")?;
            let enabled = params
                .get("enabled")
                .and_then(Value::as_bool)
                .ok_or_else(|| "Missing boolean parameter: enabled".to_string())?;
            let (session, cache, session_id) =
                active_session(app, profile_id, PermissionLevel::Manage).await?;
            let sites = server::list_sites(&session, &cache, &session_id).await?;
            let site = sites
                .iter()
                .find(|site| site.domain == domain)
                .ok_or_else(|| format!("Site not found in LeePanel: {domain}"))?;
            if site.enabled == enabled {
                return Ok(json!({
                    "message": format!("Site {domain} is already {}", if enabled { "enabled" } else { "disabled" }),
                    "domain": domain,
                    "enabled": enabled
                }));
            }
            let message = server::toggle_site(
                &session,
                &cache,
                &session_id,
                &site.config_path,
                &site.domain,
                enabled,
            )
            .await?;
            Ok(json!({ "message": message, "domain": domain, "enabled": enabled }))
        }
        _ => Err(format!("Unsupported LeePanel broker method: {method}")),
    }
}

fn required_string<'a>(params: &'a Value, name: &str) -> Result<&'a str, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("Missing string parameter: {name}"))
}

fn optional_usize(params: &Value, name: &str, default: usize, max: usize) -> Result<usize, String> {
    let Some(value) = params.get(name) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| (1..=max).contains(value))
        .ok_or_else(|| format!("Parameter {name} must be an integer between 1 and {max}"))?;
    Ok(value)
}

fn required_remote_path<'a>(params: &'a Value) -> Result<&'a str, String> {
    let path = required_string(params, "path")?;
    if !path.starts_with('/') || path.contains('\0') || path.len() > 4096 {
        return Err("Parameter path must be an absolute remote path up to 4096 bytes".to_string());
    }
    Ok(path)
}

fn required_command<'a>(params: &'a Value) -> Result<&'a str, String> {
    let command = required_string(params, "command")?;
    if command.contains('\0') || command.len() > 65_536 {
        return Err("Parameter command exceeds the 65536 byte MCP limit".to_string());
    }
    Ok(command)
}

fn reject_privilege_escalation(command: &str) -> Result<(), String> {
    let words = command
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_' || character == '-'))
        .map(str::to_ascii_lowercase);
    if words.into_iter().any(|word| matches!(word.as_str(), "sudo" | "su" | "doas" | "pkexec")) {
        return Err(
            "Privilege escalation is not allowed with management access. Use the privileged command tool with system access."
                .to_string(),
        );
    }
    Ok(())
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

fn command_result(stdout: String, stderr: String, exit_code: i32) -> Value {
    let (stdout, stdout_truncated) = truncate_utf8(&stdout, 65_536);
    let (stderr, stderr_truncated) = truncate_utf8(&stderr, 65_536);
    json!({
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": exit_code,
        "truncated": stdout_truncated || stderr_truncated
    })
}

fn required_container_action(params: &Value) -> Result<&str, String> {
    let action = required_string(params, "action")?;
    match action {
        "start" | "stop" | "restart" => Ok(action),
        _ => Err("Parameter action must be one of: start, stop, restart".to_string()),
    }
}

async fn active_session(
    app: &AppHandle,
    profile_id: &str,
    required: PermissionLevel,
) -> Result<(crate::ssh::SshSession, Arc<crate::ssh::SshCache>, String), String> {
    let db = app.state::<DbPool>();
    let profile = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        if !mcp_enabled(&conn) {
            return Err(
                "LeePanel MCP is disabled. Enable it in MCP / AI Integration settings.".to_string(),
            );
        }
        let profile = ConfigManager::list(&conn)
            .into_iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| format!("LeePanel server profile not found: {profile_id}"))?;
        let effective_required = if profile.username == "root" && required > PermissionLevel::Read {
            PermissionLevel::System
        } else {
            required
        };
        if !has_access(&conn, profile_id, effective_required) {
            return Err(format!(
                "MCP {} access is not authorized for LeePanel server profile: {profile_id}",
                effective_required.as_str()
            ));
        }
        profile
    };
    let ssh_state = app.state::<Arc<AsyncMutex<SshManager>>>();
    let manager = ssh_state.lock().await;
    let session_id = manager
        .find_session_id(&profile.host, profile.port, &profile.username)
        .ok_or_else(|| format!("Server '{}' is not connected in LeePanel", profile.name))?;
    let session = manager.get_session(&session_id)?;
    let cache = manager.cache.clone();
    Ok((session, cache, session_id))
}

pub fn run_stdio() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let _ = write_json_line(
                    &mut stdout,
                    &json_rpc_error(Value::Null, -32700, &error.to_string()),
                );
                continue;
            }
        };
        if request.get("id").is_none() {
            continue;
        }
        let response = handle_mcp_request(&request);
        if write_json_line(&mut stdout, &response).is_err() {
            break;
        }
    }
}

fn handle_mcp_request(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": request.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "leepanel", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tool_definitions() }
        }),
        "tools/call" => {
            let name = request
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match tool_to_broker_method(name) {
                Some(method) => match call_broker(method, arguments) {
                    Ok(result) => {
                        let text = serde_json::to_string_pretty(&result)
                            .unwrap_or_else(|_| result.to_string());
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": { "content": [{ "type": "text", "text": text }], "structuredContent": result }
                        })
                    }
                    Err(error) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "content": [{ "type": "text", "text": error }], "isError": true }
                    }),
                },
                None => json_rpc_error(id, -32602, &format!("Unknown LeePanel tool: {name}")),
            }
        }
        _ => json_rpc_error(id, -32601, &format!("Method not found: {method}")),
    }
}

fn tool_to_broker_method(tool: &str) -> Option<&'static str> {
    match tool {
        "leepanel_list_servers" => Some("list_servers"),
        "leepanel_get_server_status" => Some("get_server_status"),
        "leepanel_get_services" => Some("get_services"),
        "leepanel_get_nginx_status" => Some("get_nginx_status"),
        "leepanel_get_container_runtime" => Some("get_container_runtime"),
        "leepanel_list_containers" => Some("list_containers"),
        "leepanel_get_container_logs" => Some("get_container_logs"),
        "leepanel_read_file" => Some("read_file"),
        "leepanel_write_file" => Some("write_file"),
        "leepanel_run_command" => Some("run_command"),
        "leepanel_run_privileged_command" => Some("run_privileged_command"),
        "leepanel_run_container_action" => Some("run_container_action"),
        "leepanel_list_images" => Some("list_images"),
        "leepanel_list_sites" => Some("list_sites"),
        "leepanel_set_site_enabled" => Some("set_site_enabled"),
        _ => None,
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "leepanel_list_servers",
            "description": "List server profiles saved in LeePanel and show whether each one is currently connected. Passwords and private keys are never returned.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_get_server_status",
            "description": "Read operating system, CPU, memory, disks, uptime, load, and core service status from a server currently connected in LeePanel.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_get_services",
            "description": "Read the status of core services detected by LeePanel on a currently connected server.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_get_nginx_status",
            "description": "Read Nginx service details, run a read-only nginx configuration test, and list detected virtual host configuration files.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_get_container_runtime",
            "description": "Read the Docker or Podman runtime selected by LeePanel, including installation, version, Compose, and running status.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_list_containers",
            "description": "List containers from the Docker or Podman runtime selected by LeePanel on a currently connected server.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_get_container_logs",
            "description": "Read a bounded number of log lines from a container currently listed by LeePanel. The container must be identified by its exact ID or name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." },
                    "container": { "type": "string", "description": "Exact container ID or name returned by leepanel_list_containers." },
                    "lines": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200, "description": "Number of recent log lines to return." }
                },
                "required": ["profile_id", "container"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_read_file",
            "description": "Read a UTF-8 text file from a currently connected LeePanel server. Requires read access. Output is bounded and reports whether it was truncated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." },
                    "path": { "type": "string", "description": "Absolute remote file path." },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 131072, "default": 65536 }
                },
                "required": ["profile_id", "path"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_write_file",
            "description": "Create or replace a UTF-8 text file on a currently connected LeePanel server. Requires management access, or system access when the SSH profile uses root.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." },
                    "path": { "type": "string", "description": "Absolute remote file path." },
                    "content": { "type": "string", "maxLength": 131072, "description": "Complete UTF-8 file content. Existing content is replaced." }
                },
                "required": ["profile_id", "path", "content"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_run_command",
            "description": "Run a command or script as the connected SSH user. Requires management access; root profiles require system access. Direct sudo, su, doas, and pkexec usage is rejected; use the privileged command tool instead.",
            "inputSchema": command_schema(),
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_run_privileged_command",
            "description": "Run a command or script as root through passwordless sudo, or directly when connected as root. Requires system access.",
            "inputSchema": command_schema(),
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_run_container_action",
            "description": "Start, stop, or restart an existing Docker or Podman container currently listed by LeePanel. The exact container ID or name is required.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." },
                    "container": { "type": "string", "description": "Exact container ID or name returned by leepanel_list_containers." },
                    "action": { "type": "string", "enum": ["start", "stop", "restart"], "description": "Controlled lifecycle action to perform." }
                },
                "required": ["profile_id", "container", "action"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_list_images",
            "description": "List images from the Docker or Podman runtime selected by LeePanel on a currently connected server.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_list_sites",
            "description": "List Nginx sites on a server currently connected in LeePanel, including domains, roots, PHP, SSL, proxy, and enabled state.",
            "inputSchema": profile_schema(),
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "leepanel_set_site_enabled",
            "description": "Enable or disable an existing Nginx site managed by LeePanel. LeePanel validates the site, runs nginx -t, reloads Nginx, and reverts when validation fails.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." },
                    "domain": { "type": "string", "description": "Exact primary domain returned by leepanel_list_sites." },
                    "enabled": { "type": "boolean", "description": "True to enable the site; false to disable it." }
                },
                "required": ["profile_id", "domain", "enabled"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
    ]
}

fn profile_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." }
        },
        "required": ["profile_id"],
        "additionalProperties": false
    })
}

fn command_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "profile_id": { "type": "string", "description": "LeePanel profile ID returned by leepanel_list_servers." },
            "command": { "type": "string", "minLength": 1, "maxLength": 65536, "description": "Shell command or script body to execute." },
            "timeout_seconds": { "type": "integer", "minimum": 1, "maximum": 300, "default": 60 }
        },
        "required": ["profile_id", "command"],
        "additionalProperties": false
    })
}

fn call_broker(method: &str, params: Value) -> Result<Value, String> {
    let path = discovery_path();
    let discovery: BrokerDiscovery = serde_json::from_slice(
        &std::fs::read(&path)
            .map_err(|_| "LeePanel AI Broker is not running. Start LeePanel first.".to_string())?,
    )
    .map_err(|_| "LeePanel AI Broker discovery data is invalid. Restart LeePanel.".to_string())?;
    if discovery.version != 1 || discovery.host != "127.0.0.1" {
        return Err("LeePanel AI Broker discovery data is invalid. Restart LeePanel.".to_string());
    }
    let mut stream =
        TcpStream::connect((discovery.host.as_str(), discovery.port)).map_err(|_| {
            "LeePanel AI Broker is not reachable. Start or restart LeePanel.".to_string()
        })?;
    stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();
    let request = json!({ "token": discovery.token, "method": method, "params": params });
    serde_json::to_writer(&mut stream, &request).map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(MAX_REQUEST_BYTES)
        .read_line(&mut line)
        .map_err(|e| format!("LeePanel AI Broker response failed: {e}"))?;
    let response: BrokerResponse = serde_json::from_str(&line).map_err(|_| {
        "LeePanel AI Broker returned an invalid response. Restart LeePanel.".to_string()
    })?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(response
            .error
            .unwrap_or_else(|| "LeePanel AI Broker request failed".to_string()))
    }
}

fn write_json_line(writer: &mut impl Write, value: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    true
}

fn codex_file_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "codex.exe"
    } else {
        "codex"
    }
}

fn codex_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(codex_file_name()))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(target_os = "windows")]
fn bundled_codex() -> Option<PathBuf> {
    let root = dirs::data_local_dir()?
        .join("OpenAI")
        .join("Codex")
        .join("bin");
    let mut directories = vec![root];
    let mut matches = Vec::new();
    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                directories.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) == Some("codex.exe") {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok();
                matches.push((modified, path));
            }
        }
    }
    matches.sort_by_key(|(modified, _)| *modified);
    matches.pop().map(|(_, path)| path)
}

fn find_codex() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    if let Some(path) = bundled_codex() {
        return Some(path);
    }

    if let Some(path) = codex_on_path() {
        return Some(path);
    }

    let mut candidates = Vec::new();
    if let Some(home) = dirs::home_dir() {
        candidates.extend([
            home.join(".local/bin/codex"),
            home.join(".cargo/bin/codex"),
            home.join(".npm-global/bin/codex"),
        ]);
        #[cfg(target_os = "macos")]
        candidates.extend([
            home.join("Applications/Codex.app/Contents/Resources/codex"),
            home.join("Applications/ChatGPT.app/Contents/Resources/codex"),
        ]);
    }
    #[cfg(target_os = "macos")]
    candidates.extend([
        PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
        PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ]);
    #[cfg(target_os = "linux")]
    candidates.extend([
        PathBuf::from("/usr/local/bin/codex"),
        PathBuf::from("/usr/bin/codex"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin/codex"),
    ]);
    candidates
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
}

fn select_mcp_executable(
    current: PathBuf,
    appimage: Option<PathBuf>,
    use_appimage: bool,
) -> PathBuf {
    if use_appimage {
        if let Some(path) = appimage.filter(|path| is_executable_file(path)) {
            return path;
        }
    }
    current
}

fn mcp_executable() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from);
    Ok(select_mcp_executable(
        current,
        appimage,
        cfg!(target_os = "linux"),
    ))
}

fn comparable_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        }
    })
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let left = comparable_path(left);
    let right = comparable_path(right);
    if cfg!(target_os = "windows") {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn command_error(action: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    if detail.is_empty() {
        format!("{action} failed with status {}", output.status)
    } else {
        format!("{action} failed: {detail}")
    }
}

fn child_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn codex_output(codex: &Path, args: &[&str]) -> Result<Output, String> {
    child_command(codex)
        .args(args)
        .output()
        .map_err(|e| format!("Failed to run Codex CLI at {}: {e}", codex.display()))
}

fn get_codex_registration(codex: &Path) -> Result<Option<CodexMcpRegistration>, String> {
    let output = codex_output(codex, &["mcp", "get", "leepanel", "--json"])?;
    if !output.status.success() {
        return Ok(None);
    }
    serde_json::from_slice(&output.stdout)
        .map(Some)
        .map_err(|e| format!("Codex returned invalid LeePanel MCP registration data: {e}"))
}

fn remove_codex_registration(codex: &Path) -> Result<(), String> {
    let output = codex_output(codex, &["mcp", "remove", "leepanel"])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("Removing LeePanel MCP registration", &output))
    }
}

fn add_codex_registration(
    codex: &Path,
    command: &Path,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<(), String> {
    let mut command_args: Vec<OsString> = vec!["mcp".into(), "add".into(), "leepanel".into()];
    for (key, value) in env {
        command_args.push("--env".into());
        command_args.push(format!("{key}={value}").into());
    }
    command_args.push("--".into());
    command_args.push(command.as_os_str().to_owned());
    command_args.extend(args.iter().map(OsString::from));
    let output = child_command(codex)
        .args(&command_args)
        .output()
        .map_err(|e| format!("Failed to run Codex CLI at {}: {e}", codex.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_error("Adding LeePanel MCP registration", &output))
    }
}

fn registration_is_current(registration: &CodexMcpRegistration, executable: &Path) -> bool {
    registration.enabled
        && paths_equal(Path::new(&registration.transport.command), executable)
        && registration.transport.args == ["--mcp"]
}

fn restore_codex_registration(codex: &Path, previous: Option<&CodexMcpRegistration>) -> bool {
    let _ = remove_codex_registration(codex);
    previous
        .map(|registration| {
            let env = registration.transport.env.clone().unwrap_or_default();
            add_codex_registration(
                codex,
                Path::new(&registration.transport.command),
                &registration.transport.args,
                &env,
            )
            .is_ok()
        })
        .unwrap_or(true)
}

fn mcp_version_at(executable: &Path) -> String {
    child_command(executable)
        .arg("--mcp-version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

fn register_mcp() -> Result<(), String> {
    let codex = find_codex().ok_or_else(|| "ChatGPT / Codex CLI was not found".to_string())?;
    let executable = mcp_executable()?;
    if mcp_version_at(&executable).is_empty() {
        return Err(format!(
            "LeePanel MCP executable could not be verified: {}",
            executable.display()
        ));
    }
    let previous = get_codex_registration(&codex)?;
    if previous
        .as_ref()
        .is_some_and(|registration| registration_is_current(registration, &executable))
    {
        return Ok(());
    }
    if previous.is_some() {
        remove_codex_registration(&codex)?;
    }
    let args = vec!["--mcp".to_string()];
    if let Err(error) = add_codex_registration(&codex, &executable, &args, &BTreeMap::new()) {
        let restored = restore_codex_registration(&codex, previous.as_ref());
        return Err(format!(
            "{error}. Previous registration restored: {restored}"
        ));
    }
    let verified = match get_codex_registration(&codex) {
        Ok(registration) => registration
            .as_ref()
            .is_some_and(|registration| registration_is_current(registration, &executable)),
        Err(error) => {
            let restored = restore_codex_registration(&codex, previous.as_ref());
            return Err(format!(
                "{error}. Previous registration restored: {restored}"
            ));
        }
    };
    if !verified {
        let restored = restore_codex_registration(&codex, previous.as_ref());
        return Err(format!(
            "LeePanel MCP registration verification failed. Previous registration restored: {restored}"
        ));
    }
    Ok(())
}

fn unregister_mcp() -> Result<(), String> {
    let Some(codex) = find_codex() else {
        return Ok(());
    };
    let executable = mcp_executable()?;
    let Some(registration) = get_codex_registration(&codex)? else {
        return Ok(());
    };
    if registration_is_current(&registration, &executable) {
        remove_codex_registration(&codex)?;
    }
    Ok(())
}

fn set_mcp_enabled(conn: &rusqlite::Connection, enabled: bool) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('mcp_enabled', ?1)",
        params![enabled.to_string()],
    )
    .map_err(|e| format!("Failed to update MCP setting: {e}"))?;
    Ok(())
}

fn registration_status(enabled: bool) -> Result<McpRegistrationStatus, String> {
    let codex = find_codex();
    let executable = mcp_executable()?;
    let registration = codex
        .as_deref()
        .map(get_codex_registration)
        .transpose()?
        .flatten();
    Ok(McpRegistrationStatus {
        codex_found: codex.is_some(),
        codex_path: codex
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        registered: registration.is_some(),
        current: registration
            .as_ref()
            .is_some_and(|registration| registration_is_current(registration, &executable)),
        registered_path: registration
            .as_ref()
            .map(|registration| registration.transport.command.clone())
            .unwrap_or_default(),
        version: mcp_version_at(&executable),
        enabled,
    })
}

#[tauri::command]
pub async fn mcp_get_status(db: tauri::State<'_, DbPool>) -> Result<McpRegistrationStatus, String> {
    let enabled = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        mcp_enabled(&conn)
    };
    tauri::async_runtime::spawn_blocking(move || registration_status(enabled))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn mcp_register(db: tauri::State<'_, DbPool>) -> Result<McpRegistrationStatus, String> {
    tauri::async_runtime::spawn_blocking(register_mcp)
        .await
        .map_err(|e| e.to_string())??;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        set_mcp_enabled(&conn, true)?;
    }
    let status = tauri::async_runtime::spawn_blocking(move || registration_status(true))
        .await
        .map_err(|e| e.to_string())??;
    if !status.current {
        let conn = db.lock().map_err(|e| e.to_string())?;
        set_mcp_enabled(&conn, false)?;
        return Err("LeePanel MCP registration could not be verified".to_string());
    }
    Ok(status)
}

#[tauri::command]
pub async fn mcp_unregister(db: tauri::State<'_, DbPool>) -> Result<McpRegistrationStatus, String> {
    tauri::async_runtime::spawn_blocking(unregister_mcp)
        .await
        .map_err(|e| e.to_string())??;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        set_mcp_enabled(&conn, false)?;
        conn.execute("DELETE FROM mcp_permissions", [])
            .map_err(|e| format!("Failed to revoke MCP permissions: {e}"))?;
    }
    tauri::async_runtime::spawn_blocking(move || registration_status(false))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn mcp_list_permissions(
    db: tauri::State<'_, DbPool>,
) -> Result<Vec<McpServerPermission>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let profiles = ConfigManager::list(&conn);
    Ok(profiles
        .into_iter()
        .map(|profile| {
            McpServerPermission {
                permission_level: permission_level(&conn, &profile.id).as_str().to_string(),
                profile_id: profile.id,
                name: profile.name,
                host: profile.host,
                port: profile.port,
                username: profile.username,
            }
        })
        .collect())
}

#[tauri::command]
pub fn mcp_set_server_permission(
    db: tauri::State<'_, DbPool>,
    profile_id: String,
    permission_level: String,
) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    if !ConfigManager::list(&conn)
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return Err(format!("LeePanel server profile not found: {profile_id}"));
    }
    let level = PermissionLevel::parse(&permission_level)
        .ok_or_else(|| "Permission level must be one of: none, read, manage, system".to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO mcp_permissions (profile_id, permission_level) VALUES (?1, ?2)",
        params![profile_id, level.as_str()],
    )
    .map_err(|e| format!("Failed to save MCP permission: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn mcp_list_audit(db: tauri::State<'_, DbPool>) -> Result<Vec<McpAuditEntry>, String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    let mut statement = conn
        .prepare("SELECT id, created_at, profile_id, method, target, success, message FROM mcp_audit ORDER BY id DESC LIMIT 100")
        .map_err(|e| e.to_string())?;
    let entries = statement
        .query_map([], |row| {
            Ok(McpAuditEntry {
                id: row.get(0)?,
                created_at: row.get(1)?,
                profile_id: row.get(2)?,
                method: row.get(3)?,
                target: row.get(4)?,
                success: row.get::<_, i64>(5)? == 1,
                message: row.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_approved_tools() {
        let tools = tool_definitions();
        let names: Vec<_> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names,
            vec![
                "leepanel_list_servers",
                "leepanel_get_server_status",
                "leepanel_get_services",
                "leepanel_get_nginx_status",
                "leepanel_get_container_runtime",
                "leepanel_list_containers",
                "leepanel_get_container_logs",
                "leepanel_read_file",
                "leepanel_write_file",
                "leepanel_run_command",
                "leepanel_run_privileged_command",
                "leepanel_run_container_action",
                "leepanel_list_images",
                "leepanel_list_sites",
                "leepanel_set_site_enabled",
            ]
        );
    }

    #[test]
    fn initialize_advertises_tools() {
        let response = handle_mcp_request(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "2025-06-18" }
        }));
        assert_eq!(
            response.pointer("/result/serverInfo/name"),
            Some(&json!("leepanel"))
        );
        assert_eq!(
            response.pointer("/result/capabilities/tools/listChanged"),
            Some(&json!(false))
        );
    }

    #[test]
    fn parses_real_codex_registration_shape() {
        let registration: CodexMcpRegistration = serde_json::from_value(json!({
            "name": "leepanel",
            "enabled": true,
            "transport": {
                "type": "stdio",
                "command": "/Applications/LeePanel.app/Contents/MacOS/leepanel",
                "args": ["--mcp"],
                "env": null,
                "env_vars": [],
                "cwd": null
            }
        }))
        .unwrap();
        assert!(registration.enabled);
        assert_eq!(registration.transport.args, ["--mcp"]);
        assert!(registration.transport.env.is_none());
    }

    #[test]
    fn registration_requires_the_exact_executable_and_mcp_argument() {
        let executable = std::env::current_exe().unwrap();
        let mut registration = CodexMcpRegistration {
            enabled: true,
            transport: CodexMcpTransport {
                command: executable.to_string_lossy().into_owned(),
                args: vec!["--mcp".to_string()],
                env: None,
            },
        };
        assert!(registration_is_current(&registration, &executable));
        registration.enabled = false;
        assert!(!registration_is_current(&registration, &executable));
        registration.enabled = true;
        registration.transport.args.push("extra".to_string());
        assert!(!registration_is_current(&registration, &executable));
    }

    #[test]
    fn appimage_path_replaces_the_temporary_mount_executable() {
        let appimage = std::env::current_exe().unwrap();
        let selected = select_mcp_executable(
            PathBuf::from("/tmp/.mount_LeePanel/leepanel"),
            Some(appimage.clone()),
            true,
        );
        assert_eq!(selected, appimage);
    }

    #[test]
    fn site_toggle_schema_requires_exact_target() {
        let tools = tool_definitions();
        let toggle = tools
            .iter()
            .find(|tool| tool["name"] == "leepanel_set_site_enabled")
            .unwrap();
        assert_eq!(
            toggle.pointer("/inputSchema/required"),
            Some(&json!(["profile_id", "domain", "enabled"]))
        );
        assert_eq!(
            toggle.pointer("/annotations/readOnlyHint"),
            Some(&json!(false))
        );
    }

    #[test]
    fn second_iteration_tools_are_read_only() {
        let tools = tool_definitions();
        for name in [
            "leepanel_get_services",
            "leepanel_get_nginx_status",
            "leepanel_get_container_runtime",
            "leepanel_list_containers",
            "leepanel_get_container_logs",
            "leepanel_list_images",
        ] {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            assert_eq!(
                tool.pointer("/annotations/readOnlyHint"),
                Some(&json!(true))
            );
        }
    }

    #[test]
    fn log_line_limit_is_bounded() {
        assert_eq!(optional_usize(&json!({}), "lines", 200, 1000), Ok(200));
        assert_eq!(
            optional_usize(&json!({ "lines": 1000 }), "lines", 200, 1000),
            Ok(1000)
        );
        assert!(optional_usize(&json!({ "lines": 0 }), "lines", 200, 1000).is_err());
        assert!(optional_usize(&json!({ "lines": 1001 }), "lines", 200, 1000).is_err());
    }

    #[test]
    fn container_action_schema_is_restricted() {
        let tools = tool_definitions();
        let action = tools
            .iter()
            .find(|tool| tool["name"] == "leepanel_run_container_action")
            .unwrap();
        assert_eq!(
            action.pointer("/inputSchema/properties/action/enum"),
            Some(&json!(["start", "stop", "restart"]))
        );
        assert_eq!(
            action.pointer("/inputSchema/required"),
            Some(&json!(["profile_id", "container", "action"]))
        );
        assert_eq!(
            action.pointer("/annotations/destructiveHint"),
            Some(&json!(true))
        );
    }

    #[test]
    fn container_action_rejects_unapproved_operations() {
        for action in ["start", "stop", "restart"] {
            assert_eq!(
                required_container_action(&json!({ "action": action })),
                Ok(action)
            );
        }
        for action in ["pause", "unpause", "delete", "rm", "exec"] {
            assert!(required_container_action(&json!({ "action": action })).is_err());
        }
    }

    fn access_test_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE mcp_permissions (
                profile_id TEXT PRIMARY KEY,
                permission_level TEXT NOT NULL DEFAULT 'none'
             );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn mcp_is_disabled_and_profiles_are_denied_by_default() {
        let conn = access_test_db();
        assert!(!mcp_enabled(&conn));
        for access in [PermissionLevel::Read, PermissionLevel::Manage, PermissionLevel::System] {
            assert!(!has_access(&conn, "root-profile", access));
        }
    }

    #[test]
    fn permission_levels_inherit_lower_access() {
        let conn = access_test_db();
        set_mcp_enabled(&conn, true).unwrap();
        conn.execute(
            "INSERT INTO mcp_permissions (profile_id, permission_level) VALUES (?1, 'manage')",
            params!["profile-1"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mcp_permissions (profile_id, permission_level) VALUES (?1, 'system')",
            params!["profile-2"],
        )
        .unwrap();
        assert!(mcp_enabled(&conn));
        assert!(has_access(&conn, "profile-1", PermissionLevel::Read));
        assert!(has_access(&conn, "profile-1", PermissionLevel::Manage));
        assert!(!has_access(&conn, "profile-1", PermissionLevel::System));
        assert!(has_access(&conn, "profile-2", PermissionLevel::Read));
        assert!(has_access(&conn, "profile-2", PermissionLevel::Manage));
        assert!(has_access(&conn, "profile-2", PermissionLevel::System));
    }

    #[test]
    fn management_commands_reject_direct_privilege_escalation() {
        for command in ["sudo systemctl restart nginx", "su - root", "doas reboot", "pkexec sh"] {
            assert!(reject_privilege_escalation(command).is_err());
        }
        for command in ["systemctl --user status app", "bash ./deploy.sh", "echo sudokus"] {
            assert!(reject_privilege_escalation(command).is_ok());
        }
    }

    #[test]
    fn utf8_output_truncation_preserves_character_boundaries() {
        assert_eq!(truncate_utf8("你好abc", 4), ("你".to_string(), true));
        assert_eq!(truncate_utf8("abc", 4), ("abc".to_string(), false));
    }
}
