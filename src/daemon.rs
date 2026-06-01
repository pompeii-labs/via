use crate::docker;
use crate::paths::ViaPaths;
use crate::rpc::{RpcRequest, RpcResponse};
use crate::state::ViaState;
use anyhow::{bail, Result};
use std::collections::HashSet;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub async fn run(bind: String, paths: ViaPaths) -> Result<()> {
    let listener = TcpListener::bind(&bind).await?;
    let addr = listener.local_addr()?;
    println!("via daemon listening on {addr}");
    let mut state = ViaState::open(paths).await?;
    let paths = state.paths().clone();
    let mut seen_nonces = HashSet::new();

    loop {
        let (socket, _) = listener.accept().await?;
        if let Err(error) = handle_connection(&mut state, &paths, &mut seen_nonces, socket).await {
            eprintln!("via daemon request failed: {error}");
        }
    }
}

async fn handle_connection(
    state: &mut ViaState,
    paths: &ViaPaths,
    seen_nonces: &mut HashSet<String>,
    socket: tokio::net::TcpStream,
) -> Result<()> {
    let mut reader = BufReader::new(socket);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let response = match crate::rpc::verify_request(paths, &line) {
        Ok(verified) => match verified.nonce {
            Some(nonce) => {
                if !seen_nonces.insert(nonce) {
                    RpcResponse::Error {
                        message: "replayed RPC nonce".to_string(),
                    }
                } else {
                    match handle_request(state, verified.request).await {
                        Ok(response) => response,
                        Err(error) => RpcResponse::Error {
                            message: error.to_string(),
                        },
                    }
                }
            }
            None => match handle_request(state, verified.request).await {
                Ok(response) => response,
                Err(error) => RpcResponse::Error {
                    message: error.to_string(),
                },
            },
        },
        Err(error) => RpcResponse::Error {
            message: error.to_string(),
        },
    };

    let mut socket = reader.into_inner();
    socket
        .write_all(&crate::rpc::encode_response(paths, &response)?)
        .await?;
    socket.write_all(b"\n").await?;
    Ok(())
}

async fn handle_request(state: &mut ViaState, request: RpcRequest) -> Result<RpcResponse> {
    match request {
        RpcRequest::Ping => Ok(RpcResponse::Pong),
        RpcRequest::ExportSnapshot => Ok(RpcResponse::Snapshot {
            snapshot: state.snapshot().await?,
        }),
        RpcRequest::ImportSnapshot { snapshot } => {
            state.import_snapshot(snapshot).await?;
            state.persist().await?;
            Ok(RpcResponse::Ok)
        }
        RpcRequest::DeployImage {
            image,
            service,
            container,
            port,
            env,
            command,
        } => {
            let local_node = state.local_node().await?;
            let service = docker::deploy_local_image(
                &local_node,
                &image,
                &service,
                &container,
                port,
                &env,
                &command,
            )?;
            state.upsert_service(&service).await?;
            state.append_event("service.started", &service).await?;
            state.persist().await?;
            Ok(RpcResponse::Service { service })
        }
        RpcRequest::DeployPath { .. } => {
            bail!("path deploy over daemon RPC is not implemented yet; use a Docker image for now")
        }
        RpcRequest::Logs { container, follow } => Ok(RpcResponse::Logs {
            output: docker::local_logs(&container, follow)?,
        }),
        RpcRequest::ContainerStatus { container } => Ok(RpcResponse::ContainerStatus {
            status: docker::local_container_status(&container)?,
        }),
        RpcRequest::Exec { command } => Ok(RpcResponse::Exec {
            output: crate::util::run_command_capture(&command)?,
        }),
        RpcRequest::Stop { container } => {
            docker::local_stop(&container)?;
            state.persist().await?;
            Ok(RpcResponse::Ok)
        }
        RpcRequest::Restart { container } => {
            docker::local_restart(&container)?;
            state.persist().await?;
            Ok(RpcResponse::Ok)
        }
        RpcRequest::Remove { container } => {
            docker::local_remove(&container)?;
            state.persist().await?;
            Ok(RpcResponse::Ok)
        }
        RpcRequest::DockerCheck => {
            docker::local_docker_check()?;
            Ok(RpcResponse::Ok)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::paths::ViaPaths;
    use crate::rpc::{self, RpcResponse};
    use tempfile::TempDir;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn daemon_responds_to_ping() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
        };
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = std_listener.local_addr().unwrap();
        drop(std_listener);

        let bind = addr.to_string();
        let client_paths = paths.clone();
        let task = tokio::spawn(async move {
            let _ = run(bind, paths).await;
        });

        let mut last_error = None;
        for _ in 0..25 {
            match rpc::call_with_paths(&addr.to_string(), &client_paths, rpc::RpcRequest::Ping)
                .await
            {
                Ok(rpc::RpcResponse::Pong) => {
                    task.abort();
                    return;
                }
                Ok(response) => {
                    last_error = Some(anyhow::anyhow!("unexpected response: {response:?}"));
                    sleep(Duration::from_millis(20)).await;
                }
                Err(error) => {
                    last_error = Some(error);
                    sleep(Duration::from_millis(20)).await;
                }
            }
        }
        task.abort();
        panic!("daemon did not respond to ping: {last_error:?}");
    }

    #[tokio::test]
    async fn daemon_rejects_replayed_signed_rpc_nonces() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
        };
        crate::security::ensure_mesh_key(&paths).unwrap();
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = std_listener.local_addr().unwrap();
        drop(std_listener);

        let bind = addr.to_string();
        let client_paths = paths.clone();
        let task = tokio::spawn(async move {
            let _ = run(bind, paths).await;
        });

        let mut ready = false;
        for _ in 0..25 {
            match rpc::call_with_paths(&addr.to_string(), &client_paths, rpc::RpcRequest::Ping)
                .await
            {
                Ok(RpcResponse::Pong) => {
                    ready = true;
                    break;
                }
                _ => sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(ready, "daemon did not become ready");

        let encoded = rpc::encode_request(&client_paths, rpc::RpcRequest::Ping).unwrap();
        let first = send_raw_rpc(&addr.to_string(), &client_paths, &encoded).await;
        let second = send_raw_rpc(&addr.to_string(), &client_paths, &encoded).await;
        task.abort();

        assert!(matches!(first, RpcResponse::Pong));
        assert!(matches!(second, RpcResponse::Error { message } if message.contains("replayed")));
    }

    async fn send_raw_rpc(addr: &str, paths: &ViaPaths, encoded: &[u8]) -> RpcResponse {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(encoded).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        rpc::decode_response(paths, &line).unwrap()
    }
}
