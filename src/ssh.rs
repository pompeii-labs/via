use crate::paths::ViaPaths;
use anyhow::{bail, Context, Result};
use std::process::Command;

pub async fn bootstrap(
    host: &str,
    paths: &ViaPaths,
    slug: &str,
    mesh_id: &str,
    node_id: &str,
) -> Result<()> {
    let remote_bin = "~/.via/bin/via";
    run("ssh", &[host, "mkdir -p ~/.via ~/.via/lux ~/.via/logs"])?;
    run(
        "ssh",
        &[host, &remote_install_command(env!("CARGO_PKG_VERSION"))],
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
    run("ssh", &[host, "chmod 600 ~/.via/mesh.key"])?;
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

fn remote_install_command(version: &str) -> String {
    format!(
        "if command -v curl >/dev/null 2>&1; then curl -fsSL https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh | bash -s -- {version}; elif command -v wget >/dev/null 2>&1; then wget -q https://raw.githubusercontent.com/pompeii-labs/via/main/install.sh -O - | bash -s -- {version}; else echo 'Via bootstrap needs curl or wget on the remote node.' >&2; exit 127; fi"
    )
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

#[cfg(test)]
mod tests {
    use super::remote_install_command;

    #[test]
    fn remote_install_command_uses_release_installer() {
        let command = remote_install_command("0.1.0");
        assert!(command.contains("raw.githubusercontent.com/pompeii-labs/via/main/install.sh"));
        assert!(command.contains("bash -s -- 0.1.0"));
        assert!(command.contains("curl"));
        assert!(command.contains("wget"));
        assert!(!command.contains("cargo"));
        assert!(!command.contains("rsync"));
    }
}
