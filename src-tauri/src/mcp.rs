use crate::{config::ConfigManager, server, ssh::SshManager, DbPool};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::PathBuf,
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
    pub read_access: bool,
    pub site_manage: bool,
    pub container_manage: bool,
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

#[derive(Clone, Copy)]
enum RequiredAccess {
    Read,
    SiteManage,
    ContainerManage,
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
    for key in ["container", "domain", "profile_id"] {
        if let Some(value) = params.get(key).and_then(Value::as_str) {
            return value.chars().take(200).collect();
        }
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

fn has_access(conn: &rusqlite::Connection, profile_id: &str, required: RequiredAccess) -> bool {
    let column = match required {
        RequiredAccess::Read => "read_access",
        RequiredAccess::SiteManage => "site_manage",
        RequiredAccess::ContainerManage => "container_manage",
    };
    conn.query_row(
        &format!("SELECT {column} FROM mcp_permissions WHERE profile_id = ?1"),
        params![profile_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value == 1)
    .unwrap_or(false)
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
                    .filter(|profile| has_access(&conn, &profile.id, RequiredAccess::Read))
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
                active_session(app, profile_id, RequiredAccess::Read).await?;
            let system = server::get_system_info(&session, &cache, &session_id).await?;
            let services = server::get_service_statuses(&session, &cache, &session_id).await?;
            Ok(json!({ "system": system, "services": services }))
        }
        "get_services" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, RequiredAccess::Read).await?;
            let services = server::get_service_statuses(&session, &cache, &session_id).await?;
            serde_json::to_value(services).map_err(|e| e.to_string())
        }
        "get_nginx_status" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, RequiredAccess::Read).await?;
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
                active_session(app, profile_id, RequiredAccess::Read).await?;
            let status = server::check_docker(&session, &cache, &session_id).await?;
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "list_containers" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, RequiredAccess::Read).await?;
            let containers = server::docker_container_list(&session, &cache, &session_id).await?;
            serde_json::to_value(containers).map_err(|e| e.to_string())
        }
        "get_container_logs" => {
            let profile_id = required_string(&params, "profile_id")?;
            let requested = required_string(&params, "container")?;
            let lines = optional_usize(&params, "lines", 200, 1000)?;
            let (session, cache, session_id) =
                active_session(app, profile_id, RequiredAccess::Read).await?;
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
        "run_container_action" => {
            let profile_id = required_string(&params, "profile_id")?;
            let requested = required_string(&params, "container")?;
            let action = required_container_action(&params)?;
            let (session, cache, session_id) =
                active_session(app, profile_id, RequiredAccess::ContainerManage).await?;
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
                active_session(app, profile_id, RequiredAccess::Read).await?;
            let images = server::docker_image_list(&session, &cache, &session_id).await?;
            serde_json::to_value(images).map_err(|e| e.to_string())
        }
        "list_sites" => {
            let profile_id = required_string(&params, "profile_id")?;
            let (session, cache, session_id) =
                active_session(app, profile_id, RequiredAccess::Read).await?;
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
                active_session(app, profile_id, RequiredAccess::SiteManage).await?;
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
    required: RequiredAccess,
) -> Result<(crate::ssh::SshSession, Arc<crate::ssh::SshCache>, String), String> {
    let db = app.state::<DbPool>();
    let profile = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        if !mcp_enabled(&conn) {
            return Err(
                "LeePanel MCP is disabled. Enable it in MCP / AI Integration settings.".to_string(),
            );
        }
        if !has_access(&conn, profile_id, required) {
            return Err(format!(
                "MCP access is not authorized for LeePanel server profile: {profile_id}"
            ));
        }
        ConfigManager::list(&conn)
            .into_iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| format!("LeePanel server profile not found: {profile_id}"))?
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

#[cfg(target_os = "windows")]
fn resource_script(app: &AppHandle, name: &str) -> Result<PathBuf, String> {
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    [
        resource_dir.join("resources").join(name),
        resource_dir.join(name),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .ok_or_else(|| format!("LeePanel MCP resource was not found: {name}"))
}

#[cfg(target_os = "windows")]
fn run_mcp_script(app: &AppHandle, name: &str) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let script = resource_script(app, name)?;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let output = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(script)
        .arg("-LeePanelPath")
        .arg(executable)
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("Failed to run LeePanel MCP script: {e}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if error.is_empty() {
            format!("LeePanel MCP script failed with status {}", output.status)
        } else {
            error
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(not(target_os = "windows"))]
fn run_mcp_script(_app: &AppHandle, _name: &str) -> Result<String, String> {
    Err("LeePanel MCP registration is currently supported on Windows only".to_string())
}

fn set_mcp_enabled(conn: &rusqlite::Connection, enabled: bool) -> Result<(), String> {
    conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('mcp_enabled', ?1)",
        params![enabled.to_string()],
    )
    .map_err(|e| format!("Failed to update MCP setting: {e}"))?;
    Ok(())
}

fn registration_status(app: &AppHandle, enabled: bool) -> Result<McpRegistrationStatus, String> {
    let output = run_mcp_script(app, "get_leepanel_mcp_status.ps1")?;
    let value: Value = serde_json::from_str(
        output
            .lines()
            .last()
            .ok_or_else(|| "LeePanel MCP status returned no data".to_string())?,
    )
    .map_err(|e| format!("Invalid LeePanel MCP status: {e}"))?;
    Ok(McpRegistrationStatus {
        codex_found: value["codex_found"].as_bool().unwrap_or(false),
        codex_path: value["codex_path"].as_str().unwrap_or("").to_string(),
        registered: value["registered"].as_bool().unwrap_or(false),
        current: value["current"].as_bool().unwrap_or(false),
        registered_path: value["registered_path"].as_str().unwrap_or("").to_string(),
        version: value["version"].as_str().unwrap_or("").to_string(),
        enabled,
    })
}

#[tauri::command]
pub async fn mcp_get_status(
    app: AppHandle,
    db: tauri::State<'_, DbPool>,
) -> Result<McpRegistrationStatus, String> {
    let enabled = {
        let conn = db.lock().map_err(|e| e.to_string())?;
        mcp_enabled(&conn)
    };
    tauri::async_runtime::spawn_blocking(move || registration_status(&app, enabled))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn mcp_register(
    app: AppHandle,
    db: tauri::State<'_, DbPool>,
) -> Result<McpRegistrationStatus, String> {
    let app_for_script = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_mcp_script(&app_for_script, "register_leepanel_mcp.ps1")
    })
    .await
    .map_err(|e| e.to_string())??;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        set_mcp_enabled(&conn, true)?;
    }
    let status = tauri::async_runtime::spawn_blocking(move || registration_status(&app, true))
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
pub async fn mcp_unregister(
    app: AppHandle,
    db: tauri::State<'_, DbPool>,
) -> Result<McpRegistrationStatus, String> {
    let app_for_script = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_mcp_script(&app_for_script, "unregister_leepanel_mcp.ps1")
    })
    .await
    .map_err(|e| e.to_string())??;
    {
        let conn = db.lock().map_err(|e| e.to_string())?;
        set_mcp_enabled(&conn, false)?;
        conn.execute("DELETE FROM mcp_permissions", [])
            .map_err(|e| format!("Failed to revoke MCP permissions: {e}"))?;
    }
    tauri::async_runtime::spawn_blocking(move || registration_status(&app, false))
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
            let permission = conn
                .query_row(
                    "SELECT read_access, site_manage, container_manage FROM mcp_permissions WHERE profile_id = ?1",
                    params![profile.id],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
                )
                .unwrap_or((0, 0, 0));
            McpServerPermission {
                profile_id: profile.id,
                name: profile.name,
                host: profile.host,
                port: profile.port,
                username: profile.username,
                read_access: permission.0 == 1,
                site_manage: permission.1 == 1,
                container_manage: permission.2 == 1,
            }
        })
        .collect())
}

