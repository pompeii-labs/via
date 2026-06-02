use crate::model::{MeshSnapshot, Service};
use crate::paths::ViaPaths;
use crate::security;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcRequest {
    Ping,
    NodeInfo,
    ExportSnapshot,
    ImportSnapshot {
        snapshot: MeshSnapshot,
    },
    DeployImage {
        image: String,
        service: String,
        container: String,
        port: Option<String>,
        env: Vec<(String, String)>,
        command: Vec<String>,
    },
    ContainerStatus {
        container: String,
    },
    Logs {
        container: String,
        follow: bool,
    },
    Exec {
        command: Vec<String>,
    },
    Stop {
        container: String,
    },
    Restart {
        container: String,
    },
    Remove {
        container: String,
    },
    DockerCheck,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcEnvelope {
    pub version: u8,
    pub timestamp_ms: i64,
    pub nonce: String,
    pub payload: String,
    pub signature: String,
}

#[derive(Debug)]
pub struct VerifiedRpcRequest {
    pub request: RpcRequest,
    pub nonce: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcResponse {
    Ok,
    Pong,
    NodeInfo { iroh_addr: Option<String> },
    Snapshot { snapshot: MeshSnapshot },
    Service { service: Service },
    ContainerStatus { status: String },
    Logs { output: String },
    Exec { output: String },
    Error { message: String },
}

pub async fn call(addr: &str, request: RpcRequest) -> Result<RpcResponse> {
    let paths = ViaPaths::new()?;
    call_with_paths(addr, &paths, request).await
}

pub async fn call_with_paths(
    addr: &str,
    paths: &ViaPaths,
    request: RpcRequest,
) -> Result<RpcResponse> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("failed to connect to Via daemon at {addr}"))?;
    let encoded = encode_request(paths, request)?;
    stream.write_all(&encoded).await?;
    stream.write_all(b"\n").await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.trim().is_empty() {
        bail!("Via daemon at {addr} returned an empty response");
    }
    let response = decode_response(paths, &line)?;
    if let RpcResponse::Error { message } = &response {
        return Err(anyhow!(message.clone()));
    }
    Ok(response)
}

pub fn encode_request(paths: &ViaPaths, request: RpcRequest) -> Result<Vec<u8>> {
    encode_frame(paths, "rpc.request", &request)
}

pub fn encode_response(paths: &ViaPaths, response: &RpcResponse) -> Result<Vec<u8>> {
    encode_frame(paths, "rpc.response", response)
}

pub fn decode_response(paths: &ViaPaths, line: &str) -> Result<RpcResponse> {
    decode_frame(paths, "rpc.response", line)
}

pub fn verify_request(paths: &ViaPaths, line: &str) -> Result<VerifiedRpcRequest> {
    let Some(key) = security::mesh_key_if_present(paths)? else {
        return Ok(VerifiedRpcRequest {
            request: serde_json::from_str(line)?,
            nonce: None,
        });
    };
    let envelope = verify_envelope(&key, "rpc.request", line)?;
    let plaintext = security::decrypt_string(&key, &envelope.payload)?;
    Ok(VerifiedRpcRequest {
        request: serde_json::from_str(&plaintext)?,
        nonce: Some(envelope.nonce),
    })
}

#[cfg(test)]
fn verify_signed_request(paths: &ViaPaths, line: &str) -> Result<RpcRequest> {
    Ok(verify_request(paths, line)?.request)
}

fn encode_frame<T: Serialize>(paths: &ViaPaths, kind: &str, value: &T) -> Result<Vec<u8>> {
    let Some(key) = security::mesh_key_if_present(paths)? else {
        return Ok(serde_json::to_vec(value)?);
    };
    let plaintext = serde_json::to_string(value)?;
    let payload = security::encrypt_string(&key, &plaintext)?;
    let timestamp_ms = security::now_ms();
    let nonce = security::nonce()?;
    let signature = security::sign(&key, &signed_payload(kind, timestamp_ms, &nonce, &payload))?;
    Ok(serde_json::to_vec(&RpcEnvelope {
        version: 1,
        timestamp_ms,
        nonce,
        payload,
        signature,
    })?)
}

fn decode_frame<T: for<'de> Deserialize<'de>>(
    paths: &ViaPaths,
    kind: &str,
    line: &str,
) -> Result<T> {
    let Some(key) = security::mesh_key_if_present(paths)? else {
        return Ok(serde_json::from_str(line)?);
    };
    let envelope = verify_envelope(&key, kind, line)?;
    let plaintext = security::decrypt_string(&key, &envelope.payload)?;
    Ok(serde_json::from_str(&plaintext)?)
}

fn verify_envelope(key: &[u8], kind: &str, line: &str) -> Result<RpcEnvelope> {
    let envelope: RpcEnvelope = serde_json::from_str(line)
        .context("encrypted RPC envelope required once mesh auth is initialized")?;
    if envelope.version != 1 {
        bail!("unsupported RPC envelope version");
    }
    let age_ms = (security::now_ms() - envelope.timestamp_ms).abs();
    if age_ms > 5 * 60 * 1000 {
        bail!("RPC request timestamp is outside the accepted window");
    }
    let signed = signed_payload(
        kind,
        envelope.timestamp_ms,
        &envelope.nonce,
        &envelope.payload,
    );
    security::verify(key, &signed, &envelope.signature)?;
    Ok(envelope)
}

