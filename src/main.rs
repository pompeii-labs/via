mod cli;
mod daemon;
mod docker;
mod model;
mod paths;
mod rpc;
mod security;
mod ssh;
mod state;
mod transport;
mod util;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command, NodeCommand, SecretCommand};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = paths::ViaPaths::new()?;
    if let Command::Daemon { bind } = &cli.command {
        daemon::run(bind.clone(), paths).await?;
        return Ok(());
    }

    let mut state = state::ViaState::open(paths.clone()).await?;

    match cli.command {
        Command::Init {
            name,
            mesh_id,
            node_id,
        } => {
            commands::init(&mut state, name, mesh_id, node_id).await?;
        }
        Command::Doctor => {
            commands::doctor(&state, &paths).await?;
        }
        Command::Add {
            ssh_host,
            name,
            public,
        } => {
            commands::add_node(&mut state, &paths, ssh_host, name, public).await?;
        }
        Command::Start { bind } => {
            commands::start(&paths, bind).await?;
        }
        Command::Nodes => {
            commands::nodes(&state).await?;
        }
        Command::Node { command } => match command {
            NodeCommand::Rename { old, new } => {
                commands::rename_node(&mut state, old, new).await?;
            }
            NodeCommand::Addr { node, address } => {
                commands::set_node_addr(&mut state, node, address).await?;
            }
            NodeCommand::Ping { node, route } => {
                commands::node_ping(&state, node, route).await?;
            }
            NodeCommand::Rm { node } => {
                commands::node_rm(&mut state, node).await?;
            }
        },
        Command::Ps => {
            commands::ps(&state).await?;
        }
        Command::Deploy {
            target,
            to,
            name,
            port,
            route,
            command,
        } => {
            commands::deploy(
                &mut state,
                &paths,
                commands::DeployArgs {
                    target,
                    to,
                    name,
                    port,
                    route,
                    command,
                },
            )
            .await?;
        }
        Command::Services => {
            commands::services(&state).await?;
        }
        Command::Status { service } => {
            commands::status(&state, service).await?;
        }
        Command::Logs {
            service,
            follow,
            limit,
        } => {
            commands::logs(&state, service, follow, limit).await?;
        }
        Command::Stop { service } => {
            commands::stop(&mut state, service).await?;
        }
        Command::Restart { service } => {
            commands::restart(&mut state, service).await?;
        }
        Command::Rm { service } => {
            commands::rm(&mut state, service).await?;
        }
        Command::Exec {
            node,
            route,
            command,
        } => {
            commands::exec(&mut state, node, route, command).await?;
        }
        Command::Proxy {
            service,
            listen,
            route,
        } => {
            commands::proxy(&state, service, listen, route).await?;
        }
        Command::Open { service } => {
            commands::open(&state, service).await?;
        }
        Command::Update {
            check,
            all,
            node,
            version,
        } => {
            commands::update(&mut state, check, all, node, version).await?;
        }
        Command::Publish { service, private } => {
            commands::publish(&mut state, service, private).await?;
        }
        Command::Secret { command } => match command {
            SecretCommand::Set { name, value } => {
                commands::secret_set(&mut state, &paths, name, value).await?;
            }
            SecretCommand::Delete { name } => {
                commands::secret_delete(&mut state, name).await?;
            }
            SecretCommand::List => {
                commands::secret_list(&state).await?;
            }
        },
        Command::Daemon { .. } => unreachable!("daemon is handled before state initialization"),
    }

    state.shutdown().await?;
    Ok(())
}

mod commands {
    use crate::cli::RouteMode;
    use crate::docker;
    use crate::model::{Mesh, Node, ServiceStatus};
    use crate::paths::ViaPaths;
    use crate::ssh;
    use crate::state::ViaState;
    use crate::util::{format_ts, normalize_slug, now_ts};
    use anyhow::{anyhow, bail, Context, Result};
    use std::path::PathBuf;
    use std::process::Command as ProcessCommand;
    use uuid::Uuid;

    const UPDATE_REPO: &str = "pompeii-labs/via";
    const INSTALL_URL: &str = "https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh";