#[tauri::command]
pub fn mcp_set_server_permission(
    db: tauri::State<'_, DbPool>,
    profile_id: String,
    read_access: bool,
    site_manage: bool,
    container_manage: bool,
) -> Result<(), String> {
    let conn = db.lock().map_err(|e| e.to_string())?;
    if !ConfigManager::list(&conn)
        .iter()
        .any(|profile| profile.id == profile_id)
    {
        return Err(format!("LeePanel server profile not found: {profile_id}"));
    }
    let effective_read = read_access || site_manage || container_manage;
    conn.execute(
        "INSERT OR REPLACE INTO mcp_permissions (profile_id, read_access, site_manage, container_manage) VALUES (?1, ?2, ?3, ?4)",
        params![profile_id, effective_read as i64, site_manage as i64, container_manage as i64],
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
                read_access INTEGER NOT NULL DEFAULT 0,
                site_manage INTEGER NOT NULL DEFAULT 0,
                container_manage INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn mcp_is_disabled_and_profiles_are_denied_by_default() {
        let conn = access_test_db();
        assert!(!mcp_enabled(&conn));
        for access in [
            RequiredAccess::Read,
            RequiredAccess::SiteManage,
            RequiredAccess::ContainerManage,
        ] {
            assert!(!has_access(&conn, "root-profile", access));
        }
    }

    #[test]
    fn permissions_are_scoped_independently() {
        let conn = access_test_db();
        set_mcp_enabled(&conn, true).unwrap();
        conn.execute(
            "INSERT INTO mcp_permissions (profile_id, read_access, site_manage, container_manage) VALUES (?1, 1, 0, 1)",
            params!["profile-1"],
        )
        .unwrap();
        assert!(mcp_enabled(&conn));
        assert!(has_access(&conn, "profile-1", RequiredAccess::Read));
        assert!(has_access(
            &conn,
            "profile-1",
            RequiredAccess::ContainerManage
        ));
        assert!(!has_access(&conn, "profile-1", RequiredAccess::SiteManage));
    }
}
