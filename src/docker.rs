use crate::model::{Node, Service, ServiceStatus};
use crate::ssh;
use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};
use uuid::Uuid;

pub async fn deploy_image(
    node: &Node,
    image: &str,
    container: &str,
    port: Option<String>,
    env: &[(String, String)],
    command: &[String],
) -> Result<Service> {
    let cmd = docker_run_command(image, container, port.as_deref(), env, command);
    run_on_node(node, "docker version >/dev/null")?;
    let _ = run_on_node(node, &format!("docker rm -f {container} >/dev/null 2>&1"));
    run_on_node(node, &cmd)?;
    Ok(service_from_node(node, image, container, port, command))
}

pub async fn deploy_path(
    node: &Node,
    path: &Path,
    container: &str,
    port: Option<String>,
    env: &[(String, String)],
    command: &[String],
) -> Result<Service> {
    let dockerfile = path.join("Dockerfile");
    if !dockerfile.exists() {
        bail!("path deploy requires a Dockerfile");
    }
    let image = format!("via/{}:latest", container.trim_start_matches("via-"));
    if is_local(node) {
        run_local("docker", &["build", "-t", &image, path_str(path)?])?;
    } else {
        let host = primary_addr(node)?;
        let remote_dir = format!("~/.via/builds/{container}");
        ssh::copy_dir(host, path_str(path)?, &remote_dir)?;
        run_on_node(node, &format!("docker build -t {image} {remote_dir}"))?;
    }
    deploy_image(node, &image, container, port, env, command).await
}

pub async fn logs(service: &Service, local: bool, follow: bool) -> Result<()> {
    if !local {
        match crate::rpc::call(
            &service.node_addr,
            crate::rpc::RpcRequest::Logs {
                container: service.container.clone(),
                follow,
            },
        )
        .await?
        {
            crate::rpc::RpcResponse::Logs { output } => {
                print!("{output}");
                return Ok(());
            }
            other => bail!("unexpected logs response: {other:?}"),
        }
    }
    let mut args = vec!["logs".to_string()];
    if follow {
        args.push("-f".to_string());
    }
    args.push(service.container.clone());
    run_docker_for_service(service, &args)
}

pub async fn stop(service: &Service, local: bool) -> Result<()> {
    if !local {
        crate::rpc::call(
            &service.node_addr,
            crate::rpc::RpcRequest::Stop {
                container: service.container.clone(),
            },
        )
        .await?;
        return Ok(());
    }
    run_docker_for_service(service, &["stop".to_string(), service.container.clone()])
}

pub async fn restart(service: &Service, local: bool) -> Result<()> {
    if !local {
        crate::rpc::call(
            &service.node_addr,
            crate::rpc::RpcRequest::Restart {
                container: service.container.clone(),
            },
        )
        .await?;
        return Ok(());
    }
    run_docker_for_service(service, &["restart".to_string(), service.container.clone()])
}

pub fn deploy_local_image(
    local_node: &Node,
    image: &str,
    service: &str,
    container: &str,
    port: Option<String>,
    env: &[(String, String)],
    command: &[String],
) -> Result<Service> {
    run_shell("docker version >/dev/null")?;
    let _ = run_shell(&format!("docker rm -f {container} >/dev/null 2>&1"));
    run_shell(&docker_run_command(
        image,
        container,
        port.as_deref(),
        env,
        command,
    ))?;
    let mut svc = service_from_node(local_node, image, container, port, command);
    svc.name = service.to_string();
    Ok(svc)
}

pub fn local_container_status(container: &str) -> Result<String> {
    let output = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Status}}", container])
        .output()
        .with_context(|| format!("failed to inspect {container}"))?;
    if !output.status.success() {
        return Ok("missing".to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn local_logs(container: &str, follow: bool) -> Result<String> {
    if follow {
        return Err(anyhow!(
            "remote follow logs are not supported; omit --follow"
        ));
    }
    let output = Command::new("docker")
        .arg("logs")
        .arg(container)
        .output()
        .with_context(|| format!("failed to read logs for {container}"))?;
    if !output.status.success() {
        bail!(
            "docker logs failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn local_stop(container: &str) -> Result<()> {
    run_local("docker", &["stop", container])
}

pub fn local_restart(container: &str) -> Result<()> {
    run_local("docker", &["restart", container])
}

pub fn local_remove(container: &str) -> Result<()> {
    run_local("docker", &["rm", "-f", container])
}

pub fn local_docker_check() -> Result<()> {
    run_shell("docker version >/dev/null")
}

fn docker_run_command(
    image: &str,
    container: &str,
    port: Option<&str>,
    env: &[(String, String)],
    command: &[String],
) -> String {
    let mut cmd = format!("docker run -d --name {container}");
    if let Some(port) = port {
        if port.contains(':') {
            cmd.push_str(&format!(" -p {port}"));
        } else {
            cmd.push_str(&format!(" -p {port}:{port}"));
        }
    }
    for (name, value) in env {
        cmd.push_str(" -e ");
        cmd.push_str(&shell_escape_env(name, value));
    }
    cmd.push(' ');
    cmd.push_str(image);
    for arg in command {
        cmd.push(' ');
        cmd.push_str(&shell_escape_arg(arg));
    }
    cmd
}

fn shell_escape_env(name: &str, value: &str) -> String {
    format!("{name}={}", shell_escape_arg(value))
}

fn shell_escape_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn run_docker_for_service(service: &Service, args: &[String]) -> Result<()> {
    let _ = service;
    run_local_dynamic("docker", args)
}

fn run_on_node(node: &Node, command: &str) -> Result<()> {
    if is_local(node) {
        run_shell(command)
    } else {
        ssh::remote(primary_addr(node)?, command)
    }
}

fn run_shell(command: &str) -> Result<()> {
    let status = Command::new("sh")
        .arg("-lc")
        .arg(command)
        .status()
        .with_context(|| format!("failed to run `{command}`"))?;
    if !status.success() {
        bail!("command failed: {command}");
    }
    Ok(())
}

fn run_local(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} failed with status {status}");
    }
    Ok(())
}

fn run_local_dynamic(program: &str, args: &[String]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} failed with status {status}");
    }
    Ok(())
}

