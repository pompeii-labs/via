use crate::cli::RouteMode;
use crate::model::{Node, Service};
use crate::paths::ViaPaths;
use crate::rpc::{RpcRequest, RpcResponse};
use crate::state::ViaState;
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, SecretKey};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const RPC_ALPN: &[u8] = b"via-rpc/1";
pub const PROXY_ALPN: &[u8] = b"via-proxy/1";

#[derive(Debug, Serialize, Deserialize)]
struct ProxyRequest {
    host: String,
    port: u16,
}

pub async fn endpoint(paths: &ViaPaths) -> Result<Endpoint> {
    let key = ensure_iroh_key(paths)?;
    Ok(Endpoint::builder(presets::N0)
        .secret_key(key)
        .alpns(vec![RPC_ALPN.to_vec(), PROXY_ALPN.to_vec()])
        .bind()
        .await?)
}

pub async fn update_local_iroh_addr(state: &mut ViaState, endpoint: &Endpoint) -> Result<()> {
    if let Ok(mut local) = state.local_node().await {
        local.iroh_addr = Some(serialize_addr(&endpoint.addr())?);
        local.last_seen_at = Some(crate::util::now_ts());
        state.upsert_node(&local).await?;
        state.persist().await?;
    }
    Ok(())
}

pub fn serialize_addr(addr: &EndpointAddr) -> Result<String> {
    Ok(serde_json::to_string(addr)?)
}

pub fn node_iroh_id(node: &Node) -> Result<Option<String>> {
    let Some(raw) = &node.iroh_addr else {
        return Ok(None);
    };
    let addr: EndpointAddr = serde_json::from_str(raw)?;
    Ok(Some(addr.id.to_string()))
}

pub async fn call_node(
    state: &ViaState,
    node: &Node,
    request: RpcRequest,
    route: RouteMode,
) -> Result<RpcResponse> {
    match route {
        RouteMode::Direct => crate::rpc::call(&node.daemon_addr, request).await,
        RouteMode::Iroh => call_node_iroh(state.paths(), node, request).await,
        RouteMode::Auto => match crate::rpc::call(&node.daemon_addr, request.clone()).await {
            Ok(response) => Ok(response),
            Err(direct_error) => match call_node_iroh(state.paths(), node, request).await {
                Ok(response) => Ok(response),
                Err(iroh_error) => Err(anyhow!(
                    "direct route failed: {direct_error}; iroh route failed: {iroh_error}"
                )),
            },
        },
    }
}

pub async fn call_node_iroh(
    paths: &ViaPaths,
    node: &Node,
    request: RpcRequest,
) -> Result<RpcResponse> {
    let endpoint = endpoint(paths).await?;
    let addr = node_iroh_addr(node)?;
    let conn = endpoint
        .connect(addr, RPC_ALPN)
        .await
        .with_context(|| format!("failed to connect to '{}' over iroh", node.slug))?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let encoded = crate::rpc::encode_request(paths, request)?;
    send.write_all(&encoded).await?;
    send.write_all(b"\n").await?;
    send.finish()?;
    send.stopped().await?;

    let response = recv.read_to_end(16 * 1024 * 1024).await?;
    endpoint.close().await;
    let line = std::str::from_utf8(&response)?;
    let response = crate::rpc::decode_response(paths, line)?;
    if let RpcResponse::Error { message } = &response {
        return Err(anyhow!(message.clone()));
    }
    Ok(response)
}

pub async fn proxy_service(
    state: &ViaState,
    service: &Service,
    listen: &str,
    route: RouteMode,
) -> Result<()> {
    let node = state
        .node_by_id(&service.node_id)
        .await?
        .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
    let local = state.local_node().await?.id == node.id;
    let port = service
        .port
        .as_deref()
        .ok_or_else(|| anyhow!("service '{}' has no published port", service.name))?;
    let port = host_port(port)?;
    let listener = TcpListener::bind(listen).await?;
    let bound = listener.local_addr()?;
    println!("via://{}/{} -> http://{}", node.slug, service.name, bound);

    loop {
        let (socket, _) = listener.accept().await?;
        let node = node.clone();
        let paths = state.paths().clone();
        tokio::spawn(async move {
            let result = if local || matches!(route, RouteMode::Direct) {
                proxy_direct(socket, &node, port).await
            } else {
                proxy_iroh(socket, &paths, &node, port).await
            };
            if let Err(error) = result {
                eprintln!("via proxy connection failed: {error}");
            }
        });
    }
}