    pub async fn init(
        state: &mut ViaState,
        name: Option<String>,
        mesh_id: Option<String>,
        node_id: Option<String>,
    ) -> Result<()> {
        state.ensure_dirs()?;
        crate::security::ensure_mesh_key(state.paths())?;
        if state.mesh().await?.is_some() {
            if let (Some(name), Some(node_id)) = (name, node_id) {
                let host = hostname()?;
                let slug = normalize_slug(&name)?;
                let node = Node {
                    id: node_id,
                    slug: slug.clone(),
                    display_name: host.clone(),
                    addresses: vec![host.clone()],
                    daemon_addr: format!("{host}:47819"),
                    iroh_addr: None,
                    public: false,
                    created_at: now_ts(),
                    last_seen_at: Some(now_ts()),
                };
                state.save_local_node_id(&node.id).await?;
                state.upsert_node(&node).await?;
                state.append_event("node.refreshed", &node).await?;
                println!("Refreshed Via node '{}'.", slug);
                return Ok(());
            }
            println!("Via mesh already initialized.");
            return Ok(());
        }

        let host = hostname()?;
        let slug = match name {
            Some(name) => normalize_slug(&name)?,
            None => normalize_slug(&host)?,
        };
        let mesh = Mesh {
            id: mesh_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            created_at: now_ts(),
        };
        let node = Node {
            id: node_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            slug: slug.clone(),
            display_name: host.clone(),
            addresses: vec![host.clone()],
            daemon_addr: format!("{host}:47819"),
            iroh_addr: None,
            public: false,
            created_at: now_ts(),
            last_seen_at: Some(now_ts()),
        };

        state.save_mesh(&mesh).await?;
        state.save_local_node_id(&node.id).await?;
        state.upsert_node(&node).await?;
        state.append_event("mesh.initialized", &mesh).await?;
        state.append_event("node.joined", &node).await?;

        println!("Initialized Via mesh {} as node '{}'.", mesh.id, slug);
        Ok(())
    }

