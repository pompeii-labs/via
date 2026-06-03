use crate::model::{Mesh, Node};
use crate::paths::ViaPaths;
use crate::rpc::{RpcRequest, RpcResponse};
use crate::state::ViaState;
use crate::util::now_ts;
use anyhow::{anyhow, bail, Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::signature::{UnparsedPublicKey, ED25519};
use futures_util::{SinkExt, StreamExt};
use lux::{EmbeddedClient, EmbeddedValue, ServerConfig, ServerHandle};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::time::{interval, sleep, timeout, Duration, MissedTickBehavior};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite;
use url::Url;
use uuid::Uuid;

const INIT_HUB: &str = include_str!("../lux/migrations/20260602000000_init_hub.lux");
const ADMIN_TOKEN_ENV: &str = "VIA_HUB_ADMIN_TOKEN";
const ISSUER_PUBKEY_ENV: &str = "VIA_HUB_ISSUER_PUBKEY";
const CLOUD_INGEST_URL_ENV: &str = "VIA_HUB_CLOUD_INGEST_URL";
const CLOUD_INGEST_TOKEN_ENV: &str = "VIA_HUB_CLOUD_INGEST_TOKEN";
const HUB_URL_ENV: &str = "VIA_HUB_URL";
const CLOUD_API_URL_ENV: &str = "VIA_CLOUD_API_URL";
const API_KEY_ENV: &str = "VIA_API_KEY";
const CLOUD_API_KEY_ENV: &str = "VIA_CLOUD_API_KEY";
const HOSTED_HUB_URL_ENV: &str = "VIA_HOSTED_HUB_URL";
const GRANT_PREFIX: &str = "viahub1.";
const HOSTED_HUB_URL: &str = "https://hub.via.pompeiilabs.com";
const DEFAULT_CLOUD_API_URL: &str = "https://api.via.pompeiilabs.com";
const CLOUD_PROVISION_PATH: &str = "/api/hub/provision";
const USAGE_EVENT_BUFFER: usize = 1024;
const USAGE_EVENT_BATCH_SIZE: usize = 100;
const USAGE_EVENT_INTERVAL: Duration = Duration::from_secs(10);
const USAGE_EVENT_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const USAGE_EVENT_BACKOFF_MAX: Duration = Duration::from_secs(60);
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

#[derive(Debug, Serialize, Deserialize)]
struct RegisterNodeRequest {
    mesh: String,
    node: String,
    slug: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProvisionGrantRequest {
    grant: String,
    #[serde(alias = "node_id")]
    node: String,
    #[serde(alias = "node_slug")]
    slug: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SignedGrantJson {
    payload: String,
    signature: String,
}

#[derive(Debug, Serialize)]
struct CloudProvisionRequest {
    hub_url: String,
    mesh_id: String,
    mesh_name: String,
    node_id: String,
    node_slug: String,
}

#[derive(Debug, Deserialize)]
struct CloudProvisionResponse {
    #[serde(alias = "signed_grant")]
    grant: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct HubProvisionGrant {
    v: u8,
    aud: String,
    exp: i64,
    #[serde(alias = "nonce")]
    jti: String,
    #[serde(alias = "account_id")]
    account: String,
    #[serde(alias = "mesh_id")]
    mesh: String,
    #[serde(default, alias = "mesh_name")]
    name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JoinRequest {
    mesh: String,
    node: String,
    slug: String,
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AuthResponse {
    token: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct NodesQuery {
    mesh: String,
    node: String,
    token: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HubNode {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub status: String,
    pub seen: i64,
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
    admin_token: Option<String>,
    issuer_pubkey: Option<Vec<u8>>,
    hub_url: Option<String>,
    usage_reporter: Option<UsageReporter>,
    sessions: Mutex<HashMap<SessionKey, mpsc::Sender<HubToAgent>>>,
    pending: Mutex<HashMap<String, oneshot::Sender<String>>>,
    results: Mutex<HashMap<String, CmdResponse>>,
    seen_grants: Mutex<HashMap<String, i64>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct UsageEvent {
    account_id: String,
    mesh_id: String,
    node_id: String,
    event_type: String,
    timestamp: i64,
}

#[derive(Debug, Serialize, Deserialize)]
struct UsageEventBatch {
    events: Vec<UsageEvent>,
}

#[derive(Clone)]
struct UsageReporter {
    tx: mpsc::Sender<UsageEvent>,
}

impl UsageReporter {
    fn from_env() -> Option<Self> {
        let url = std::env::var(CLOUD_INGEST_URL_ENV)
            .ok()
            .filter(|value| !value.is_empty())?;
        let token = std::env::var(CLOUD_INGEST_TOKEN_ENV)
            .ok()
            .filter(|value| !value.is_empty())?;
        Some(spawn_usage_reporter(
            url,
            token,
            USAGE_EVENT_INTERVAL,
            USAGE_EVENT_BATCH_SIZE,
            USAGE_EVENT_BUFFER,
        ))
    }

    fn enqueue(&self, event: UsageEvent) {
        if let Err(error) = self.tx.try_send(event) {
            eprintln!("via hub usage event dropped: {error}");
        }
    }
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
    let url = normalize_hub_ref(&url)?;
    let mut token = None;
    if let Some(mesh) = state.mesh().await? {
        let local = state.local_node().await?;
        let client = reqwest::Client::new();
        let response = if is_hosted_hub_url(&url)? {
            provision_hosted_hub(&client, paths, &url, &mesh.id, &local.id, &local.slug).await?
        } else {
            provision_self_hosted_hub(&client, paths, &url, &mesh.id, &local.id, &local.slug).await?
        };
        token = Some(response.token);
    }
    save_config(
        paths,
        &HubConfig {
            url: url.clone(),
            token,
        },
    )?;
    println!("Via hub set to {url}.");
    Ok(())
}

async fn provision_hosted_hub(
    client: &reqwest::Client,
    paths: &ViaPaths,
    hub_url: &str,
    mesh_id: &str,
    node_id: &str,
    node_slug: &str,
) -> Result<AuthResponse> {
    let api_key = cloud_api_key(paths)?;
    let cloud_url = cloud_api_url()?;
    let grant = client
        .post(format!("{cloud_url}{CLOUD_PROVISION_PATH}"))
        .bearer_auth(api_key)
        .json(&CloudProvisionRequest {
            hub_url: hub_url.to_string(),
            mesh_id: mesh_id.to_string(),
            mesh_name: "default".to_string(),
            node_id: node_id.to_string(),
            node_slug: node_slug.to_string(),
        })
        .send()
        .await
        .context("failed to request hosted hub grant from Via cloud")?
        .error_for_status()
        .map_err(|error| cloud_provision_error(error))?
        .json::<CloudProvisionResponse>()
        .await
        .context("Via cloud returned an invalid hosted hub grant response")?
        .grant;

    client
        .post(format!("{hub_url}/v1/grants/provision"))
        .json(&ProvisionGrantRequest {
            grant,
            node: node_id.to_string(),
            slug: node_slug.to_string(),
        })
        .send()
        .await
        .context("failed to provision hosted hub with cloud grant")?
        .error_for_status()
        .map_err(|error| hosted_hub_grant_error(error))?
        .json::<AuthResponse>()
        .await
        .context("hosted hub returned an invalid provisioning response")
}

async fn provision_self_hosted_hub(
    client: &reqwest::Client,
    paths: &ViaPaths,
    hub_url: &str,
    mesh_id: &str,
    node_id: &str,
    node_slug: &str,
) -> Result<AuthResponse> {
    admin_request(client.post(format!("{hub_url}/v1/meshes")), paths)
        .json(&CreateMeshRequest {
            id: mesh_id.to_string(),
            name: "default".to_string(),
        })
        .send()
        .await?
        .error_for_status()?;
    admin_request(client.post(format!("{hub_url}/v1/nodes/register")), paths)
        .json(&RegisterNodeRequest {
            mesh: mesh_id.to_string(),
            node: node_id.to_string(),
            slug: node_slug.to_string(),
        })
        .send()
        .await?
        .error_for_status()?
        .json::<AuthResponse>()
        .await
        .context("self-hosted hub returned an invalid registration response")
}

pub async fn status(state: &ViaState) -> Result<()> {
    let Some(config) = load_config(state.paths())? else {
        println!("hub: none");
        return Ok(());
    };
    println!("hub: {}", config.url);
    let mesh = state
        .mesh()
        .await?
        .ok_or_else(|| anyhow!("run `via init` first"))?;
    println!("mesh: {}", mesh.id);
    let local = state.local_node().await?;
    println!("node: {}", local.slug);
    if config.token.is_none() {
        println!("auth: missing token");
        return Ok(());
    }
    match nodes(state).await {
        Ok(nodes) => {
            println!("auth: ok");
            let connected = nodes.iter().filter(|node| node.status == "online").count();
            let daemon = nodes
                .iter()
                .find(|node| node.slug == local.slug)
                .map(|node| node.status.as_str())
                .unwrap_or("unknown");
            println!("daemon: {daemon}");
            println!("nodes: {connected}/{} connected", nodes.len());
        }
        Err(error) => {
            println!("auth: failed");
            println!("error: {error}");
        }
    }
    Ok(())
}

pub async fn list(state: &ViaState) -> Result<()> {
    let Some(config) = load_config(state.paths())? else {
        println!("No hub configured.");
        return Ok(());
    };
    println!("{:<8} URL", "ACTIVE");
    println!("{:<8} {}", "yes", config.url);
    Ok(())
}

pub fn drop_hub(paths: &ViaPaths) -> Result<()> {
    if paths.hub_config.exists() {
        std::fs::remove_file(&paths.hub_config)
            .with_context(|| format!("failed to remove {}", paths.hub_config.display()))?;
        println!("Via hub disconnected for this node.");
    } else {
        println!("No hub configured.");
    }
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
    admin_request(client.post(format!("{}/v1/meshes", config.url)), paths)
        .json(&CreateMeshRequest {
            id: mesh.id.clone(),
            name: "default".to_string(),
        })
        .send()
        .await?
        .error_for_status()?;
    admin_request(client.post(format!("{}/v1/tokens", config.url)), paths)
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
    let node_name = name
        .or_else(|| invite.name.clone())
        .unwrap_or_else(|| local_hostname().unwrap_or_else(|_| "node".to_string()));
    let slug = crate::util::normalize_slug(&node_name)?;
    let mesh_id = invite.mesh.clone();
    let node_id = Uuid::new_v4().to_string();
    let response = reqwest::Client::new()
        .post(format!("{}/v1/join", invite.hub))
        .json(&JoinRequest {
            mesh: mesh_id.clone(),
            node: node_id.clone(),
            slug,
            token: invite.token,
        })
        .send()
        .await?
        .error_for_status()?
        .json::<AuthResponse>()
        .await?;
    crate::security::install_mesh_key(paths, &invite.key)?;
    save_config(
        paths,
        &HubConfig {
            url: invite.hub,
            token: Some(response.token),
        },
    )?;
    let mut state = ViaState::open(paths.clone()).await?;
    crate::commands::init(&mut state, Some(node_name), Some(mesh_id), Some(node_id)).await?;
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

pub async fn nodes(state: &ViaState) -> Result<Vec<HubNode>> {
    let config =
        load_config(state.paths())?.ok_or_else(|| anyhow!("run `via hub use <url>` first"))?;
    let token = config
        .token
        .ok_or_else(|| anyhow!("hub is configured but missing a node token"))?;
    let mesh = state
        .mesh()
        .await?
        .ok_or_else(|| anyhow!("run `via init` first"))?;
    let local = state.local_node().await?;
    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/v1/nodes", config.url))
        .query(&NodesQuery {
            mesh: mesh.id,
            node: local.slug,
            token,
        })
        .send()
        .await
        .context("hub node discovery request failed")?;
    let status = response.status();
    if !status.is_success() {
        bail!("hub node discovery failed with status {status}");
    }
    Ok(response.json::<Vec<HubNode>>().await?)
}

pub async fn node_by_slug(state: &ViaState, slug: &str) -> Result<Option<Node>> {
    Ok(nodes(state)
        .await?
        .into_iter()
        .find(|node| node.slug == slug)
        .map(|node| Node {
            id: node.id,
            slug: node.slug.clone(),
            display_name: node.name,
            addresses: Vec::new(),
            daemon_addr: String::new(),
            public: false,
            created_at: node.seen,
            last_seen_at: if node.status == "online" {
                Some(node.seen)
            } else {
                None
            },
        }))
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
    let issuer_pubkey = load_issuer_pubkey()?;
    let usage_reporter = issuer_pubkey
        .as_ref()
        .and_then(|_| UsageReporter::from_env());
    let runtime = Arc::new(HubRuntime {
        db,
        admin_token: std::env::var(ADMIN_TOKEN_ENV)
            .ok()
            .filter(|token| !token.is_empty()),
        issuer_pubkey,
        hub_url: std::env::var(HUB_URL_ENV)
            .ok()
            .filter(|url| !url.is_empty())
            .map(|url| normalize_http_url(&url))
            .transpose()?,
        usage_reporter,
        sessions: Mutex::new(HashMap::new()),
        pending: Mutex::new(HashMap::new()),
        results: Mutex::new(HashMap::new()),
        seen_grants: Mutex::new(HashMap::new()),
    });
    let app = Router::new()
        .route("/health", get(health))
        .route("/v1/agent/connect", get(agent_connect))
        .route("/v1/meshes", post(create_mesh))
        .route("/v1/tokens", post(create_token))
        .route("/v1/grants/provision", post(provision_with_grant))
        .route("/v1/join", post(join_mesh))
        .route("/v1/nodes", get(list_nodes))
        .route("/v1/nodes/register", post(register_node))
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
    headers: HeaderMap,
    Json(req): Json<CreateMeshRequest>,
) -> impl IntoResponse {
    if let Err(error) = require_admin(&runtime, &headers) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    match runtime.db.insert_mesh(&req.id, &req.name).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn create_token(
    State(runtime): State<Arc<HubRuntime>>,
    headers: HeaderMap,
    Json(req): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    if let Err(error) = require_admin(&runtime, &headers) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    match runtime
        .db
        .insert_invite_token(&req.mesh, req.name.as_deref(), &req.token, req.exp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn register_node(
    State(runtime): State<Arc<HubRuntime>>,
    headers: HeaderMap,
    Json(req): Json<RegisterNodeRequest>,
) -> impl IntoResponse {
    if let Err(error) = require_admin(&runtime, &headers) {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    match runtime
        .db
        .register_node(&req.mesh, &req.node, &req.slug)
        .await
    {
        Ok(token) => Json(AuthResponse { token }).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn provision_with_grant(
    State(runtime): State<Arc<HubRuntime>>,
    headers: HeaderMap,
    Json(req): Json<ProvisionGrantRequest>,
) -> impl IntoResponse {
    let grant = match verify_grant(&runtime, &headers, &req.grant).await {
        Ok(grant) => grant,
        Err(error) => return (StatusCode::UNAUTHORIZED, error.to_string()).into_response(),
    };
    match runtime
        .db
        .provision_grant_mesh(
            &grant.mesh,
            grant.name.as_deref(),
            &grant.account,
            &req.node,
            &req.slug,
        )
        .await
    {
        Ok(token) => Json(AuthResponse { token }).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn join_mesh(
    State(runtime): State<Arc<HubRuntime>>,
    Json(req): Json<JoinRequest>,
) -> impl IntoResponse {
    match runtime
        .db
        .claim_invite(&req.mesh, &req.node, &req.slug, &req.token)
        .await
    {
        Ok(token) => Json(AuthResponse { token }).into_response(),
        Err(error) => (StatusCode::UNAUTHORIZED, error.to_string()).into_response(),
    }
}

async fn list_nodes(
    State(runtime): State<Arc<HubRuntime>>,
    Query(query): Query<NodesQuery>,
) -> impl IntoResponse {
    if let Err(error) = runtime
        .db
        .validate_node_token(&query.mesh, &query.node, &query.token)
        .await
    {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    match runtime.db.nodes(&query.mesh).await {
        Ok(nodes) => Json(nodes).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

async fn agent_connect(
    ws: WebSocketUpgrade,
    State(runtime): State<Arc<HubRuntime>>,
    Query(query): Query<AgentQuery>,
) -> impl IntoResponse {
    let Some(token) = query.token.as_deref() else {
        return (StatusCode::UNAUTHORIZED, "missing hub token").into_response();
    };
    if let Err(error) = runtime
        .db
        .validate_node_token(&query.mesh, &query.slug, token)
        .await
    {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
    ws.on_upgrade(move |socket| handle_agent(socket, runtime, query))
        .into_response()
}

async fn post_cmd(
    State(runtime): State<Arc<HubRuntime>>,
    Json(req): Json<PostCmdRequest>,
) -> impl IntoResponse {
    let id = Uuid::new_v4().to_string();
    let Some(token) = req.token.as_deref() else {
        return (StatusCode::UNAUTHORIZED, "missing hub token").into_response();
    };
    if let Err(error) = runtime
        .db
        .validate_node_token(&req.mesh, &req.src, token)
        .await
    {
        return (StatusCode::UNAUTHORIZED, error.to_string()).into_response();
    }
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
    emit_cloud_usage_events(
        &runtime,
        &query.mesh,
        &query.node,
        &["connected", "session_started"],
    )
    .await;

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
        .remove(&(query.mesh.clone(), query.slug));
    let _ = runtime.db.mark_node_offline(&query.mesh, &query.node).await;
    emit_cloud_usage_events(
        &runtime,
        &query.mesh,
        &query.node,
        &["session_ended", "disconnected"],
    )
    .await;
}

async fn emit_cloud_usage_events(
    runtime: &HubRuntime,
    mesh: &str,
    node: &str,
    event_types: &[&str],
) {
    let Some(reporter) = runtime.usage_reporter.as_ref() else {
        return;
    };
    let account = match runtime.db.mesh_account(mesh).await {
        Ok(Some(account)) => account,
        Ok(None) => return,
        Err(error) => {
            eprintln!("via hub usage event skipped: {error}");
            return;
        }
    };
    let timestamp = now_ts();
    for event_type in event_types {
        reporter.enqueue(UsageEvent {
            account_id: account.clone(),
            mesh_id: mesh.to_string(),
            node_id: node.to_string(),
            event_type: (*event_type).to_string(),
            timestamp,
        });
    }
}

fn spawn_usage_reporter(
    url: String,
    token: String,
    flush_interval: Duration,
    batch_size: usize,
    buffer_size: usize,
) -> UsageReporter {
    let (tx, rx) = mpsc::channel(buffer_size);
    tokio::spawn(run_usage_reporter(
        reqwest::Client::new(),
        url,
        token,
        flush_interval,
        batch_size,
        buffer_size,
        rx,
    ));
    UsageReporter { tx }
}

async fn run_usage_reporter(
    client: reqwest::Client,
    url: String,
    token: String,
    flush_interval: Duration,
    batch_size: usize,
    max_pending: usize,
    mut rx: mpsc::Receiver<UsageEvent>,
) {
    let mut ticker = interval(flush_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pending = Vec::new();
    let mut backoff = USAGE_EVENT_BACKOFF_INITIAL;
    loop {
        tokio::select! {
            Some(event) = rx.recv() => {
                if pending.len() >= max_pending {
                    eprintln!("via hub usage retry buffer full; event dropped");
                } else {
                    pending.push(event);
                }
                while pending.len() < batch_size {
                    match rx.try_recv() {
                        Ok(event) if pending.len() < max_pending => pending.push(event),
                        Ok(_) => eprintln!("via hub usage retry buffer full; event dropped"),
                        Err(_) => break,
                    }
                }
                if pending.len() >= batch_size && post_usage_batch(&client, &url, &token, &pending).await.is_ok() {
                    pending.clear();
                    backoff = USAGE_EVENT_BACKOFF_INITIAL;
                }
            }
            _ = ticker.tick() => {
                if pending.is_empty() {
                    continue;
                }
                match post_usage_batch(&client, &url, &token, &pending).await {
                    Ok(()) => {
                        pending.clear();
                        backoff = USAGE_EVENT_BACKOFF_INITIAL;
                    }
                    Err(error) => {
                        eprintln!("via hub usage ingest failed: {error}");
                        sleep(backoff).await;
                        backoff = (backoff * 2).min(USAGE_EVENT_BACKOFF_MAX);
                    }
                }
            }
            else => break,
        }
    }
}

async fn post_usage_batch(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    events: &[UsageEvent],
) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    client
        .post(url)
        .bearer_auth(token)
        .json(&UsageEventBatch {
            events: events.to_vec(),
        })
        .send()
        .await?
        .error_for_status()?;
    Ok(())
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

fn hosted_hub_url() -> Result<String> {
    match std::env::var(HOSTED_HUB_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => normalize_http_url(url.trim()),
        _ => Ok(HOSTED_HUB_URL.to_string()),
    }
}

fn is_hosted_hub_url(url: &str) -> Result<bool> {
    Ok(normalize_http_url(url)? == hosted_hub_url()?)
}

fn cloud_api_url() -> Result<String> {
    match std::env::var(CLOUD_API_URL_ENV) {
        Ok(url) if !url.trim().is_empty() => normalize_http_url(url.trim()),
        _ => Ok(DEFAULT_CLOUD_API_URL.to_string()),
    }
}

fn cloud_api_key(paths: &ViaPaths) -> Result<String> {
    crate::auth::resolve_api_key(paths)?
        .or_else(|| {
            std::env::var(CLOUD_API_KEY_ENV)
                .ok()
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
        })
        .ok_or_else(|| {
            anyhow!(
                "missing Via API key for hosted hub; run `via auth init` or set {API_KEY_ENV} before running `via hub use hosted`"
            )
        })
}

fn cloud_provision_error(error: reqwest::Error) -> anyhow::Error {
    match error.status() {
        Some(reqwest::StatusCode::UNAUTHORIZED) | Some(reqwest::StatusCode::FORBIDDEN) => {
            anyhow!("Via cloud rejected the API key; check {API_KEY_ENV} and try again")
        }
        _ => anyhow!(error).context("Via cloud rejected the hosted hub grant request"),
    }
}

fn hosted_hub_grant_error(error: reqwest::Error) -> anyhow::Error {
    match error.status() {
        Some(reqwest::StatusCode::UNAUTHORIZED) | Some(reqwest::StatusCode::FORBIDDEN) => {
            anyhow!("hosted hub rejected the cloud grant; it may have expired, retry `via hub use hosted`")
        }
        _ => anyhow!(error).context("hosted hub rejected the cloud grant"),
    }
}

fn normalize_hub_ref(raw: &str) -> Result<String> {
    match raw {
        "hosted" | "via" | "default" => hosted_hub_url(),
        _ => normalize_http_url(raw),
    }
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

fn local_hostname() -> Result<String> {
    let output = std::process::Command::new("hostname")
        .output()
        .context("failed to read hostname")?;
    if !output.status.success() {
        bail!("hostname command failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn admin_request(builder: reqwest::RequestBuilder, paths: &ViaPaths) -> reqwest::RequestBuilder {
    match std::env::var(ADMIN_TOKEN_ENV) {
        Ok(token) if !token.is_empty() => builder.bearer_auth(token),
        _ => match crate::auth::resolve_api_key(paths) {
            Ok(Some(api_key)) => builder.bearer_auth(api_key),
            _ => builder,
        },
    }
}

fn load_issuer_pubkey() -> Result<Option<Vec<u8>>> {
    let Some(raw) = std::env::var(ISSUER_PUBKEY_ENV)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let bytes = decode_base64(&raw).context("invalid issuer public key")?;
    if bytes.len() != 32 {
        bail!("issuer public key must be 32 bytes");
    }
    Ok(Some(bytes))
}

async fn verify_grant(
    runtime: &HubRuntime,
    headers: &HeaderMap,
    grant: &str,
) -> Result<HubProvisionGrant> {
    let issuer_pubkey = runtime
        .issuer_pubkey
        .as_ref()
        .ok_or_else(|| anyhow!("grant provisioning is not configured"))?;
    let (payload, signature) = decode_signed_grant(grant)?;
    UnparsedPublicKey::new(&ED25519, issuer_pubkey)
        .verify(&payload, &signature)
        .map_err(|_| anyhow!("invalid grant signature"))?;
    let claims: HubProvisionGrant = serde_json::from_slice(&payload)?;
    if claims.v != 1 {
        bail!("unsupported grant version");
    }
    if claims.exp < now_ts() {
        bail!("grant expired");
    }
    if claims.jti.trim().is_empty() {
        bail!("grant missing jti");
    }
    if claims.account.trim().is_empty() {
        bail!("grant missing account");
    }
    if claims.mesh.trim().is_empty() {
        bail!("grant missing mesh");
    }
    let expected_audience = grant_audience(runtime, headers)?;
    if normalize_http_url(&claims.aud)? != expected_audience {
        bail!("grant audience mismatch");
    }
    let mut seen = runtime.seen_grants.lock().await;
    let now = now_ts();
    seen.retain(|_, exp| *exp >= now);
    if seen.contains_key(&claims.jti) {
        bail!("grant replayed");
    }
    seen.insert(claims.jti.clone(), claims.exp);
    Ok(claims)
}

fn decode_signed_grant(grant: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    if let Some(raw) = grant.strip_prefix(GRANT_PREFIX) {
        let mut parts = raw.split('.');
        let payload = parts
            .next()
            .ok_or_else(|| anyhow!("grant missing payload"))?;
        let signature = parts
            .next()
            .ok_or_else(|| anyhow!("grant missing signature"))?;
        if parts.next().is_some() {
            bail!("invalid grant format");
        }
        return signed_grant_parts(payload, signature);
    }
    if grant.contains('.') {
        let mut parts = grant.split('.');
        let payload = parts
            .next()
            .ok_or_else(|| anyhow!("grant missing payload"))?;
        let signature = parts
            .next()
            .ok_or_else(|| anyhow!("grant missing signature"))?;
        if parts.next().is_some() {
            bail!("invalid grant format");
        }
        return signed_grant_parts(payload, signature);
    }
    let grant: SignedGrantJson = serde_json::from_str(grant)?;
    signed_grant_parts(&grant.payload, &grant.signature)
}

fn signed_grant_parts(payload: &str, signature: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let payload = URL_SAFE_NO_PAD.decode(payload)?;
    let signature = decode_base64(signature)?;
    if signature.len() != 64 {
        bail!("grant signature must be 64 bytes");
    }
    Ok((payload, signature))
}

fn decode_base64(raw: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(raw)
        .or_else(|_| URL_SAFE.decode(raw))
        .or_else(|_| STANDARD.decode(raw))
        .map_err(|error| error.into())
}

fn grant_audience(runtime: &HubRuntime, headers: &HeaderMap) -> Result<String> {
    if let Some(url) = runtime.hub_url.as_deref() {
        return Ok(url.to_string());
    }
    let host = headers
        .get(axum::http::header::HOST)
        .ok_or_else(|| anyhow!("missing host header"))?
        .to_str()
        .context("invalid host header")?;
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http")
        .split(',')
        .next()
        .unwrap_or("http")
        .trim();
    normalize_http_url(&format!("{scheme}://{host}"))
}

fn require_admin(runtime: &HubRuntime, headers: &HeaderMap) -> Result<()> {
    let Some(expected) = runtime.admin_token.as_deref() else {
        return Ok(());
    };
    let Some(header) = headers.get(axum::http::header::AUTHORIZATION) else {
        bail!("missing admin token");
    };
    let raw = header.to_str().context("invalid authorization header")?;
    let Some(actual) = raw.strip_prefix("Bearer ") else {
        bail!("invalid admin token");
    };
    if actual != expected {
        bail!("invalid admin token");
    }
    Ok(())
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

    async fn insert_invite_token(
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
                ("kind", "invite"),
                ("node", ""),
                ("name", name.unwrap_or("")),
                ("exp", &exp),
                ("used", "false"),
                ("created", &created),
            ],
        )
        .await
    }

    async fn register_node(&self, mesh: &str, node: &str, slug: &str) -> Result<String> {
        self.insert_node_record(mesh, node, slug).await?;
        self.create_node_token(mesh, slug).await
    }

    async fn provision_grant_mesh(
        &self,
        mesh: &str,
        name: Option<&str>,
        account: &str,
        node: &str,
        slug: &str,
    ) -> Result<String> {
        self.insert_mesh(mesh, name.unwrap_or("default")).await?;
        self.record_mesh_account(mesh, account).await?;
        self.register_node(mesh, node, slug).await
    }

    async fn record_mesh_account(&self, mesh: &str, account: &str) -> Result<()> {
        self.client
            .execute(
                "TUPDATE",
                &["meshes", "SET", "account", account, "WHERE", "id", "=", mesh],
            )
            .await?;
        self.persist().await
    }

    async fn mesh_account(&self, mesh: &str) -> Result<Option<String>> {
        let row = self
            .select_rows("meshes", &["WHERE", "id", "=", mesh, "LIMIT", "1"])
            .await?
            .into_iter()
            .next();
        Ok(row.and_then(|row| {
            field(&row, "account")
                .filter(|account| !account.is_empty())
                .map(str::to_string)
        }))
    }

    async fn claim_invite(
        &self,
        mesh: &str,
        node: &str,
        slug: &str,
        token: &str,
    ) -> Result<String> {
        let hash = hash_token(token);
        let row = self
            .token_by_hash(&hash)
            .await?
            .ok_or_else(|| anyhow!("invalid invite token"))?;
        require_field(&row, "mesh", mesh)?;
        require_field(&row, "kind", "invite")?;
        if field(&row, "used").is_some_and(|used| used == "true") {
            bail!("invite token has already been used");
        }
        let exp = field(&row, "exp")
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        if exp > 0 && exp < now_ts() {
            bail!("invite token expired");
        }
        self.client
            .execute(
                "TUPDATE",
                &[
                    "tokens", "SET", "used", "true", "node", slug, "WHERE", "hash", "=", &hash,
                ],
            )
            .await?;
        self.insert_node_record(mesh, node, slug).await?;
        self.create_node_token(mesh, slug).await
    }

    async fn validate_node_token(&self, mesh: &str, node: &str, token: &str) -> Result<()> {
        let row = self
            .token_by_hash(&hash_token(token))
            .await?
            .ok_or_else(|| anyhow!("invalid hub token"))?;
        require_field(&row, "mesh", mesh)?;
        require_field(&row, "kind", "node")?;
        require_field(&row, "node", node)?;
        let exp = field(&row, "exp")
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(0);
        if exp > 0 && exp < now_ts() {
            bail!("hub token expired");
        }
        Ok(())
    }

    async fn create_node_token(&self, mesh: &str, slug: &str) -> Result<String> {
        let token = crate::security::nonce()?;
        let id = Uuid::new_v4().to_string();
        let created = now_ts().to_string();
        let hash = hash_token(&token);
        self.insert_ignore(
            "tokens",
            &[
                ("id", &id),
                ("mesh", mesh),
                ("hash", &hash),
                ("kind", "node"),
                ("node", slug),
                ("name", slug),
                ("exp", "0"),
                ("used", "false"),
                ("created", &created),
            ],
        )
        .await?;
        Ok(token)
    }

    async fn token_by_hash(&self, hash: &str) -> Result<Option<HashMap<String, String>>> {
        Ok(self
            .select_rows("tokens", &["WHERE", "hash", "=", hash, "LIMIT", "1"])
            .await?
            .into_iter()
            .next())
    }

    async fn insert_node(&self, query: &AgentQuery) -> Result<()> {
        self.insert_node_record(&query.mesh, &query.node, &query.slug)
            .await
    }

    async fn insert_node_record(&self, mesh: &str, node: &str, slug: &str) -> Result<()> {
        let seen = now_ts().to_string();
        self.insert_ignore(
            "nodes",
            &[
                ("id", node),
                ("mesh", mesh),
                ("name", slug),
                ("slug", slug),
                ("os", std::env::consts::OS),
                ("arch", std::env::consts::ARCH),
                ("ver", env!("CARGO_PKG_VERSION")),
                ("seen", &seen),
                ("status", "online"),
            ],
        )
        .await?;
        self.client
            .execute(
                "TUPDATE",
                &[
                    "nodes", "SET", "seen", &seen, "status", "online", "WHERE", "id", "=",
                    node,
                ],
            )
            .await?;
        self.persist().await
    }

    async fn mark_node_offline(&self, _mesh: &str, node: &str) -> Result<()> {
        let seen = now_ts().to_string();
        self.client
            .execute(
                "TUPDATE",
                &[
                    "nodes", "SET", "seen", &seen, "status", "offline", "WHERE", "id", "=",
                    node,
                ],
            )
            .await?;
        self.persist().await
    }

    async fn nodes(&self, mesh: &str) -> Result<Vec<HubNode>> {
        let rows = self
            .select_rows("nodes", &["WHERE", "mesh", "=", mesh])
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| HubNode {
                id: field(&row, "id").unwrap_or_default().to_string(),
                slug: field(&row, "slug").unwrap_or_default().to_string(),
                name: field(&row, "name").unwrap_or_default().to_string(),
                status: field(&row, "status").unwrap_or("unknown").to_string(),
                seen: field(&row, "seen")
                    .and_then(|value| value.parse::<i64>().ok())
                    .unwrap_or(0),
            })
            .filter(|node| !node.slug.is_empty())
            .collect())
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

    async fn select_rows(
        &self,
        table: &str,
        suffix: &[&str],
    ) -> Result<Vec<HashMap<String, String>>> {
        let mut args = Vec::with_capacity(3 + suffix.len());
        args.extend(["*", "FROM", table]);
        args.extend(suffix.iter().copied());
        match self.client.execute_value("TSELECT", &args).await? {
            EmbeddedValue::Array(rows) => rows.into_iter().map(row_to_map).collect(),
            other => Err(anyhow!("unexpected Lux table response: {other:?}")),
        }
    }
}

fn row_to_map(value: EmbeddedValue) -> Result<HashMap<String, String>> {
    let EmbeddedValue::Array(values) = value else {
        bail!("unexpected Lux table row: {value:?}");
    };
    let mut row = HashMap::new();
    let mut iter = values.into_iter();
    while let (Some(key), Some(value)) = (iter.next(), iter.next()) {
        row.insert(embedded_to_string(key)?, embedded_to_string(value)?);
    }
    Ok(row)
}

fn embedded_to_string(value: EmbeddedValue) -> Result<String> {
    match value {
        EmbeddedValue::Simple(value) => Ok(value),
        EmbeddedValue::Bulk(value) => Ok(std::str::from_utf8(&value)?.to_string()),
        EmbeddedValue::Int(value) => Ok(value.to_string()),
        other => Err(anyhow!("unexpected Lux table field value: {other:?}")),
    }
}

fn field<'a>(row: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    row.get(key).map(String::as_str)
}

fn require_field(row: &HashMap<String, String>, key: &str, expected: &str) -> Result<()> {
    match field(row, key) {
        Some(actual) if actual == expected => Ok(()),
        Some(_) | None => bail!("invalid hub token"),
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
        call_node, decode_invite, hash_token, normalize_hub_ref, parse_migration, save_config,
        emit_cloud_usage_events, post_usage_batch, spawn_usage_reporter, start, use_hub,
        AuthResponse,
        CreateMeshRequest, CreateTokenRequest, HubConfig, HubDb, HubRuntime, InviteToken,
        JoinRequest, PostCmdRequest, RegisterNodeRequest, UsageEvent, UsageEventBatch,
        ADMIN_TOKEN_ENV, API_KEY_ENV, CLOUD_API_KEY_ENV, CLOUD_API_URL_ENV,
        CLOUD_INGEST_TOKEN_ENV, CLOUD_INGEST_URL_ENV, GRANT_PREFIX, HOSTED_HUB_URL,
        HOSTED_HUB_URL_ENV, HUB_URL_ENV, INIT_HUB, ISSUER_PUBKEY_ENV, TABLES,
    };
    use crate::model::{Mesh, Node};
    use crate::paths::ViaPaths;
    use crate::rpc::{RpcRequest, RpcResponse};
    use crate::state::ViaState;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::OnceLock;
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex as TokioMutex;
    use tokio::time::{sleep, timeout, Duration};

    static ENV_LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();

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

    #[test]
    fn hosted_hub_aliases_resolve_to_hosted_url() {
        assert_eq!(normalize_hub_ref("hosted").unwrap(), HOSTED_HUB_URL);
        assert_eq!(normalize_hub_ref("via").unwrap(), HOSTED_HUB_URL);
        assert_eq!(normalize_hub_ref("default").unwrap(), HOSTED_HUB_URL);
        assert_eq!(
            normalize_hub_ref("https://hub.example.com").unwrap(),
            "https://hub.example.com"
        );
    }

    #[tokio::test]
    async fn hub_relay_exec_round_trips_without_plaintext_in_hub_store() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
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
        let hub_lux = hub_temp.path().join("lux");
        let hub_task = tokio::spawn(start(
            hub_addr.clone(),
            Some(hub_lux.to_string_lossy().to_string()),
            true,
        ));
        wait_for_health(&hub_url).await;

        let client = reqwest::Client::new();
        client
            .post(format!("{hub_url}/v1/meshes"))
            .json(&CreateMeshRequest {
                id: mesh.id.clone(),
                name: "default".to_string(),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let source_token = register_test_node(&client, &hub_url, &mesh.id, &source).await;
        let target_token = register_test_node(&client, &hub_url, &mesh.id, &target).await;
        save_config(
            &source_paths,
            &HubConfig {
                url: hub_url.clone(),
                token: Some(source_token),
            },
        )
        .unwrap();
        save_config(
            &target_paths,
            &HubConfig {
                url: hub_url.clone(),
                token: Some(target_token),
            },
        )
        .unwrap();

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

    #[tokio::test]
    async fn hub_rejects_commands_without_valid_node_token() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub_temp = TempDir::new().unwrap();
        let hub_addr = free_addr();
        let hub_url = format!("http://{hub_addr}");
        let hub_lux = hub_temp.path().join("lux");
        let hub_task = tokio::spawn(start(
            hub_addr,
            Some(hub_lux.to_string_lossy().to_string()),
            true,
        ));
        wait_for_health(&hub_url).await;

        let client = reqwest::Client::new();
        client
            .post(format!("{hub_url}/v1/meshes"))
            .json(&CreateMeshRequest {
                id: "mesh-auth".to_string(),
                name: "default".to_string(),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let response = client
            .post(format!("{hub_url}/v1/cmds"))
            .json(&PostCmdRequest {
                mesh: "mesh-auth".to_string(),
                src: "laptop".to_string(),
                dst: "rig".to_string(),
                req: "ciphertext".to_string(),
                token: None,
            })
            .send()
            .await
            .unwrap();

        hub_task.abort();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn invite_tokens_are_single_use() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub_temp = TempDir::new().unwrap();
        let hub_addr = free_addr();
        let hub_url = format!("http://{hub_addr}");
        let hub_lux = hub_temp.path().join("lux");
        let hub_task = tokio::spawn(start(
            hub_addr,
            Some(hub_lux.to_string_lossy().to_string()),
            true,
        ));
        wait_for_health(&hub_url).await;

        let client = reqwest::Client::new();
        client
            .post(format!("{hub_url}/v1/meshes"))
            .json(&CreateMeshRequest {
                id: "mesh-join".to_string(),
                name: "default".to_string(),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        client
            .post(format!("{hub_url}/v1/tokens"))
            .json(&CreateTokenRequest {
                mesh: "mesh-join".to_string(),
                name: Some("rig".to_string()),
                token: "invite-secret".to_string(),
                exp: crate::util::now_ts() + 60,
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();

        let request = JoinRequest {
            mesh: "mesh-join".to_string(),
            node: "node-1".to_string(),
            slug: "rig".to_string(),
            token: "invite-secret".to_string(),
        };
        client
            .post(format!("{hub_url}/v1/join"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let second = client
            .post(format!("{hub_url}/v1/join"))
            .json(&request)
            .send()
            .await
            .unwrap();

        hub_task.abort();
        assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_grant_provisions_mesh_and_node_token() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub = GrantHub::start().await;
        let client = reqwest::Client::new();
        let grant = signed_grant(
            &hub.key_pair,
            &hub.url,
            crate::util::now_ts() + 60,
            "jti-valid",
            "mesh-grant",
        );

        let auth = client
            .post(format!("{}/v1/grants/provision", hub.url))
            .json(&serde_json::json!({
                "grant": grant,
                "node": "node-1",
                "slug": "rig"
            }))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<AuthResponse>()
            .await
            .unwrap();
        let nodes = client
            .get(format!("{}/v1/nodes", hub.url))
            .query(&[
                ("mesh", "mesh-grant"),
                ("node", "rig"),
                ("token", auth.token.as_str()),
            ])
            .send()
            .await
            .unwrap();

        hub.task.abort();
        clear_hub_env();
        assert_eq!(nodes.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn hosted_hub_use_requests_cloud_grant_and_stores_node_token() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub = GrantHub::start().await;
        std::env::set_var(HOSTED_HUB_URL_ENV, &hub.url);
        std::env::set_var(API_KEY_ENV, "via_test_key");
        let cloud = start_cloud_provision_server(hub.url.clone(), hub.key_pair, "via_test_key")
            .await;
        std::env::set_var(CLOUD_API_URL_ENV, &cloud.url);
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        let mut state = ViaState::open(paths.clone()).await.unwrap();
        let mesh = Mesh {
            id: "mesh-hosted-cli".to_string(),
            created_at: crate::util::now_ts(),
        };
        let node = Node {
            id: "node-hosted-cli".to_string(),
            slug: "rig".to_string(),
            display_name: "rig".to_string(),
            addresses: Vec::new(),
            daemon_addr: "127.0.0.1:47819".to_string(),
            public: false,
            created_at: crate::util::now_ts(),
            last_seen_at: None,
        };
        state.save_mesh(&mesh).await.unwrap();
        state.upsert_node(&node).await.unwrap();
        state.save_local_node_id(&node.id).await.unwrap();

        use_hub(&state, &paths, "hosted".to_string()).await.unwrap();
        let config = super::load_config(&paths).unwrap().unwrap();
        let nodes = reqwest::Client::new()
            .get(format!("{}/v1/nodes", hub.url))
            .query(&[
                ("mesh", mesh.id.as_str()),
                ("node", node.slug.as_str()),
                ("token", config.token.as_deref().unwrap()),
            ])
            .send()
            .await
            .unwrap();

        state.shutdown().await.unwrap();
        hub.task.abort();
        clear_hub_env();
        assert_eq!(config.url, hub.url);
        assert!(config.token.is_some());
        assert_eq!(nodes.status(), reqwest::StatusCode::OK);
        let requests = cloud.requests.lock().await.clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].hub_url, hub.url);
        assert_eq!(requests[0].mesh_id, mesh.id);
        assert_eq!(requests[0].node_id, node.id);
        assert_eq!(requests[0].node_slug, node.slug);
    }

    #[tokio::test]
    async fn self_hosted_hub_use_stays_admin_token_only() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub_temp = TempDir::new().unwrap();
        let hub_addr = free_addr();
        let hub_url = format!("http://{hub_addr}");
        let hub_lux = hub_temp.path().join("lux");
        std::env::set_var(ADMIN_TOKEN_ENV, "admin-secret");
        let hub_task = tokio::spawn(start(
            hub_addr,
            Some(hub_lux.to_string_lossy().to_string()),
            true,
        ));
        wait_for_health(&hub_url).await;
        let cloud = start_unexpected_cloud_server().await;
        std::env::set_var(CLOUD_API_URL_ENV, &cloud.url);
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        let mut state = ViaState::open(paths.clone()).await.unwrap();
        let mesh = Mesh {
            id: "mesh-self-hosted-cli".to_string(),
            created_at: crate::util::now_ts(),
        };
        let node = Node {
            id: "node-self-hosted-cli".to_string(),
            slug: "rig".to_string(),
            display_name: "rig".to_string(),
            addresses: Vec::new(),
            daemon_addr: "127.0.0.1:47819".to_string(),
            public: false,
            created_at: crate::util::now_ts(),
            last_seen_at: None,
        };
        state.save_mesh(&mesh).await.unwrap();
        state.upsert_node(&node).await.unwrap();
        state.save_local_node_id(&node.id).await.unwrap();

        use_hub(&state, &paths, hub_url.clone()).await.unwrap();
        let config = super::load_config(&paths).unwrap().unwrap();

        state.shutdown().await.unwrap();
        hub_task.abort();
        clear_hub_env();
        assert_eq!(config.url, hub_url);
        assert!(config.token.is_some());
        assert_eq!(*cloud.hits.lock().await, 0);
    }

    #[tokio::test]
    async fn expired_grant_is_rejected() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub = GrantHub::start().await;
        let grant = signed_grant(
            &hub.key_pair,
            &hub.url,
            crate::util::now_ts() - 1,
            "jti-expired",
            "mesh-expired",
        );
        let response = post_grant(&hub.url, grant).await;

        hub.task.abort();
        clear_hub_env();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn bad_signature_grant_is_rejected() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub = GrantHub::start().await;
        let other_key = test_key_pair(8);
        let grant = signed_grant(
            &other_key,
            &hub.url,
            crate::util::now_ts() + 60,
            "jti-bad-sig",
            "mesh-bad-sig",
        );
        let response = post_grant(&hub.url, grant).await;

        hub.task.abort();
        clear_hub_env();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_audience_grant_is_rejected() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub = GrantHub::start().await;
        let grant = signed_grant(
            &hub.key_pair,
            "https://wrong-hub.example",
            crate::util::now_ts() + 60,
            "jti-wrong-aud",
            "mesh-wrong-aud",
        );
        let response = post_grant(&hub.url, grant).await;

        hub.task.abort();
        clear_hub_env();
        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn replayed_grant_jti_is_rejected() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let hub = GrantHub::start().await;
        let grant = signed_grant(
            &hub.key_pair,
            &hub.url,
            crate::util::now_ts() + 60,
            "jti-replay",
            "mesh-replay",
        );
        let first = post_grant(&hub.url, grant.clone()).await;
        let second = post_grant(&hub.url, grant).await;

        hub.task.abort();
        clear_hub_env();
        assert_eq!(first.status(), reqwest::StatusCode::OK);
        assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn no_issuer_pubkey_preserves_admin_token_flow() {
        let _guard = env_lock().lock().await;
        let hub_temp = TempDir::new().unwrap();
        let hub_addr = free_addr();
        let hub_url = format!("http://{hub_addr}");
        let hub_lux = hub_temp.path().join("lux");
        clear_hub_env();
        std::env::set_var(ADMIN_TOKEN_ENV, "admin-secret");
        let hub_task = tokio::spawn(start(
            hub_addr,
            Some(hub_lux.to_string_lossy().to_string()),
            true,
        ));
        wait_for_health(&hub_url).await;

        let client = reqwest::Client::new();
        let unauth = client
            .post(format!("{hub_url}/v1/meshes"))
            .json(&CreateMeshRequest {
                id: "mesh-admin".to_string(),
                name: "default".to_string(),
            })
            .send()
            .await
            .unwrap();
        let auth = client
            .post(format!("{hub_url}/v1/meshes"))
            .bearer_auth("admin-secret")
            .json(&CreateMeshRequest {
                id: "mesh-admin".to_string(),
                name: "default".to_string(),
            })
            .send()
            .await
            .unwrap();

        hub_task.abort();
        clear_hub_env();
        assert_eq!(unauth.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(auth.status(), reqwest::StatusCode::NO_CONTENT);
    }


    #[tokio::test]
    async fn usage_events_batch_to_cloud_ingest() {
        let batches = Arc::new(TokioMutex::new(Vec::<Vec<UsageEvent>>::new()));
        let ingest = start_ingest_server(batches.clone(), axum::http::StatusCode::NO_CONTENT).await;
        let reporter = spawn_usage_reporter(
            ingest.url,
            "ingest-secret".to_string(),
            Duration::from_millis(25),
            2,
            8,
        );

        reporter.enqueue(test_usage_event("connected"));
        reporter.enqueue(test_usage_event("session_started"));
        reporter.enqueue(test_usage_event("disconnected"));

        let recorded = wait_for_batches(&batches, 2).await;
        assert_eq!(recorded.iter().map(Vec::len).sum::<usize>(), 3);
        assert!(recorded.iter().any(|batch| batch.len() == 2));
        assert!(recorded.iter().any(|batch| batch.len() == 1));
    }

    #[tokio::test]
    async fn cloud_mesh_usage_events_are_emitted() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let batches = Arc::new(TokioMutex::new(Vec::<Vec<UsageEvent>>::new()));
        let ingest = start_ingest_server(batches.clone(), axum::http::StatusCode::NO_CONTENT).await;
        let temp = TempDir::new().unwrap();
        let db = HubDb::open(Some(temp.path().join("lux").to_string_lossy().to_string()))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        db.provision_grant_mesh("mesh-cloud", Some("default"), "acct_123", "node-1", "rig")
            .await
            .unwrap();
        let runtime = HubRuntime {
            db,
            admin_token: None,
            issuer_pubkey: None,
            hub_url: None,
            usage_reporter: Some(spawn_usage_reporter(
                ingest.url,
                "ingest-secret".to_string(),
                Duration::from_millis(25),
                10,
                8,
            )),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            results: tokio::sync::Mutex::new(HashMap::new()),
            seen_grants: tokio::sync::Mutex::new(HashMap::new()),
        };

        emit_cloud_usage_events(
            &runtime,
            "mesh-cloud",
            "node-1",
            &["connected", "session_started", "session_ended", "disconnected"],
        )
        .await;

        let recorded = wait_for_batches(&batches, 1).await;
        let events = recorded.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(events.len(), 4);
        assert!(events.iter().all(|event| event.account_id == "acct_123"));
        assert!(events.iter().all(|event| event.mesh_id == "mesh-cloud"));
        assert!(events.iter().all(|event| event.node_id == "node-1"));
        assert!(events.iter().any(|event| event.event_type == "connected"));
        assert!(events.iter().any(|event| event.event_type == "session_started"));
        assert!(events.iter().any(|event| event.event_type == "session_ended"));
        assert!(events.iter().any(|event| event.event_type == "disconnected"));
    }

    #[tokio::test]
    async fn non_cloud_mesh_usage_events_are_not_emitted() {
        let _guard = env_lock().lock().await;
        clear_hub_env();
        let batches = Arc::new(TokioMutex::new(Vec::<Vec<UsageEvent>>::new()));
        let ingest = start_ingest_server(batches.clone(), axum::http::StatusCode::NO_CONTENT).await;
        let temp = TempDir::new().unwrap();
        let db = HubDb::open(Some(temp.path().join("lux").to_string_lossy().to_string()))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        db.insert_mesh("mesh-oss", "default").await.unwrap();
        db.register_node("mesh-oss", "node-1", "rig").await.unwrap();
        let runtime = HubRuntime {
            db,
            admin_token: None,
            issuer_pubkey: None,
            hub_url: None,
            usage_reporter: Some(spawn_usage_reporter(
                ingest.url,
                "ingest-secret".to_string(),
                Duration::from_millis(25),
                10,
                8,
            )),
            sessions: tokio::sync::Mutex::new(HashMap::new()),
            pending: tokio::sync::Mutex::new(HashMap::new()),
            results: tokio::sync::Mutex::new(HashMap::new()),
            seen_grants: tokio::sync::Mutex::new(HashMap::new()),
        };

        emit_cloud_usage_events(&runtime, "mesh-oss", "node-1", &["connected"]).await;
        sleep(Duration::from_millis(75)).await;

        assert!(batches.lock().await.is_empty());
    }

    #[tokio::test]
    async fn usage_ingest_failure_does_not_block_event_production() {
        let event = test_usage_event("connected");
        let started = std::time::Instant::now();
        let result = timeout(
            Duration::from_millis(100),
            post_usage_batch(
                &reqwest::Client::new(),
                "http://127.0.0.1:1/v1/hub/events",
                "ingest-secret",
                &[event],
            ),
        )
        .await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    struct GrantHub {
        url: String,
        task: tokio::task::JoinHandle<anyhow::Result<()>>,
        key_pair: Ed25519KeyPair,
        _temp: TempDir,
    }

    impl GrantHub {
        async fn start() -> Self {
            let key_pair = test_key_pair(7);
            let public_key = URL_SAFE_NO_PAD.encode(key_pair.public_key().as_ref());
            let hub_temp = TempDir::new().unwrap();
            let hub_addr = free_addr();
            let hub_url = format!("http://{hub_addr}");
            let hub_lux = hub_temp.path().join("lux");
            std::env::set_var(ISSUER_PUBKEY_ENV, public_key);
            std::env::set_var(HUB_URL_ENV, &hub_url);
            let task = tokio::spawn(start(
                hub_addr,
                Some(hub_lux.to_string_lossy().to_string()),
                true,
            ));
            wait_for_health(&hub_url).await;
            Self {
                url: hub_url,
                task,
                key_pair,
                _temp: hub_temp,
            }
        }
    }

    #[derive(Debug, Clone, Deserialize)]
    struct TestCloudProvisionRequest {
        hub_url: String,
        mesh_id: String,
        mesh_name: String,
        node_id: String,
        node_slug: String,
    }

    struct CloudProvisionServer {
        url: String,
        requests: Arc<TokioMutex<Vec<TestCloudProvisionRequest>>>,
        _task: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    async fn start_cloud_provision_server(
        audience: String,
        key_pair: Ed25519KeyPair,
        expected_api_key: &'static str,
    ) -> CloudProvisionServer {
        let requests = Arc::new(TokioMutex::new(Vec::<TestCloudProvisionRequest>::new()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let key_pair = Arc::new(key_pair);
        async fn provision_handler(
            axum::extract::State((requests, audience, key_pair, expected_api_key)): axum::extract::State<(
                Arc<TokioMutex<Vec<TestCloudProvisionRequest>>>,
                String,
                Arc<Ed25519KeyPair>,
                &'static str,
            )>,
            headers: HeaderMap,
            Json(req): Json<TestCloudProvisionRequest>,
        ) -> impl IntoResponse {
            let expected_auth = format!("Bearer {expected_api_key}");
            if headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                != Some(expected_auth.as_str())
            {
                return axum::http::StatusCode::UNAUTHORIZED.into_response();
            }
            requests.lock().await.push(req.clone());
            let payload = serde_json::to_vec(&serde_json::json!({
                "v": 1,
                "aud": audience,
                "exp": crate::util::now_ts() + 60,
                "jti": format!("jti-{}", req.node_id),
                "account_id": "acct_cli",
                "mesh_id": req.mesh_id,
                "mesh_name": req.mesh_name
            }))
            .unwrap();
            let signature = key_pair.sign(&payload);
            Json(serde_json::json!({
                "grant": format!(
                    "{GRANT_PREFIX}{}.{}",
                    URL_SAFE_NO_PAD.encode(payload),
                    URL_SAFE_NO_PAD.encode(signature.as_ref())
                )
            }))
            .into_response()
        }

        let app = Router::new()
            .route("/api/hub/provision", post(provision_handler))
            .with_state((
                requests.clone(),
                audience,
                key_pair,
                expected_api_key,
            ));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await?;
            Ok(())
        });
        CloudProvisionServer {
            url: format!("http://{addr}"),
            requests,
            _task: task,
        }
    }

    struct UnexpectedCloudServer {
        url: String,
        hits: Arc<TokioMutex<usize>>,
        _task: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    async fn start_unexpected_cloud_server() -> UnexpectedCloudServer {
        let hits = Arc::new(TokioMutex::new(0usize));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        async fn handler(
            axum::extract::State(hits): axum::extract::State<Arc<TokioMutex<usize>>>,
        ) -> impl IntoResponse {
            *hits.lock().await += 1;
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
        let app = Router::new()
            .route("/api/hub/provision", post(handler))
            .with_state(hits.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await?;
            Ok(())
        });
        UnexpectedCloudServer {
            url: format!("http://{addr}"),
            hits,
            _task: task,
        }
    }


    struct IngestServer {
        url: String,
        _task: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    async fn start_ingest_server(
        batches: Arc<TokioMutex<Vec<Vec<UsageEvent>>>>,
        status: axum::http::StatusCode,
    ) -> IngestServer {
        async fn ingest_handler(
            axum::extract::State((batches, status)): axum::extract::State<(
                Arc<TokioMutex<Vec<Vec<UsageEvent>>>>,
                axum::http::StatusCode,
            )>,
            headers: HeaderMap,
            Json(batch): Json<UsageEventBatch>,
        ) -> impl IntoResponse {
            assert_eq!(
                headers
                    .get(axum::http::header::AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer ingest-secret")
            );
            batches.lock().await.push(batch.events);
            status
        }

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/hub/events", post(ingest_handler))
            .with_state((batches, status));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await?;
            Ok(())
        });
        IngestServer {
            url: format!("http://{addr}/v1/hub/events"),
            _task: task,
        }
    }

    async fn wait_for_batches(
        batches: &Arc<TokioMutex<Vec<Vec<UsageEvent>>>>,
        expected: usize,
    ) -> Vec<Vec<UsageEvent>> {
        for _ in 0..50 {
            let snapshot = batches.lock().await.clone();
            if snapshot.len() >= expected {
                return snapshot;
            }
            sleep(Duration::from_millis(20)).await;
        }
        batches.lock().await.clone()
    }

    fn test_usage_event(event_type: &str) -> UsageEvent {
        UsageEvent {
            account_id: "acct_123".to_string(),
            mesh_id: "mesh-cloud".to_string(),
            node_id: "node-1".to_string(),
            event_type: event_type.to_string(),
            timestamp: crate::util::now_ts(),
        }
    }

    async fn post_grant(hub_url: &str, grant: String) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{hub_url}/v1/grants/provision"))
            .json(&serde_json::json!({
                "grant": grant,
                "node": "node-1",
                "slug": "rig"
            }))
            .send()
            .await
            .unwrap()
    }

    fn signed_grant(
        key_pair: &Ed25519KeyPair,
        audience: &str,
        exp: i64,
        jti: &str,
        mesh: &str,
    ) -> String {
        let payload = serde_json::to_vec(&serde_json::json!({
            "v": 1,
            "aud": audience,
            "exp": exp,
            "jti": jti,
            "account_id": "acct_123",
            "mesh_id": mesh,
            "mesh_name": "default"
        }))
        .unwrap();
        let signature = key_pair.sign(&payload);
        format!(
            "{GRANT_PREFIX}{}.{}",
            URL_SAFE_NO_PAD.encode(payload),
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        )
    }

    fn test_key_pair(seed_byte: u8) -> Ed25519KeyPair {
        Ed25519KeyPair::from_seed_unchecked(&[seed_byte; 32]).unwrap()
    }

    fn clear_hub_env() {
        std::env::remove_var(ADMIN_TOKEN_ENV);
        std::env::remove_var(ISSUER_PUBKEY_ENV);
        std::env::remove_var(CLOUD_INGEST_URL_ENV);
        std::env::remove_var(CLOUD_INGEST_TOKEN_ENV);
        std::env::remove_var(HUB_URL_ENV);
        std::env::remove_var(CLOUD_API_URL_ENV);
        std::env::remove_var(API_KEY_ENV);
        std::env::remove_var(CLOUD_API_KEY_ENV);
        std::env::remove_var(HOSTED_HUB_URL_ENV);
    }

    fn env_lock() -> &'static TokioMutex<()> {
        ENV_LOCK.get_or_init(|| TokioMutex::new(()))
    }

    fn temp_paths(temp: &TempDir) -> ViaPaths {
        ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
            auth_config: temp.path().join("auth.json"),
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

    async fn register_test_node(
        client: &reqwest::Client,
        hub_url: &str,
        mesh: &str,
        node: &Node,
    ) -> String {
        client
            .post(format!("{hub_url}/v1/nodes/register"))
            .json(&RegisterNodeRequest {
                mesh: mesh.to_string(),
                node: node.id.clone(),
                slug: node.slug.clone(),
            })
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json::<AuthResponse>()
            .await
            .unwrap()
            .token
    }
}
