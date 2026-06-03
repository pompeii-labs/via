use crate::model::{Mesh, Node};
use crate::paths::ViaPaths;
use crate::rpc::{RpcRequest, RpcResponse};
use crate::state::ViaState;
use crate::util::now_ts;
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use lux::{EmbeddedClient, ServerConfig, ServerHandle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{sleep, timeout, Duration};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite;
use url::Url;
use uuid::Uuid;

const INIT_HUB: &str = include_str!("../lux/migrations/20260602000000_init_hub.lux");
const TABLES: &[&str] = &[
    "meshes", "nodes", "tokens", "sessions", "cmds", "events", "audit",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubConfig {
    pub url: String,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteToken {
    pub v: u8,
    pub hub: String,
    pub mesh: String,
    pub key: String,
    pub token: String,
    pub name: Option<String>,
    pub exp: i64,
}

#[derive(Debug)]
pub struct HubAgentRpc {
    pub req: String,
    pub reply: oneshot::Sender<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentQuery {
    mesh: String,
    node: String,
    slug: String,
    token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HubToAgent {
    Rpc { id: String, req: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentToHub {
    RpcResult { id: String, res: String },
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateMeshRequest {
    id: String,
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct CreateTokenRequest {
    mesh: String,
    name: Option<String>,
    token: String,
    exp: i64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PostCmdRequest {
    pub mesh: String,
    pub src: String,
    pub dst: String,
    pub req: String,
    pub token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CmdResponse {
    pub id: String,
    pub state: String,
    pub res: Option<String>,
    pub err: Option<String>,
}

type SessionKey = (String, String);

struct HubRuntime {
    db: HubDb,
    sessions: Mutex<HashMap<SessionKey, mpsc::Sender<HubToAgent>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<String>>>,
    results: Mutex<HashMap<String, CmdResponse>>,
}

pub fn configured(paths: &ViaPaths) -> bool {
    paths.hub_config.exists()
}

pub fn load_config(paths: &ViaPaths) -> Result<Option<HubConfig>> {
    if !paths.hub_config.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&paths.hub_config)
        .with_context(|| format!("failed to read {}", paths.hub_config.display()))?;
    Ok(Some(serde_json::from_str(&raw)?))
}

pub fn save_config(paths: &ViaPaths, config: &HubConfig) -> Result<()> {
    paths.ensure()?;
    let raw = serde_json::to_string_pretty(config)?;
    std::fs::write(&paths.hub_config, raw)?;
    Ok(())
}

pub async fn use_hub(state: &ViaState, paths: &ViaPaths, url: String) -> Result<()> {
    let url = normalize_http_url(&url)?;
    save_config(
        paths,
        &HubConfig {
            url: url.clone(),
            token: load_config(paths)?.and_then(|config| config.token),
        },
    )?;
    if let Some(mesh) = state.mesh().await? {
        let client = reqwest::Client::new();
        let _ = client
            .post(format!("{url}/v1/meshes"))
            .json(&CreateMeshRequest {
                id: mesh.id,
                name: "default".to_string(),
            })
            .send()
            .await;
    }
    println!("Via hub set to {url}.");
    Ok(())
}

pub async fn create_invite(
    state: &ViaState,
    paths: &ViaPaths,
    name: Option<String>,
    ttl: i64,
) -> Result<String> {
    let config = load_config(paths)?.ok_or_else(|| anyhow!("run `via hub use <url>` first"))?;
    let mesh = state
        .mesh()
        .await?
        .ok_or_else(|| anyhow!("run `via init` before creating invites"))?;
    let key = std::fs::read_to_string(&paths.mesh_key)
        .with_context(|| format!("failed to read {}", paths.mesh_key.display()))?;
    let token_secret = crate::security::nonce()?;
    let exp = now_ts() + ttl.max(60);
    let invite = InviteToken {
        v: 1,
        hub: config.url.clone(),
        mesh: mesh.id.clone(),
        key: key.trim().to_string(),
        token: token_secret.clone(),
        name: name.clone(),
        exp,
    };

    let client = reqwest::Client::new();
    client
        .post(format!("{}/v1/meshes", config.url))
        .json(&CreateMeshRequest {
            id: mesh.id.clone(),
            name: "default".to_string(),
        })
        .send()
        .await?
        .error_for_status()?;
    client
        .post(format!("{}/v1/tokens", config.url))
        .json(&CreateTokenRequest {
            mesh: mesh.id,
            name,
            token: token_secret,
            exp,
        })
        .send()
        .await?
        .error_for_status()?;

    Ok(format!(
        "via1.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&invite)?)
    ))
}

pub async fn join(paths: &ViaPaths, name: Option<String>, token: String) -> Result<()> {
    let invite = decode_invite(&token)?;
    if invite.exp < now_ts() {
        bail!("invite token expired");
    }
    crate::security::install_mesh_key(paths, &invite.key)?;
    save_config(
        paths,
        &HubConfig {
            url: invite.hub.clone(),
            token: Some(invite.token),
        },
    )?;
    let mut state = ViaState::open(paths.clone()).await?;
    let node_name = name.or(invite.name);
    crate::commands::init(
        &mut state,
        node_name,
        Some(invite.mesh),
        Some(Uuid::new_v4().to_string()),
    )
    .await?;
    state.shutdown().await?;
    Ok(())
}

pub async fn call_node(state: &ViaState, node: &Node, request: RpcRequest) -> Result<RpcResponse> {
    let config =
        load_config(state.paths())?.ok_or_else(|| anyhow!("run `via hub use <url>` first"))?;
    let mesh = state
        .mesh()
        .await?
        .ok_or_else(|| anyhow!("run `via init` first"))?;
    let local = state.local_node().await?;
    let req = String::from_utf8(crate::rpc::encode_request(state.paths(), request)?)?;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/cmds", config.url))
        .json(&PostCmdRequest {
            mesh: mesh.id,
            src: local.slug,
            dst: node.slug.clone(),
            req,
            token: config.token,
        })
        .send()
        .await?
        .error_for_status()?
        .json::<CmdResponse>()
        .await?;
    if let Some(error) = response.err {
        bail!(error);
    }
    let res = response
        .res
        .ok_or_else(|| anyhow!("hub command returned no response"))?;
    crate::rpc::decode_response(state.paths(), &res)
}

pub fn spawn_agent(paths: ViaPaths, mesh: Mesh, node: Node) -> Option<mpsc::Receiver<HubAgentRpc>> {
    let config = match load_config(&paths) {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(error) => {
            eprintln!("via hub config ignored: {error}");
            return None;
        }
    };
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(agent_loop(config, mesh, node, tx));
    Some(rx)
}

pub async fn start(bind: String, lux_dir: Option<String>, migrate: bool) -> Result<()> {
    let db = HubDb::open(lux_dir).await?;
    if migrate {
        db.migrate().await?;
    } else {
        db.check_schema().await?;
    }
    let runtime = Arc::new(HubRuntime {
        db,
        sessions: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        results: Mutex::new(HashMap::new()),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/agent/connect", get(agent_connect))
        .route("/v1/meshes", post(create_mesh))
        .route("/v1/tokens", post(create_token))
        .route("/v1/cmds", post(post_cmd))
        .route("/v1/cmds/{id}", get(get_cmd))
        .with_state(runtime);
    let listener = TcpListener::bind(&bind).await?;
    println!("via hub listening on {}", listener.local_addr()?);
    axum::serve(listener, app).await?;
    Ok(())
}

pub async fn migrate(lux_dir: Option<String>) -> Result<()> {
    let db = HubDb::open(lux_dir).await?;
    db.migrate().await?;
    db.persist().await
}

async fn health() -> &'static str {
    "ok"
}

async fn create_mesh(
    State(runtime): State<Arc<HubRuntime>>,
    Json(req): Json<CreateMeshRequest>,
) -> impl IntoResponse {
    match runtime.db.insert_mesh(&req.id, &req.name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn create_token(
    State(runtime): State<Arc<HubRuntime>>,
    Json(req): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    match runtime
        .db
        .insert_token(&req.mesh, req.name.as_deref(), &req.token, req.exp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn agent_connect(
    ws: WebSocketUpgrade,
    State(runtime): State<Arc<HubRuntime>>,
    Query(query): Query<AgentQuery>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_agent(socket, runtime, query))
}

async fn post_cmd(
    State(runtime): State<Arc<HubRuntime>>,
    Json(req): Json<PostCmdRequest>,
) -> impl IntoResponse {
    let id = Uuid::new_v4().to_string();
    if let Err(error) = runtime.db.insert_cmd(&id, &req).await {
        return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
    }
    let session = {
        let sessions = runtime.sessions.lock().await;
        sessions.get(&(req.mesh.clone(), req.dst.clone())).cloned()
    };
    let Some(session) = session else {
        let response = CmdResponse {
            id,
            state: "error".to_string(),
            res: None,
            err: Some(format!("node '{}' is not connected to the hub", req.dst)),
        };
        return (StatusCode::NOT_FOUND, Json(response)).into_response();
    };

    let (tx, rx) = oneshot::channel();
    runtime.pending.lock().await.insert(id.clone(), tx);
    if let Err(error) = session
        .send(HubToAgent::Rpc {
            id: id.clone(),
            req: req.req,
        })
        .await
    {
        runtime.pending.lock().await.remove(&id);
        let response = CmdResponse {
            id,
            state: "error".to_string(),
            res: None,
            err: Some(format!("failed to relay command: {error}")),
        };
        return (StatusCode::BAD_GATEWAY, Json(response)).into_response();
    }

    let response = match timeout(Duration::from_secs(60), rx).await {
        Ok(Ok(res)) => CmdResponse {
            id: id.clone(),
            state: "done".to_string(),
            res: Some(res),
            err: None,
        },
        Ok(Err(_)) => CmdResponse {
            id: id.clone(),
            state: "error".to_string(),
            res: None,
            err: Some("agent disconnected before replying".to_string()),
        },
        Err(_) => CmdResponse {
            id: id.clone(),
            state: "error".to_string(),
            res: None,
            err: Some("command timed out waiting for agent".to_string()),
        },
    };
    let _ = runtime.db.finish_cmd(&response).await;
    runtime
        .results
        .lock()
        .await
        .insert(id.clone(), response.clone());
    Json(response).into_response()
}

async fn get_cmd(
    State(runtime): State<Arc<HubRuntime>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let results = runtime.results.lock().await;
    match results.get(&id).cloned() {
        Some(response) => Json(response).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn handle_agent(mut socket: WebSocket, runtime: Arc<HubRuntime>, query: AgentQuery) {
    let (tx, mut rx) = mpsc::channel(64);
    runtime
        .sessions
        .lock()
        .await
        .insert((query.mesh.clone(), query.slug.clone()), tx);
    let _ = runtime.db.insert_node(&query).await;
    let session_id = Uuid::new_v4().to_string();
    let _ = runtime.db.insert_session(&session_id, &query).await;

    loop {
        tokio::select! {
            Some(outbound) = rx.recv() => {
                match serde_json::to_string(&outbound) {
                    Ok(text) => {
                        if socket.send(Message::Text(text.into())).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            inbound = socket.next() => {
                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(AgentToHub::RpcResult { id, res }) = serde_json::from_str(&text) {
                            if let Some(reply) = runtime.pending.lock().await.remove(&id) {
                                let _ = reply.send(res);
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
    runtime
        .sessions
        .lock()
        .await
        .remove(&(query.mesh, query.slug));
}

async fn agent_loop(config: HubConfig, mesh: Mesh, node: Node, tx: mpsc::Sender<HubAgentRpc>) {
    loop {
        match run_agent_session(&config, &mesh, &node, &tx).await {
            Ok(()) => {}
            Err(error) => eprintln!("via hub session lost: {error}"),
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn run_agent_session(
    config: &HubConfig,
    mesh: &Mesh,
    node: &Node,
    tx: &mpsc::Sender<HubAgentRpc>,
) -> Result<()> {
    let url = agent_ws_url(config, mesh, node)?;
    let (socket, _) = connect_async(url.as_str()).await?;
    let (mut write, mut read) = socket.split();
    while let Some(message) = read.next().await {
        let message = message?;
        let tungstenite::Message::Text(text) = message else {
            continue;
        };
        let HubToAgent::Rpc { id, req } = serde_json::from_str(&text)?;
        let (reply_tx, reply_rx) = oneshot::channel();
        tx.send(HubAgentRpc {
            req,
            reply: reply_tx,
        })
        .await?;
        let res = reply_rx.await?;
        let outbound = serde_json::to_string(&AgentToHub::RpcResult { id, res })?;
        write
            .send(tungstenite::Message::Text(outbound.into()))
            .await?;
    }
    Ok(())
}

fn agent_ws_url(config: &HubConfig, mesh: &Mesh, node: &Node) -> Result<Url> {
    let mut url = Url::parse(&config.url)?;
    url.set_scheme(match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => bail!("unsupported hub URL scheme '{other}'"),
    })
    .map_err(|_| anyhow!("failed to set websocket scheme"))?;
    url.set_path("/v1/agent/connect");
    url.query_pairs_mut()
        .append_pair("mesh", &mesh.id)
        .append_pair("node", &node.id)
        .append_pair("slug", &node.slug);
    if let Some(token) = &config.token {
        url.query_pairs_mut().append_pair("token", token);
    }
    Ok(url)
}

fn normalize_http_url(raw: &str) -> Result<String> {
    let mut url = Url::parse(raw)?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("hub URL must start with http:// or https://");
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

fn decode_invite(token: &str) -> Result<InviteToken> {
    let raw = token
        .strip_prefix("via1.")
        .ok_or_else(|| anyhow!("invalid Via invite token"))?;
    let bytes = URL_SAFE_NO_PAD.decode(raw)?;
    let invite: InviteToken = serde_json::from_slice(&bytes)?;
    if invite.v != 1 {
        bail!("unsupported Via invite version");
    }
    Ok(invite)
}

fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

struct HubDb {
    _handle: ServerHandle,
    client: EmbeddedClient,
}

impl HubDb {
    async fn open(lux_dir: Option<String>) -> Result<Self> {
        let data_dir = lux_dir
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".via-hub/lux"));
        std::fs::create_dir_all(&data_dir)?;
        let handle = lux::run_with_config(ServerConfig {
            enable_resp: false,
            http_port: 0,
            data_dir: data_dir.to_string_lossy().to_string(),
            ..ServerConfig::default()
        })
        .await
        .context("failed to start hub Lux")?;
        let client = handle.client();
        Ok(Self {
            _handle: handle,
            client,
        })
    }

    async fn migrate(&self) -> Result<()> {
        for command in parse_migration(INIT_HUB)? {
            let name = command
                .first()
                .ok_or_else(|| anyhow!("empty migration command"))?;
            let args = command
                .iter()
                .skip(1)
                .map(String::as_str)
                .collect::<Vec<_>>();
            match self.client.execute(name, &args).await {
                Ok(_) => {}
                Err(error) if error.to_string().to_ascii_lowercase().contains("exists") => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.persist().await
    }

    async fn check_schema(&self) -> Result<()> {
        let mut missing = Vec::new();
        for table in TABLES {
            if self.client.execute("TSCHEMA", &[table]).await.is_err() {
                missing.push(*table);
            }
        }
        if !missing.is_empty() {
            bail!(
                "hub schema is missing tables: {}; run `via hub migrate` or start with `via hub start --migrate`",
                missing.join(", ")
            );
        }
        Ok(())
    }

    async fn persist(&self) -> Result<()> {
        self.client.execute("SAVE", &[]).await?;
        Ok(())
    }

    async fn insert_mesh(&self, id: &str, name: &str) -> Result<()> {
        let now = now_ts().to_string();
        self.insert_ignore(
            "meshes",
            &[
                ("id", id),
                ("name", name),
                ("created", &now),
                ("updated", &now),
            ],
        )
        .await
    }

    async fn insert_token(
        &self,
        mesh: &str,
        name: Option<&str>,
        token: &str,
        exp: i64,
    ) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let exp = exp.to_string();
        let created = now_ts().to_string();
        let hash = hash_token(token);
        self.insert_ignore(
            "tokens",
            &[
                ("id", &id),
                ("mesh", mesh),
                ("hash", &hash),
                ("name", name.unwrap_or("")),
                ("exp", &exp),
                ("used", "false"),
                ("created", &created),
            ],
        )
        .await
    }

    async fn insert_node(&self, query: &AgentQuery) -> Result<()> {
        let seen = now_ts().to_string();
        self.insert_ignore(
            "nodes",
            &[
                ("id", &query.node),
                ("mesh", &query.mesh),
                ("name", &query.slug),
                ("slug", &query.slug),
                ("os", std::env::consts::OS),
                ("arch", std::env::consts::ARCH),
                ("ver", env!("CARGO_PKG_VERSION")),
                ("seen", &seen),
                ("status", "online"),
            ],
        )
        .await
    }

    async fn insert_session(&self, id: &str, query: &AgentQuery) -> Result<()> {
        let now = now_ts().to_string();
        self.insert_ignore(
            "sessions",
            &[
                ("id", id),
                ("mesh", &query.mesh),
                ("node", &query.slug),
                ("started", &now),
                ("seen", &now),
                ("addr", ""),
            ],
        )
        .await
    }

    async fn insert_cmd(&self, id: &str, req: &PostCmdRequest) -> Result<()> {
        let now = now_ts().to_string();
        let exp = (now_ts() + 60).to_string();
        self.insert_ignore(
            "cmds",
            &[
                ("id", id),
                ("mesh", &req.mesh),
                ("src", &req.src),
                ("dst", &req.dst),
                ("state", "pending"),
                ("req", &req.req),
                ("res", ""),
                ("created", &now),
                ("updated", &now),
                ("exp", &exp),
            ],
        )
        .await
    }

    async fn finish_cmd(&self, response: &CmdResponse) -> Result<()> {
        let now = now_ts().to_string();
        let res = response.res.clone().unwrap_or_default();
        let state = response.state.as_str();
        match self
            .client
            .execute(
                "TUPDATE",
                &[
                    "cmds",
                    "SET",
                    "state",
                    state,
                    "res",
                    &res,
                    "updated",
                    &now,
                    "WHERE",
                    "id",
                    "=",
                    &response.id,
                ],
            )
            .await
        {
            Ok(_) => {
                self.persist().await?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn insert_ignore(&self, table: &str, fields: &[(&str, &str)]) -> Result<()> {
        let mut args = Vec::with_capacity(1 + fields.len() * 2);
        args.push(table);
        for (key, value) in fields {
            args.push(*key);
            args.push(*value);
        }
        match self.client.execute("TINSERT", &args).await {
            Ok(_) => {
                self.persist().await?;
                Ok(())
            }
            Err(error) if error.to_string().to_ascii_lowercase().contains("unique") => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn parse_migration(raw: &str) -> Result<Vec<Vec<String>>> {
    let mut commands = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("--") {
            continue;
        }
        commands.push(serde_json::from_str(line)?);
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::{
        call_node, decode_invite, hash_token, parse_migration, save_config, start, HubConfig,
        InviteToken, INIT_HUB, TABLES,
    };
    use crate::model::{Mesh, Node};
    use crate::paths::ViaPaths;
    use crate::rpc::{RpcRequest, RpcResponse};
    use crate::state::ViaState;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use tempfile::TempDir;
    use tokio::time::{sleep, Duration};

    #[test]
    fn hub_migration_uses_short_table_names() {
        let commands = parse_migration(INIT_HUB).unwrap();
        let names = commands
            .iter()
            .filter_map(|cmd| cmd.get(1).cloned())
            .collect::<Vec<_>>();
        for table in TABLES {
            assert!(names.iter().any(|name| name == table));
        }
        assert!(!names.iter().any(|name| name == "command_events"));
        assert!(!names.iter().any(|name| name == "agent_sessions"));
    }

    #[test]
    fn invite_tokens_round_trip() {
        let invite = InviteToken {
            v: 1,
            hub: "https://hub.example".to_string(),
            mesh: "mesh".to_string(),
            key: "key".to_string(),
            token: "secret".to_string(),
            name: Some("rig".to_string()),
            exp: 42,
        };
        let token = format!(
            "via1.{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&invite).unwrap())
        );
        let decoded = decode_invite(&token).unwrap();
        assert_eq!(decoded.hub, invite.hub);
        assert_eq!(decoded.mesh, invite.mesh);
    }

    #[test]
    fn token_hashes_do_not_expose_secret() {
        let hash = hash_token("super-secret");
        assert_ne!(hash, "super-secret");
        assert_eq!(hash, hash_token("super-secret"));
    }

    #[tokio::test]
    async fn hub_relay_exec_round_trips_without_plaintext_in_hub_store() {
        let source_temp = TempDir::new().unwrap();
        let target_temp = TempDir::new().unwrap();
        let hub_temp = TempDir::new().unwrap();
        let source_paths = temp_paths(&source_temp);
        let target_paths = temp_paths(&target_temp);
        source_paths.ensure().unwrap();
        target_paths.ensure().unwrap();

        let mesh = Mesh {
            id: "mesh-test".to_string(),
            created_at: 1,
        };
        let source = Node {
            id: "source-node".to_string(),
            slug: "laptop".to_string(),
            display_name: "laptop".to_string(),
            addresses: vec!["laptop".to_string()],
            daemon_addr: "127.0.0.1:1".to_string(),
            public: false,
            created_at: 1,
            last_seen_at: Some(1),
        };
        let target = Node {
            id: "target-node".to_string(),
            slug: "rig".to_string(),
            display_name: "rig".to_string(),
            addresses: vec!["rig".to_string()],
            daemon_addr: "127.0.0.1:1".to_string(),
            public: false,
            created_at: 1,
            last_seen_at: None,
        };

        crate::security::ensure_mesh_key(&source_paths).unwrap();
        let encoded_key = std::fs::read_to_string(&source_paths.mesh_key).unwrap();
        crate::security::install_mesh_key(&target_paths, &encoded_key).unwrap();

        let hub_addr = free_addr();
        let hub_url = format!("http://{hub_addr}");
        let config = HubConfig {
            url: hub_url.clone(),
            token: None,
        };
        save_config(&source_paths, &config).unwrap();
        save_config(&target_paths, &config).unwrap();

        let hub_lux = hub_temp.path().join("lux");
        let hub_task = tokio::spawn(start(
            hub_addr.clone(),
            Some(hub_lux.to_string_lossy().to_string()),
            true,
        ));
        wait_for_health(&hub_url).await;

        let mut source_state = ViaState::open(source_paths.clone()).await.unwrap();
        source_state.save_mesh(&mesh).await.unwrap();
        source_state.save_local_node_id(&source.id).await.unwrap();
        source_state.upsert_node(&source).await.unwrap();
        source_state.upsert_node(&target).await.unwrap();

        let mut target_state = ViaState::open(target_paths.clone()).await.unwrap();
        let mut target_local = target.clone();
        target_local.last_seen_at = Some(1);
        target_state.save_mesh(&mesh).await.unwrap();
        target_state
            .save_local_node_id(&target_local.id)
            .await
            .unwrap();
        target_state.upsert_node(&target_local).await.unwrap();
        target_state.shutdown().await.unwrap();

        let daemon_addr = free_addr();
        let daemon_task = tokio::spawn(crate::daemon::run(daemon_addr, target_paths));

        let mut last_error = None;
        let mut response = None;
        for _ in 0..50 {
            match call_node(
                &source_state,
                &target,
                RpcRequest::Exec {
                    command: vec![
                        "sh".to_string(),
                        "-lc".to_string(),
                        "printf via-hub-secret".to_string(),
                    ],
                },
            )
            .await
            {
                Ok(RpcResponse::Exec { output }) => {
                    response = Some(output);
                    break;
                }
                Ok(other) => {
                    last_error = Some(anyhow::anyhow!("unexpected response: {other:?}"));
                }
                Err(error) => {
                    last_error = Some(error);
                    sleep(Duration::from_millis(100)).await;
                }
            }
        }

        daemon_task.abort();
        hub_task.abort();
        source_state.shutdown().await.unwrap();

        assert_eq!(response.as_deref(), Some("via-hub-secret"));
        let hub_store = std::fs::read_to_string(hub_lux.join("lux.dat")).unwrap_or_default();
        assert!(
            !hub_store.contains("via-hub-secret"),
            "hub store leaked plaintext command/result"
        );
        assert!(
            last_error.is_none() || response.is_some(),
            "hub exec never succeeded: {last_error:?}"
        );
    }

    fn temp_paths(temp: &TempDir) -> ViaPaths {
        ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
        }
    }

    fn free_addr() -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        addr.to_string()
    }

    async fn wait_for_health(url: &str) {
        let health = format!("{url}/health");
        for _ in 0..50 {
            if reqwest::get(&health)
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return;
            }
            sleep(Duration::from_millis(50)).await;
        }
        panic!("hub did not become healthy");
    }
}