    pub async fn add_node(
        state: &mut ViaState,
        paths: &ViaPaths,
        ssh_host: String,
        name: Option<String>,
        public: bool,
    ) -> Result<()> {
        let mesh = state
            .mesh()
            .await?
            .ok_or_else(|| anyhow!("run `via init` before adding nodes"))?;
        crate::security::ensure_mesh_key(paths)?;
        let slug = normalize_slug(name.as_deref().unwrap_or(&ssh_host))?;
        let existing = state.node_by_slug(&slug).await?;
        let node_id = existing
            .as_ref()
            .map(|node| node.id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        ssh::bootstrap(&ssh_host, paths, &slug, &mesh.id, &node_id).await?;
        let daemon_host = ssh::resolved_hostname(&ssh_host).unwrap_or_else(|_| ssh_host.clone());
        let daemon_addr = format!("{daemon_host}:47819");
        crate::rpc::wait_until_ready(&daemon_addr).await?;
        let iroh_addr = match crate::rpc::call(&daemon_addr, crate::rpc::RpcRequest::NodeInfo).await
        {
            Ok(crate::rpc::RpcResponse::NodeInfo { iroh_addr }) => iroh_addr,
            Ok(_) | Err(_) => existing.as_ref().and_then(|node| node.iroh_addr.clone()),
        };

        let node = Node {
            id: node_id,
            slug: slug.clone(),
            display_name: ssh_host.clone(),
            addresses: vec![ssh_host],
            daemon_addr,
            iroh_addr,
            public,
            created_at: now_ts(),
            last_seen_at: None,
        };
        state.upsert_node(&node).await?;
        state
            .append_event(
                if existing.is_some() {
                    "node.refreshed"
                } else {
                    "node.joined"
                },
                &node,
            )
            .await?;
        sync_all(state).await?;
        println!(
            "{} node '{}'.",
            if existing.is_some() {
                "Refreshed"
            } else {
                "Added"
            },
            slug
        );
        Ok(())
    }

    pub async fn start(paths: &ViaPaths, bind: String) -> Result<()> {
        if crate::rpc::ping("127.0.0.1:47819").await.is_ok() {
            println!("Via daemon is already running.");
            return Ok(());
        }
        paths.ensure()?;
        let exe = std::env::current_exe().context("failed to locate current via binary")?;
        let log = paths.root.join("daemon.log");
        let command = format!(
            "nohup '{}' daemon --bind '{}' > '{}' 2>&1 &",
            exe.display(),
            bind,
            log.display()
        );
        let status = ProcessCommand::new("sh")
            .arg("-lc")
            .arg(&command)
            .status()
            .context("failed to start via daemon")?;
        if !status.success() {
            bail!("failed to start via daemon");
        }
        let local_probe = local_probe_addr(&bind);
        crate::rpc::wait_until_ready(&local_probe).await?;
        println!("Via daemon started on {bind}.");
        Ok(())
    }

    fn local_probe_addr(bind: &str) -> String {
        match bind.rsplit_once(':') {
            Some((_, port)) => format!("127.0.0.1:{port}"),
            None => "127.0.0.1:47819".to_string(),
        }
    }

    pub async fn doctor(state: &ViaState, paths: &ViaPaths) -> Result<()> {
        let mut failed = false;
        check("state directory", paths.root.exists(), &mut failed);
        check("mesh key present", paths.mesh_key.exists(), &mut failed);
        match crate::security::read_mesh_key(paths) {
            Ok(_) => check("mesh key valid", true, &mut failed),
            Err(error) => {
                check("mesh key valid", false, &mut failed);
                println!("  {}", error);
            }
        }

        let local_node = state.local_node().await.ok();
        check("local node initialized", local_node.is_some(), &mut failed);
        for node in state.nodes().await? {
            if local_node.as_ref().is_some_and(|local| local.id == node.id) {
                let docker_ok = docker::local_docker_check().is_ok();
                check(&format!("docker on {}", node.slug), docker_ok, &mut failed);
            } else {
                let ping_ok = crate::transport::call_node(
                    state,
                    &node,
                    crate::rpc::RpcRequest::Ping,
                    RouteMode::Auto,
                )
                .await
                .is_ok();
                check(
                    &format!("daemon {} {}", node.slug, node.daemon_addr),
                    ping_ok,
                    &mut failed,
                );
            }
        }

        if failed {
            bail!("doctor found issues");
        }
        Ok(())
    }

    fn check(label: &str, ok: bool, failed: &mut bool) {
        println!("{:<34} {}", label, if ok { "ok" } else { "fail" });
        if !ok {
            *failed = true;
        }
    }

    pub async fn nodes(state: &ViaState) -> Result<()> {
        let nodes = state.nodes().await?;
        if nodes.is_empty() {
            println!("No nodes. Run `via init` first.");
            return Ok(());
        }
        println!("{:<18} {:<8} {:<6} ADDRESS", "NAME", "PUBLIC", "IROH");
        for node in nodes {
            println!(
                "{:<18} {:<8} {:<6} {}",
                node.slug,
                if node.public { "yes" } else { "no" },
                if node.iroh_addr.is_some() {
                    "yes"
                } else {
                    "no"
                },
                node.addresses.join(",")
            );
        }
        Ok(())
    }

    pub async fn rename_node(state: &mut ViaState, old: String, new: String) -> Result<()> {
        let mut node = state
            .node_by_slug(&old)
            .await?
            .ok_or_else(|| anyhow!("unknown node '{old}'"))?;
        let new_slug = normalize_slug(&new)?;
        if state.node_by_slug(&new_slug).await?.is_some() {
            bail!("node '{new_slug}' already exists");
        }
        state.delete_node_slug(&old).await?;
        node.slug = new_slug.clone();
        state.upsert_node(&node).await?;
        state.append_event("node.renamed", &node).await?;
        sync_all(state).await?;
        println!("Renamed node '{old}' to '{new_slug}'.");
        Ok(())
    }

    pub async fn set_node_addr(
        state: &mut ViaState,
        node_slug: String,
        address: String,
    ) -> Result<()> {
        let mut node = state
            .node_by_slug(&node_slug)
            .await?
            .ok_or_else(|| anyhow!("unknown node '{node_slug}'"))?;
        node.addresses = vec![address.clone()];
        node.daemon_addr = if address.rsplit_once(':').is_some() {
            address
        } else {
            format!("{address}:47819")
        };
        state.upsert_node(&node).await?;
        state.append_event("node.addr_changed", &node).await?;
        sync_all(state).await?;
        println!(
            "Updated node '{}' address to {}.",
            node.slug, node.daemon_addr
        );
        Ok(())
    }

    pub async fn node_ping(state: &ViaState, node_slug: String, route: RouteMode) -> Result<()> {
        let node = state
            .node_by_slug(&node_slug)
            .await?
            .ok_or_else(|| anyhow!("unknown node '{node_slug}'"))?;
        if state.local_node().await?.id == node.id {
            println!("Node '{}' is local.", node.slug);
            return Ok(());
        }
        crate::transport::call_node(state, &node, crate::rpc::RpcRequest::Ping, route).await?;
        println!("Node '{}' is reachable via {:?}.", node.slug, route);
        Ok(())
    }

    pub async fn node_rm(state: &mut ViaState, node_slug: String) -> Result<()> {
        let node = state
            .node_by_slug(&node_slug)
            .await?
            .ok_or_else(|| anyhow!("unknown node '{node_slug}'"))?;
        if state.local_node().await?.id == node.id {
            bail!("cannot remove the local node from itself");
        }
        let services = state.services().await?;
        if services.iter().any(|service| service.node_id == node.id) {
            bail!("node '{}' still has services; remove them first", node.slug);
        }
        state.delete_node_slug(&node.slug).await?;
        state.append_event("node.removed", &node).await?;
        state.persist().await?;
        sync_all(state).await?;
        println!("Removed node '{}'.", node.slug);
        Ok(())
    }

    pub struct DeployArgs {
        pub target: String,
        pub to: String,
        pub name: String,
        pub port: Option<String>,
        pub route: RouteMode,
        pub command: Vec<String>,
    }

    pub async fn deploy(state: &mut ViaState, paths: &ViaPaths, args: DeployArgs) -> Result<()> {
        let DeployArgs {
            target,
            to,
            name,
            port,
            route,
            command,
        } = args;
        let node = state
            .node_by_slug(&to)
            .await?
            .ok_or_else(|| anyhow!("unknown node '{to}'"))?;
        let service_name = normalize_slug(&name)?;
        if state.service_by_name(&service_name).await?.is_some() {
            bail!("service '{service_name}' already exists");
        }

        let container = format!("via-{}", service_name);
        let target_path = PathBuf::from(&target);
        let env = decrypted_secrets(state, paths).await?;
        let mut service = if target_path.exists() {
            if node.last_seen_at.is_some() {
                docker::deploy_path(&node, &target_path, &container, port, &env, &command).await?
            } else {
                bail!("remote path deploy is not supported in v0.1.0; build and push an image, then deploy the image")
            }
        } else if node.last_seen_at.is_some() {
            docker::deploy_image(&node, &target, &container, port, &env, &command).await?
        } else {
            match crate::transport::call_node(
                state,
                &node,
                crate::rpc::RpcRequest::DeployImage {
                    image: target,
                    service: service_name.clone(),
                    container,
                    port,
                    env,
                    command,
                },
                route,
            )
            .await?
            {
                crate::rpc::RpcResponse::Service { service } => service,
                other => bail!("unexpected deploy response: {other:?}"),
            }
        };
        service.node_addr = node.daemon_addr.clone();
        state.upsert_service(&service).await?;
        state.append_event("service.started", &service).await?;
        sync_all(state).await?;
        println!("Deployed service '{}' to '{}'.", service.name, node.slug);
        Ok(())
    }

    pub async fn ps(state: &ViaState) -> Result<()> {
        let services = state.services().await?;
        if services.is_empty() {
            println!("No services.");
            return Ok(());
        }
        println!(
            "{:<18} {:<18} {:<12} {:<16} TARGET",
            "NAME", "NODE", "ACTUAL", "PORT"
        );
        for service in services {
            let service = resolve_service_node_addr(state, service).await?;
            let local = state.local_node().await?.id == service.node_id;
            let actual = container_status(state, &service, local)
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            println!(
                "{:<18} {:<18} {:<12} {:<16} {}",
                service.name,
                service.node_slug,
                actual,
                service.port.clone().unwrap_or_else(|| "-".to_string()),
                service.target
            );
        }
        Ok(())
    }

    pub async fn services(state: &ViaState) -> Result<()> {
        let services = state.services().await?;
        if services.is_empty() {
            println!("No services.");
            return Ok(());
        }
        println!("{:<18} {:<18} {:<10} TARGET", "NAME", "NODE", "STATUS");
        for service in services {
            println!(
                "{:<18} {:<18} {:<10} {}",
                service.name,
                service.node_slug,
                service.status.as_str(),
                service.target
            );
        }
        Ok(())
    }

    pub async fn status(state: &ViaState, service: String) -> Result<()> {
        let service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        println!("name: {}", service.name);
        println!("node: {}", service.node_slug);
        println!("status: {}", service.status.as_str());
        println!("target: {}", service.target);
        println!("container: {}", service.container);
        if let Some(port) = service.port {
            println!("port: {port}");
        }
        println!("private: {}", service.published_private);
        Ok(())
    }

    pub async fn logs(
        state: &ViaState,
        service: Option<String>,
        follow: bool,
        limit: usize,
    ) -> Result<()> {
        let Some(service) = service else {
            if follow {
                bail!("system log follow is not supported; omit --follow");
            }
            let events = state.events(limit).await?;
            if events.is_empty() {
                println!("No Via events.");
                return Ok(());
            }
            println!("{:<25} {:<26} PAYLOAD", "TIME", "KIND");
            for event in events {
                println!(
                    "{:<25} {:<26} {}",
                    format_ts(event.created_at),
                    event.kind,
                    event.payload
                );
            }
            return Ok(());
        };
        let service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        let local = state.local_node().await?.id == service.node_id;
        let service = resolve_service_node_addr(state, service).await?;
        if local {
            return docker::logs(&service, true, follow).await;
        }
        let node = state
            .node_by_id(&service.node_id)
            .await?
            .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
        match crate::transport::call_node(
            state,
            &node,
            crate::rpc::RpcRequest::Logs {
                container: service.container,
                follow,
            },
            RouteMode::Auto,
        )
        .await?
        {
            crate::rpc::RpcResponse::Logs { output } => {
                print!("{output}");
                Ok(())
            }
            other => bail!("unexpected logs response: {other:?}"),
        }
    }

    pub async fn stop(state: &mut ViaState, service: String) -> Result<()> {
        let mut service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        let local = state.local_node().await?.id == service.node_id;
        service = resolve_service_node_addr(state, service).await?;
        if local {
            docker::stop(&service, true).await?;
        } else {
            let node = state
                .node_by_id(&service.node_id)
                .await?
                .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
            crate::transport::call_node(
                state,
                &node,
                crate::rpc::RpcRequest::Stop {
                    container: service.container.clone(),
                },
                RouteMode::Auto,
            )
            .await?;
        }
        service.status = ServiceStatus::Stopped;
        service.updated_at = now_ts();
        state.upsert_service(&service).await?;
        state.append_event("service.stopped", &service).await?;
        sync_all(state).await?;
        println!("Stopped service '{}'.", service.name);
        Ok(())
    }

    pub async fn restart(state: &mut ViaState, service: String) -> Result<()> {
        let mut service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        let local = state.local_node().await?.id == service.node_id;
        service = resolve_service_node_addr(state, service).await?;
        if local {
            docker::restart(&service, true).await?;
        } else {
            let node = state
                .node_by_id(&service.node_id)
                .await?
                .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
            crate::transport::call_node(
                state,
                &node,
                crate::rpc::RpcRequest::Restart {
                    container: service.container.clone(),
                },
                RouteMode::Auto,
            )
            .await?;
        }
        service.status = ServiceStatus::Running;
        service.updated_at = now_ts();
        state.upsert_service(&service).await?;
        state.append_event("service.started", &service).await?;
        sync_all(state).await?;
        println!("Restarted service '{}'.", service.name);
        Ok(())
    }

    pub async fn rm(state: &mut ViaState, service: String) -> Result<()> {
        let service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        let local = state.local_node().await?.id == service.node_id;
        let service = resolve_service_node_addr(state, service).await?;
        if local {
            docker::local_remove(&service.container)?;
        } else {
            let node = state
                .node_by_id(&service.node_id)
                .await?
                .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
            crate::transport::call_node(
                state,
                &node,
                crate::rpc::RpcRequest::Remove {
                    container: service.container.clone(),
                },
                RouteMode::Auto,
            )
            .await?;
        }
        state.delete_service(&service.name).await?;
        state.append_event("service.removed", &service).await?;
        state.persist().await?;
        sync_all(state).await?;
        println!("Removed service '{}'.", service.name);
        Ok(())
    }

    pub async fn exec(
        state: &mut ViaState,
        node: String,
        route: RouteMode,
        command: Vec<String>,
    ) -> Result<()> {
        let node = state
            .node_by_slug(&node)
            .await?
            .ok_or_else(|| anyhow!("unknown node '{node}'"))?;
        let local = state.local_node().await?.id == node.id;
        let output = if local {
            crate::util::run_command_capture(&command)?
        } else {
            match crate::transport::call_node(
                state,
                &node,
                crate::rpc::RpcRequest::Exec {
                    command: command.clone(),
                },
                route,
            )
            .await?
            {
                crate::rpc::RpcResponse::Exec { output } => output,
                other => bail!("unexpected exec response: {other:?}"),
            }
        };
        state
            .append_event(
                "node.exec",
                &serde_json::json!({
                    "node": node.slug,
                    "argc": command.len(),
                    "local": local
                }),
            )
            .await?;
        state.persist().await?;
        sync_all(state).await?;
        print!("{output}");
        Ok(())
    }

    pub async fn proxy(
        state: &ViaState,
        service: String,
        listen: String,
        route: RouteMode,
    ) -> Result<()> {
        let service = match state.service_by_name(&service).await? {
            Some(service) => service,
            None => service_from_via_url(state, &service)
                .await?
                .ok_or_else(|| anyhow!("unknown service"))?,
        };
        crate::transport::proxy_service(state, &service, &listen, route).await
    }

    pub async fn open(state: &ViaState, service: String) -> Result<()> {
        let service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        let port = service
            .port
            .ok_or_else(|| anyhow!("service '{}' has no published port", service.name))?;
        let host_port = host_port(&port);
        let node = state
            .node_by_id(&service.node_id)
            .await?
            .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
        let host = if state.local_node().await?.id == node.id {
            "127.0.0.1".to_string()
        } else {
            node.daemon_addr
                .rsplit_once(':')
                .map(|(host, _)| host.to_string())
                .unwrap_or(node.slug)
        };
        println!("http://{}:{}", host, host_port);
        Ok(())
    }

    pub async fn update(
        state: &mut ViaState,
        check: bool,
        all: bool,
        node: Option<String>,
        version: Option<String>,
    ) -> Result<()> {
        if all && node.is_some() {
            bail!("use either --all or --node, not both");
        }

        let latest = match version {
            Some(version) => normalize_release_version(&version)?,
            None => latest_release_version()?,
        };

        if check {
            print_update_check(&latest);
            return Ok(());
        }

        let scope = if all {
            "all"
        } else if node.is_some() {
            "node"
        } else {
            "local"
        };

        if all {
            let local_node = state.local_node().await?;
            for node in state.nodes().await? {
                update_node(state, &node, node.id == local_node.id, &latest).await?;
            }
        } else if let Some(node_slug) = node {
            let node = state
                .node_by_slug(&node_slug)
                .await?
                .ok_or_else(|| anyhow!("unknown node '{node_slug}'"))?;
            let local = state.local_node().await?.id == node.id;
            update_node(state, &node, local, &latest).await?;
        } else {
            update_local(&latest)?;
        }

        if state.mesh().await?.is_some() {
            state
                .append_event(
                    "via.updated",
                    &serde_json::json!({
                        "version": latest,
                        "scope": scope
                    }),
                )
                .await?;
            state.persist().await?;
            sync_all(state).await?;
        }
        println!("Via binaries updated. Restart running daemons to use the new binary.");
        Ok(())
    }

    fn print_update_check(latest: &str) {
        let current = env!("CARGO_PKG_VERSION");
        println!("current: {current}");
        println!("latest:  {latest}");
        if version_newer(latest, current) {
            println!("update:  available");
        } else {
            println!("update:  not needed");
        }
    }

    async fn update_node(state: &ViaState, node: &Node, local: bool, version: &str) -> Result<()> {
        println!("Updating '{}' to {}...", node.slug, version);
        let output = if local {
            run_update_installer(version)?
        } else {
            match crate::transport::call_node(
                state,
                node,
                crate::rpc::RpcRequest::Exec {
                    command: vec![
                        "sh".to_string(),
                        "-lc".to_string(),
                        update_shell_command(version),
                    ],
                },
                RouteMode::Auto,
            )
            .await?
            {
                crate::rpc::RpcResponse::Exec { output } => output,
                other => bail!("unexpected update response: {other:?}"),
            }
        };
        print!("{output}");
        Ok(())
    }

    fn update_local(version: &str) -> Result<()> {
        println!("Updating local Via binary to {}...", version);
        let output = run_update_installer(version)?;
        print!("{output}");
        Ok(())
    }

    fn run_update_installer(version: &str) -> Result<String> {
        crate::util::run_command_capture(&[
            "sh".to_string(),
            "-lc".to_string(),
            update_shell_command(version),
        ])
    }

    fn update_shell_command(version: &str) -> String {
        format!(
            "if command -v curl >/dev/null 2>&1; then curl -fsSL {INSTALL_URL} | bash -s -- {version}; elif command -v wget >/dev/null 2>&1; then wget -q {INSTALL_URL} -O - | bash -s -- {version}; else echo 'update needs curl or wget' >&2; exit 1; fi"
        )
    }

    fn latest_release_version() -> Result<String> {
        if let Ok(version) = std::env::var("VIA_UPDATE_VERSION") {
            return normalize_release_version(&version);
        }
        let repo = std::env::var("VIA_UPDATE_REPO").unwrap_or_else(|_| UPDATE_REPO.to_string());
        let url = format!("https://api.github.com/repos/{repo}/releases/latest");
        let body = download_stdout(&url)?;
        parse_latest_release_version(&body)
            .ok_or_else(|| anyhow!("could not find latest Via release at {url}"))
    }

    fn download_stdout(url: &str) -> Result<String> {
        let output = if command_exists("curl") {
            ProcessCommand::new("curl").args(["-fsSL", url]).output()
        } else if command_exists("wget") {
            ProcessCommand::new("wget")
                .args(["-q", url, "-O", "-"])
                .output()
        } else {
            bail!("update needs curl or wget");
        }
        .context("failed to run release lookup")?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("release lookup failed: {stderr}");
        }
        Ok(stdout)
    }