pub async fn handle_rpc_connection(
    state: &mut ViaState,
    paths: &ViaPaths,
    seen_nonces: &mut std::collections::HashSet<String>,
    conn: iroh::endpoint::Connection,
) -> Result<()> {
    ensure_known_peer(state, &conn).await?;
    let (mut send, mut recv) = conn.accept_bi().await?;
    let line = recv.read_to_end(16 * 1024 * 1024).await?;
    let line = std::str::from_utf8(&line)?;
    let response = crate::daemon::handle_rpc_line(state, paths, seen_nonces, line).await?;
    send.write_all(&crate::rpc::encode_response(paths, &response)?)
        .await?;
    send.write_all(b"\n").await?;
    send.finish()?;
    send.stopped().await?;
    Ok(())
}

pub async fn authorize_iroh_peer(
    state: &ViaState,
    conn: &iroh::endpoint::Connection,
) -> Result<()> {
    ensure_known_peer(state, conn).await
}

pub async fn handle_proxy_connection(conn: iroh::endpoint::Connection) -> Result<()> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    let header = read_json_line(&mut recv).await?;
    let request: ProxyRequest = serde_json::from_slice(&header)?;
    let mut upstream = TcpStream::connect((request.host.as_str(), request.port)).await?;
    let (mut upstream_read, mut upstream_write) = upstream.split();

    let to_upstream = async {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let read = recv.read(&mut buf).await?;
            let Some(read) = read else {
                break;
            };
            upstream_write.write_all(&buf[..read]).await?;
        }
        upstream_write.shutdown().await?;
        Result::<()>::Ok(())
    };
    let from_upstream = async {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let read = upstream_read.read(&mut buf).await?;
            if read == 0 {
                break;
            }
            send.write_all(&buf[..read]).await?;
        }
        send.finish()?;
        send.stopped().await?;
        Result::<()>::Ok(())
    };
    tokio::try_join!(to_upstream, from_upstream)?;
    Ok(())
}

fn ensure_iroh_key(paths: &ViaPaths) -> Result<SecretKey> {
    paths.ensure()?;
    if paths.iroh_key.exists() {
        harden_iroh_key_permissions(paths)?;
        let encoded = std::fs::read_to_string(&paths.iroh_key)
            .with_context(|| format!("failed to read {}", paths.iroh_key.display()))?;
        let bytes = B64.decode(encoded.trim())?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("invalid Via iroh key length"))?;
        return Ok(SecretKey::from_bytes(&bytes));
    }
    let key = SecretKey::generate();
    write_iroh_key(paths, &B64.encode(key.to_bytes()))?;
    Ok(key)
}

#[cfg(unix)]
fn harden_iroh_key_permissions(paths: &ViaPaths) -> Result<()> {
    std::fs::set_permissions(&paths.iroh_key, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn harden_iroh_key_permissions(_paths: &ViaPaths) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn write_iroh_key(paths: &ViaPaths, encoded: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&paths.iroh_key)
        .with_context(|| format!("failed to create {}", paths.iroh_key.display()))?;
    file.write_all(encoded.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_iroh_key(paths: &ViaPaths, encoded: &str) -> Result<()> {
    std::fs::write(&paths.iroh_key, encoded)?;
    Ok(())
}

fn node_iroh_addr(node: &Node) -> Result<EndpointAddr> {
    let raw = node
        .iroh_addr
        .as_ref()
        .ok_or_else(|| anyhow!("node '{}' has no iroh route yet", node.slug))?;
    Ok(serde_json::from_str(raw)?)
}

async fn ensure_known_peer(state: &ViaState, conn: &iroh::endpoint::Connection) -> Result<()> {
    let remote = conn.remote_id().to_string();
    let nodes = state.nodes().await?;
    for node in nodes {
        if node_iroh_id(&node)?.as_deref() == Some(remote.as_str()) {
            return Ok(());
        }
    }
    bail!("unknown iroh peer {remote}")
}

async fn proxy_direct(mut inbound: TcpStream, node: &Node, port: u16) -> Result<()> {
    let host = node
        .daemon_addr
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or("127.0.0.1");
    let mut upstream = TcpStream::connect((host, port)).await?;
    tokio::io::copy_bidirectional(&mut inbound, &mut upstream).await?;
    Ok(())
}

async fn proxy_iroh(
    mut inbound: TcpStream,
    paths: &ViaPaths,
    node: &Node,
    port: u16,
) -> Result<()> {
    let endpoint = endpoint(paths).await?;
    let conn = endpoint.connect(node_iroh_addr(node)?, PROXY_ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let request = ProxyRequest {
        host: "127.0.0.1".to_string(),
        port,
    };
    send.write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    send.write_all(b"\n").await?;

    let (mut inbound_read, mut inbound_write) = inbound.split();
    let to_iroh = async {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let read = inbound_read.read(&mut buf).await?;
            if read == 0 {
                break;
            }
            send.write_all(&buf[..read]).await?;
        }
        send.finish()?;
        send.stopped().await?;
        Result::<()>::Ok(())
    };
    let from_iroh = async {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let read = recv.read(&mut buf).await?;
            let Some(read) = read else {
                break;
            };
            inbound_write.write_all(&buf[..read]).await?;
        }
        inbound_write.shutdown().await?;
        Result::<()>::Ok(())
    };
    tokio::try_join!(to_iroh, from_iroh)?;
    endpoint.close().await;
    Ok(())
}

async fn read_json_line(recv: &mut iroh::endpoint::RecvStream) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = recv.read(&mut byte).await?;
        let Some(read) = read else {
            bail!("iroh stream ended before proxy request");
        };
        if read == 0 {
            continue;
        }
        if byte[0] == b'\n' {
            return Ok(out);
        }
        out.push(byte[0]);
        if out.len() > 64 * 1024 {
            bail!("proxy request header too large");
        }
    }
}