fn signed_payload(kind: &str, timestamp_ms: i64, nonce: &str, payload: &str) -> Vec<u8> {
    let mut out = b"via-rpc-v1".to_vec();
    out.push(b'\n');
    out.extend_from_slice(kind.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(timestamp_ms.to_string().as_bytes());
    out.push(b'\n');
    out.extend_from_slice(nonce.as_bytes());
    out.push(b'\n');
    out.extend_from_slice(payload.as_bytes());
    out
}

pub async fn ping(addr: &str) -> Result<()> {
    match call(addr, RpcRequest::Ping).await? {
        RpcResponse::Pong => Ok(()),
        other => Err(anyhow!("unexpected ping response: {other:?}")),
    }
}

pub async fn wait_until_ready(addr: &str) -> Result<()> {
    let mut last_error = None;
    for _ in 0..50 {
        match ping(addr).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(anyhow!(
        "Via daemon at {addr} did not become ready: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        decode_response, encode_request, encode_response, signed_payload, verify_signed_request,
        RpcEnvelope, RpcRequest, RpcResponse,
    };
    use crate::paths::ViaPaths;
    use crate::security;
    use tempfile::TempDir;

    fn temp_paths(temp: &TempDir) -> ViaPaths {
        ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            iroh_key: temp.path().join("iroh.key"),
        }
    }

    #[test]
    fn rpc_messages_are_json_lines() {
        let request = RpcRequest::DeployImage {
            image: "nginx:latest".to_string(),
            service: "hello".to_string(),
            container: "via-hello".to_string(),
            port: Some("8080:80".to_string()),
            env: vec![],
            command: vec![],
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains(r#""type":"deploy_image""#));

        let response = RpcResponse::Pong;
        let encoded = serde_json::to_string(&response).unwrap();
        assert_eq!(encoded, r#"{"type":"pong"}"#);
    }

    #[test]
    fn signed_rpc_envelopes_verify_with_mesh_key() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        crate::security::ensure_mesh_key(&paths).unwrap();
        let encoded = encode_request(&paths, RpcRequest::Ping).unwrap();
        let line = String::from_utf8(encoded).unwrap();
        let verified = verify_signed_request(&paths, &line).unwrap();
        assert!(matches!(verified, RpcRequest::Ping));
    }

    #[test]
    fn encrypted_rpc_requests_do_not_expose_plaintext() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        crate::security::ensure_mesh_key(&paths).unwrap();
        let encoded = encode_request(
            &paths,
            RpcRequest::DeployImage {
                image: "private-image:latest".to_string(),
                service: "secret-service".to_string(),
                container: "via-secret-service".to_string(),
                port: None,
                env: vec![("API_KEY".to_string(), "super-secret-value".to_string())],
                command: vec!["npm".to_string(), "start".to_string()],
            },
        )
        .unwrap();
        let line = String::from_utf8(encoded).unwrap();

        assert!(!line.contains("deploy_image"));
        assert!(!line.contains("private-image"));
        assert!(!line.contains("super-secret-value"));
        assert!(verify_signed_request(&paths, &line).is_ok());
    }

    #[test]
    fn encrypted_rpc_responses_do_not_expose_plaintext() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        crate::security::ensure_mesh_key(&paths).unwrap();
        let encoded = encode_response(
            &paths,
            &RpcResponse::Logs {
                output: "sensitive-log-line".to_string(),
            },
        )
        .unwrap();
        let line = String::from_utf8(encoded).unwrap();

        assert!(!line.contains("sensitive-log-line"));
        assert!(matches!(
            decode_response(&paths, &line).unwrap(),
            RpcResponse::Logs { output } if output == "sensitive-log-line"
        ));
    }

    #[test]
    fn signed_rpc_rejects_tampered_requests() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        crate::security::ensure_mesh_key(&paths).unwrap();
        let encoded = encode_request(&paths, RpcRequest::Ping).unwrap();
        let mut envelope: RpcEnvelope = serde_json::from_slice(&encoded).unwrap();
        envelope.nonce.push_str("-tampered");
        let line = serde_json::to_string(&envelope).unwrap();

        assert!(verify_signed_request(&paths, &line).is_err());
    }

    #[test]
    fn signed_rpc_rejects_stale_timestamps() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        let key = crate::security::ensure_mesh_key(&paths).unwrap();
        let timestamp_ms = security::now_ms() - 10 * 60 * 1000;
        let nonce = "stale-nonce";
        let payload =
            security::encrypt_string(&key, &serde_json::to_string(&RpcRequest::Ping).unwrap())
                .unwrap();
        let signed_payload = signed_payload("rpc.request", timestamp_ms, nonce, &payload);
        let signature = security::sign(&key, &signed_payload).unwrap();
        let line = serde_json::to_string(&RpcEnvelope {
            version: 1,
            timestamp_ms,
            nonce: nonce.to_string(),
            payload,
            signature,
        })
        .unwrap();

        assert!(verify_signed_request(&paths, &line).is_err());
    }

    #[test]
    fn signed_rpc_rejects_response_frames_as_requests() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        let key = crate::security::ensure_mesh_key(&paths).unwrap();
        let timestamp_ms = security::now_ms();
        let nonce = "wrong-kind";
        let payload =
            security::encrypt_string(&key, &serde_json::to_string(&RpcRequest::Ping).unwrap())
                .unwrap();
        let signed_payload = signed_payload("rpc.response", timestamp_ms, nonce, &payload);
        let signature = security::sign(&key, &signed_payload).unwrap();
        let line = serde_json::to_string(&RpcEnvelope {
            version: 1,
            timestamp_ms,
            nonce: nonce.to_string(),
            payload,
            signature,
        })
        .unwrap();

        assert!(verify_signed_request(&paths, &line).is_err());
    }
}