    fn command_exists(command: &str) -> bool {
        ProcessCommand::new("sh")
            .arg("-lc")
            .arg(format!("command -v {command} >/dev/null 2>&1"))
            .status()
            .is_ok_and(|status| status.success())
    }

    fn parse_latest_release_version(body: &str) -> Option<String> {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        value
            .get("tag_name")?
            .as_str()
            .map(|tag| tag.trim_start_matches('v').to_string())
    }

    fn normalize_release_version(version: &str) -> Result<String> {
        let version = version.trim().trim_start_matches('v');
        if version.is_empty()
            || !version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            bail!("invalid Via release version '{version}'");
        }
        Ok(version.to_string())
    }

    fn version_newer(latest: &str, current: &str) -> bool {
        version_parts(latest) > version_parts(current)
    }

    fn version_parts(version: &str) -> Vec<u64> {
        version
            .trim_start_matches('v')
            .split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    }

    pub async fn publish(state: &mut ViaState, service: String, private: bool) -> Result<()> {
        if !private {
            bail!("V1 only supports private publishing; pass --private");
        }
        let mut service = state
            .service_by_name(&service)
            .await?
            .ok_or_else(|| anyhow!("unknown service"))?;
        service.published_private = true;
        service.updated_at = now_ts();
        state.upsert_service(&service).await?;
        state
            .append_event("service.published_private", &service)
            .await?;
        sync_all(state).await?;
        println!(
            "Published '{}' privately inside the Via mesh.",
            service.name
        );
        Ok(())
    }

    pub async fn secret_set(
        state: &mut ViaState,
        paths: &ViaPaths,
        name: String,
        value: String,
    ) -> Result<()> {
        state
            .mesh()
            .await?
            .ok_or_else(|| anyhow!("run `via init` before setting secrets"))?;
        let name = normalize_secret_name(&name)?;
        let key = crate::security::ensure_mesh_key(paths)?;
        let existing = state.secret_by_name(&name).await?;
        let now = now_ts();
        let secret = crate::model::Secret {
            name: name.clone(),
            ciphertext: crate::security::encrypt_string(&key, &value)?,
            created_at: existing
                .as_ref()
                .map(|secret| secret.created_at)
                .unwrap_or(now),
            updated_at: now,
        };
        state.upsert_secret(&secret).await?;
        state
            .append_event("secret.set", &serde_json::json!({ "name": name }))
            .await?;
        state.persist().await?;
        sync_all(state).await?;
        println!("Set secret '{}'.", name);
        Ok(())
    }

    pub async fn secret_delete(state: &mut ViaState, name: String) -> Result<()> {
        state
            .mesh()
            .await?
            .ok_or_else(|| anyhow!("run `via init` before deleting secrets"))?;
        let name = normalize_secret_name(&name)?;
        if state.secret_by_name(&name).await?.is_none() {
            bail!("unknown secret '{name}'");
        }
        state.delete_secret(&name).await?;
        state.append_event("secret.deleted", &name).await?;
        state.persist().await?;
        sync_all(state).await?;
        println!("Deleted secret '{}'.", name);
        Ok(())
    }

    pub async fn secret_list(state: &ViaState) -> Result<()> {
        let secrets = state.secrets().await?;
        if secrets.is_empty() {
            println!("No secrets.");
            return Ok(());
        }
        println!("{:<24} UPDATED", "NAME");
        for secret in secrets {
            println!("{:<24} {}", secret.name, format_ts(secret.updated_at));
        }
        Ok(())
    }

    async fn sync_all(state: &ViaState) -> Result<()> {
        refresh_local_node_info(state).await;
        let snapshot = state.snapshot().await?;
        for node in snapshot.nodes.iter() {
            if node.last_seen_at.is_some() {
                continue;
            }
            if let Err(error) = crate::transport::call_node(
                state,
                node,
                crate::rpc::RpcRequest::ImportSnapshot {
                    snapshot: snapshot.clone(),
                },
                RouteMode::Auto,
            )
            .await
            {
                eprintln!("warning: failed to sync node '{}': {error}", node.slug);
            }
        }
        Ok(())
    }

    async fn refresh_local_node_info(state: &ViaState) {
        let Ok(mut local) = state.local_node().await else {
            return;
        };
        let Ok(crate::rpc::RpcResponse::NodeInfo { iroh_addr }) =
            crate::rpc::call(&local.daemon_addr, crate::rpc::RpcRequest::NodeInfo).await
        else {
            return;
        };
        if iroh_addr.is_some() && local.iroh_addr != iroh_addr {
            local.iroh_addr = iroh_addr;
            if let Err(error) = state.upsert_node(&local).await {
                eprintln!("warning: failed to refresh local node iroh address: {error}");
                return;
            }
            if let Err(error) = state.persist().await {
                eprintln!("warning: failed to persist local node iroh address: {error}");
            }
        }
    }

    async fn resolve_service_node_addr(
        state: &ViaState,
        mut service: crate::model::Service,
    ) -> Result<crate::model::Service> {
        let node = state
            .node_by_id(&service.node_id)
            .await?
            .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
        service.node_addr = node.daemon_addr;
        Ok(service)
    }

    async fn service_from_via_url(
        state: &ViaState,
        value: &str,
    ) -> Result<Option<crate::model::Service>> {
        let Some(rest) = value.strip_prefix("via://") else {
            return Ok(None);
        };
        let Some((_node, service)) = rest.split_once('/') else {
            bail!("via URLs must look like via://node/service");
        };
        state.service_by_name(service).await
    }

    async fn container_status(
        state: &ViaState,
        service: &crate::model::Service,
        local: bool,
    ) -> Result<String> {
        if local {
            return docker::local_container_status(&service.container);
        }
        let node = state
            .node_by_id(&service.node_id)
            .await?
            .ok_or_else(|| anyhow!("service node '{}' is missing", service.node_slug))?;
        match crate::transport::call_node(
            state,
            &node,
            crate::rpc::RpcRequest::ContainerStatus {
                container: service.container.clone(),
            },
            RouteMode::Auto,
        )
        .await?
        {
            crate::rpc::RpcResponse::ContainerStatus { status } => Ok(status),
            other => bail!("unexpected status response: {other:?}"),
        }
    }

    async fn decrypted_secrets(
        state: &ViaState,
        paths: &ViaPaths,
    ) -> Result<Vec<(String, String)>> {
        let key = crate::security::read_mesh_key(paths)?;
        let mut env = Vec::new();
        for secret in state.secrets().await? {
            env.push((
                secret.name,
                crate::security::decrypt_string(&key, &secret.ciphertext)?,
            ));
        }
        Ok(env)
    }

    fn normalize_secret_name(name: &str) -> Result<String> {
        Ok(normalize_slug(name)?.replace('-', "_").to_ascii_uppercase())
    }

    fn host_port(port: &str) -> &str {
        port.split(':').next().unwrap_or(port)
    }

    fn hostname() -> Result<String> {
        let output = ProcessCommand::new("hostname")
            .output()
            .context("failed to read hostname")?;
        if !output.status.success() {
            bail!("hostname command failed");
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::{
            normalize_release_version, parse_latest_release_version, update_shell_command,
            version_newer,
        };

        #[test]
        fn parses_latest_release_version() {
            let body = r#"{"tag_name":"v0.1.2"}"#;
            assert_eq!(parse_latest_release_version(body).as_deref(), Some("0.1.2"));
        }

        #[test]
        fn compares_versions() {
            assert!(version_newer("0.1.1", "0.1.0"));
            assert!(version_newer("v0.2.0", "0.1.9"));
            assert!(!version_newer("0.1.0", "0.1.0"));
            assert!(!version_newer("0.1.0", "0.1.1"));
        }

        #[test]
        fn update_shell_command_pins_version() {
            let command = update_shell_command("0.1.0");
            assert!(command.contains("bash -s -- 0.1.0"));
            assert!(command.contains("curl"));
            assert!(command.contains("wget"));
        }

        #[test]
        fn release_versions_are_shell_safe() {
            assert_eq!(normalize_release_version("v0.1.0").unwrap(), "0.1.0");
            assert_eq!(
                normalize_release_version("0.1.0-rc.1").unwrap(),
                "0.1.0-rc.1"
            );
            assert!(normalize_release_version("0.1.0; echo no").is_err());
            assert!(normalize_release_version("").is_err());
        }
    }
}