fn host_port(port: &str) -> Result<u16> {
    let raw = port.split(':').next().unwrap_or(port);
    raw.parse::<u16>()
        .with_context(|| format!("invalid service host port '{raw}'"))
}

#[allow(dead_code)]
fn _socket_addr(addr: SocketAddr) -> SocketAddr {
    addr
}

#[cfg(test)]
mod tests {
    use super::{call_node_iroh, node_iroh_id, serialize_addr};
    use crate::model::{Mesh, Node};
    use crate::state::ViaState;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn missing_iroh_addr_is_not_an_identity() {
        let node = Node {
            id: "node".to_string(),
            slug: "rig".to_string(),
            display_name: "rig".to_string(),
            addresses: vec![],
            daemon_addr: "rig:47819".to_string(),
            iroh_addr: None,
            public: false,
            created_at: 1,
            last_seen_at: None,
        };
        assert!(node_iroh_id(&node).unwrap().is_none());
    }

    #[tokio::test]
    async fn endpoint_addr_round_trips_through_node_state() {
        let temp = tempfile::tempdir().unwrap();
        let paths = crate::paths::ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            iroh_key: temp.path().join("iroh.key"),
        };
        let endpoint = super::endpoint(&paths).await.unwrap();
        let node = Node {
            id: "node".to_string(),
            slug: "rig".to_string(),
            display_name: "rig".to_string(),
            addresses: vec![],
            daemon_addr: "rig:47819".to_string(),
            iroh_addr: Some(serialize_addr(&endpoint.addr()).unwrap()),
            public: false,
            created_at: 1,
            last_seen_at: None,
        };
        assert_eq!(
            node_iroh_id(&node).unwrap().unwrap(),
            endpoint.id().to_string()
        );
        endpoint.close().await;
    }

    #[tokio::test]
    async fn iroh_rpc_ping_round_trips_between_known_nodes() {
        let temp_a = tempfile::tempdir().unwrap();
        let temp_b = tempfile::tempdir().unwrap();
        let paths_a = temp_paths(temp_a.path());
        let paths_b = temp_paths(temp_b.path());
        crate::security::ensure_mesh_key(&paths_a).unwrap();
        paths_b.ensure().unwrap();
        std::fs::copy(&paths_a.mesh_key, &paths_b.mesh_key).unwrap();

        let endpoint_a = super::endpoint(&paths_a).await.unwrap();
        let endpoint_b = super::endpoint(&paths_b).await.unwrap();
        let node_a = node(
            "node-a",
            "laptop",
            Some(serialize_addr(&endpoint_a.addr()).unwrap()),
            Some(1),
        );
        let node_b = node(
            "node-b",
            "rig",
            Some(serialize_addr(&endpoint_b.addr()).unwrap()),
            None,
        );
        endpoint_a.close().await;

        let mut state_b = ViaState::open(paths_b.clone()).await.unwrap();
        state_b
            .save_mesh(&Mesh {
                id: "mesh".to_string(),
                created_at: 1,
            })
            .await
            .unwrap();
        state_b.save_local_node_id(&node_b.id).await.unwrap();
        state_b.upsert_node(&node_a).await.unwrap();
        state_b.upsert_node(&node_b).await.unwrap();

        let server_endpoint = endpoint_b.clone();
        let mut seen = std::collections::HashSet::new();
        let server = async {
            let incoming = server_endpoint.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            super::handle_rpc_connection(&mut state_b, &paths_b, &mut seen, conn)
                .await
                .unwrap();
            state_b.shutdown().await.unwrap();
        };
        let client = async {
            call_node_iroh(&paths_a, &node_b, crate::rpc::RpcRequest::Ping)
                .await
                .unwrap()
        };

        let ((), response) = tokio::join!(server, client);
        assert!(matches!(response, crate::rpc::RpcResponse::Pong));
        endpoint_b.close().await;
    }

    #[tokio::test]
    async fn iroh_proxy_pipes_bytes_to_local_tcp_service() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let temp_a = tempfile::tempdir().unwrap();
        let temp_b = tempfile::tempdir().unwrap();
        let paths_a = temp_paths(temp_a.path());
        let paths_b = temp_paths(temp_b.path());
        let endpoint_a = super::endpoint(&paths_a).await.unwrap();
        let endpoint_b = super::endpoint(&paths_b).await.unwrap();
        let node_a = node(
            "node-a",
            "laptop",
            Some(serialize_addr(&endpoint_a.addr()).unwrap()),
            Some(1),
        );
        let node_b = node(
            "node-b",
            "rig",
            Some(serialize_addr(&endpoint_b.addr()).unwrap()),
            None,
        );

        let echo_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_port = echo_listener.local_addr().unwrap().port();
        let echo = async {
            let (mut socket, _) = echo_listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let read = socket.read(&mut buf).await.unwrap();
            socket.write_all(&buf[..read]).await.unwrap();
        };

        let mut state_b = ViaState::open(paths_b.clone()).await.unwrap();
        state_b
            .save_mesh(&Mesh {
                id: "mesh".to_string(),
                created_at: 1,
            })
            .await
            .unwrap();
        state_b.save_local_node_id(&node_b.id).await.unwrap();
        state_b.upsert_node(&node_a).await.unwrap();
        state_b.upsert_node(&node_b).await.unwrap();

        let server_endpoint = endpoint_b.clone();
        let server = async {
            let incoming = server_endpoint.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            super::authorize_iroh_peer(&state_b, &conn).await.unwrap();
            super::handle_proxy_connection(conn).await.unwrap();
            state_b.shutdown().await.unwrap();
        };
        let client = async {
            let addr: iroh::EndpointAddr =
                serde_json::from_str(node_b.iroh_addr.as_ref().unwrap()).unwrap();
            let conn = endpoint_a.connect(addr, super::PROXY_ALPN).await.unwrap();
            let (mut send, mut recv) = conn.open_bi().await.unwrap();
            let request = super::ProxyRequest {
                host: "127.0.0.1".to_string(),
                port: echo_port,
            };
            send.write_all(serde_json::to_string(&request).unwrap().as_bytes())
                .await
                .unwrap();
            send.write_all(b"\nhello via").await.unwrap();
            send.finish().unwrap();
            send.stopped().await.unwrap();
            recv.read_to_end(1024).await.unwrap()
        };

        let ((), (), output) = tokio::join!(echo, server, client);
        assert_eq!(output, b"hello via");
        endpoint_a.close().await;
        endpoint_b.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn iroh_key_file_is_owner_only() {
        let temp = tempfile::tempdir().unwrap();
        let paths = temp_paths(temp.path());
        let endpoint = super::endpoint(&paths).await.unwrap();
        endpoint.close().await;
        let mode = std::fs::metadata(paths.iroh_key)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    fn temp_paths(root: &std::path::Path) -> crate::paths::ViaPaths {
        crate::paths::ViaPaths {
            root: root.to_path_buf(),
            lux: root.join("lux"),
            logs: root.join("logs"),
            bin: root.join("bin"),
            mesh_key: root.join("mesh.key"),
            iroh_key: root.join("iroh.key"),
        }
    }

    fn node(id: &str, slug: &str, iroh_addr: Option<String>, last_seen_at: Option<i64>) -> Node {
        Node {
            id: id.to_string(),
            slug: slug.to_string(),
            display_name: slug.to_string(),
            addresses: vec![slug.to_string()],
            daemon_addr: format!("{slug}:47819"),
            iroh_addr,
            public: false,
            created_at: 1,
            last_seen_at,
        }
    }
}