fn is_local(node: &Node) -> bool {
    node.last_seen_at.is_some()
}

fn primary_addr(node: &Node) -> Result<&str> {
    node.addresses
        .first()
        .map(String::as_str)
        .context("node has no address")
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().context("path is not UTF-8")
}

fn service_from_node(
    node: &Node,
    target: &str,
    container: &str,
    port: Option<String>,
    command: &[String],
) -> Service {
    let now = crate::util::now_ts();
    Service {
        id: Uuid::new_v4().to_string(),
        name: container.trim_start_matches("via-").to_string(),
        node_id: node.id.clone(),
        node_slug: node.slug.clone(),
        node_addr: node.daemon_addr.clone(),
        target: target.to_string(),
        container: container.to_string(),
        port,
        command: command.to_vec(),
        status: ServiceStatus::Running,
        published_private: false,
        created_at: now,
        updated_at: now,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        docker_run_command, is_local, primary_addr, service_from_node, shell_escape_arg,
        shell_escape_env,
    };
    use crate::model::Node;

    fn node(last_seen_at: Option<i64>, addresses: Vec<String>) -> Node {
        Node {
            id: "node-1".to_string(),
            slug: "rig".to_string(),
            display_name: "rig".to_string(),
            addresses,
            daemon_addr: "rig:47819".to_string(),
            public: false,
            created_at: 1,
            last_seen_at,
        }
    }

    #[test]
    fn docker_run_command_maps_ports_and_env() {
        let cmd = docker_run_command(
            "nginx:latest",
            "via-web",
            Some("8080"),
            &[("API_KEY".to_string(), "value with spaces".to_string())],
            &[],
        );
        assert_eq!(
            cmd,
            "docker run -d --name via-web -p 8080:8080 -e API_KEY='value with spaces' nginx:latest"
        );

        let cmd = docker_run_command("nginx:latest", "via-web", Some("18080:80"), &[], &[]);
        assert_eq!(cmd, "docker run -d --name via-web -p 18080:80 nginx:latest");
    }

    #[test]
    fn shell_escape_env_quotes_single_quotes() {
        assert_eq!(shell_escape_env("TOKEN", "a'b"), "TOKEN='a'\\''b'");
        assert_eq!(shell_escape_arg("a b"), "'a b'");
    }

    #[test]
    fn docker_run_command_appends_command_args() {
        let cmd = docker_run_command(
            "alpine:latest",
            "via-job",
            None,
            &[],
            &["sh".to_string(), "-lc".to_string(), "echo 'hi'".to_string()],
        );
        assert_eq!(
            cmd,
            "docker run -d --name via-job alpine:latest 'sh' '-lc' 'echo '\\''hi'\\'''"
        );
    }

    #[test]
    fn node_helpers_distinguish_local_and_remote() {
        let local = node(Some(1), vec!["localhost".to_string()]);
        let remote = node(None, vec!["rig".to_string()]);
        assert!(is_local(&local));
        assert!(!is_local(&remote));
        assert_eq!(primary_addr(&remote).unwrap(), "rig");
        assert!(primary_addr(&node(None, vec![])).is_err());
    }

    #[test]
    fn service_from_node_uses_container_name_and_node_metadata() {
        let node = node(None, vec!["rig".to_string()]);
        let service = service_from_node(
            &node,
            "nginx:latest",
            "via-web",
            Some("18080:80".to_string()),
            &[],
        );
        assert_eq!(service.name, "web");
        assert_eq!(service.node_id, "node-1");
        assert_eq!(service.node_slug, "rig");
        assert_eq!(service.node_addr, "rig:47819");
        assert_eq!(service.port.as_deref(), Some("18080:80"));
        assert!(service.command.is_empty());
    }
}
