use crate::paths::ViaPaths;
use anyhow::{bail, Context, Result};
use std::process::Command;

pub async fn bootstrap(
    host: &str,
    paths: &ViaPaths,
    slug: &str,
    mesh_id: &str,
    node_id: &str,
    local_binary: Option<String>,
) -> Result<()> {
    let remote_bin = "~/.via/bin/via";
    run(
        "ssh",
        &[
            host,
            "mkdir -p ~/.via/bin ~/.via/lux ~/.via/logs ~/.via/builds ~/.via/src",
        ],
    )?;
    run(
        "scp",
        &[
            paths
                .mesh_key
                .to_str()
                .context("mesh key path is not UTF-8")?,
            &format!("{host}:~/.via/mesh.key"),
        ],
    )?;
    if let Some(local_binary) = local_binary {
        let remote_tmp = "~/.via/bin/via.tmp";
        run("scp", &[&local_binary, &format!("{host}:{remote_tmp}")])?;
        run(
            "ssh",
            &[
                host,
                &format!(
                    "chmod +x {remote_tmp} && {remote_tmp} --help >/dev/null && mv {remote_tmp} {remote_bin}"
                ),
            ],
        )?;
    } else {
        let source_root = env!("CARGO_MANIFEST_DIR");
        let lux_root = std::path::Path::new(source_root)
            .parent()
            .and_then(std::path::Path::parent)
            .map(|root| root.join("Lux/lux"))
            .context("failed to locate local Lux checkout")?;
        let remote_src = "~/.via/src/Pompeii/via";
        let remote_lux = "~/.via/src/Lux/lux";
        copy_dir(host, source_root, remote_src)?;
        copy_dir(
            host,
            lux_root
                .to_str()
                .context("local Lux checkout path is not UTF-8")?,
            remote_lux,
        )?;
        let build_cmd = format!(
            r#"set -e
cd {remote_src}
if command -v cargo >/dev/null 2>&1; then
  cargo build --release
elif command -v docker >/dev/null 2>&1; then
  docker run --rm -v "$HOME/.via/src:/src" -w /src/Pompeii/via rust:1-bookworm cargo build --release
else
  echo "Via bootstrap needs Cargo or Docker on the remote node." >&2
  echo "Install Rust/Cargo, or install Docker so Via can build itself in a Rust container." >&2
  exit 127
fi
cp target/release/via {remote_bin}.next
chmod +x {remote_bin}.next
mv -f {remote_bin}.next {remote_bin}"#
        );
        run("ssh", &[host, &build_cmd])?;
    }
    run(
        "ssh",
        &[
            host,
            &format!(
                "chmod +x {remote_bin} && {remote_bin} init --name {slug} --mesh-id {mesh_id} --node-id {node_id} && (pkill -x via 2>/dev/null || true) && (nohup {remote_bin} daemon --bind 0.0.0.0:47819 > ~/.via/daemon.log 2>&1 & echo $! > ~/.via/daemon.pid)"
            ),
        ],
    )?;
    paths.ensure()?;
    Ok(())
}

pub fn remote(host: &str, command: &str) -> Result<()> {
    run("ssh", &[host, command])
}

pub fn resolved_hostname(host: &str) -> Result<String> {
    let output = Command::new("ssh")
        .args(["-G", host])
        .output()
        .with_context(|| format!("failed to resolve ssh host {host}"))?;
    if !output.status.success() {
        bail!("ssh -G failed for {host}");
    }
    let config = String::from_utf8_lossy(&output.stdout);
    for line in config.lines() {
        let mut parts = line.split_whitespace();
        if matches!(parts.next(), Some("hostname")) {
            if let Some(hostname) = parts.next() {
                return Ok(hostname.to_string());
            }
        }
    }
    Ok(host.to_string())
}

pub fn copy_dir(host: &str, local: &str, remote: &str) -> Result<()> {
    run("ssh", &[host, &format!("mkdir -p {remote}")])?;
    run(
        "rsync",
        &[
            "-az",
            "--delete",
            "--exclude",
            "target",
            "--exclude",
            ".git",
            &format!("{local}/"),
            &format!("{host}:{remote}/"),
        ],
    )
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    if !status.success() {
        bail!("{program} failed with status {status}");
    }
    Ok(())
}
