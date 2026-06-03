use crate::paths::ViaPaths;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const API_KEY_ENV: &str = "VIA_API_KEY";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthConfig {
    pub api_key: String,
}

pub fn load(paths: &ViaPaths) -> Result<Option<AuthConfig>> {
    if !paths.auth_config.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&paths.auth_config)
        .with_context(|| format!("failed to read {}", paths.auth_config.display()))?;
    Ok(Some(serde_json::from_str(&raw)?))
}

pub fn save(paths: &ViaPaths, config: &AuthConfig) -> Result<()> {
    paths.ensure()?;
    let raw = serde_json::to_string_pretty(config)?;
    write_owner_only(paths, &raw)?;
    Ok(())
}

pub fn resolve_api_key(paths: &ViaPaths) -> Result<Option<String>> {
    resolve_api_key_with_env(paths, |name| std::env::var(name).ok())
}

fn resolve_api_key_with_env<F>(paths: &ViaPaths, env: F) -> Result<Option<String>>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(api_key) = env(API_KEY_ENV)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return Ok(Some(api_key));
    }

    Ok(load(paths)?
        .map(|config| config.api_key.trim().to_string())
        .filter(|value| !value.is_empty()))
}

pub fn init(paths: &ViaPaths) -> Result<()> {
    use std::io::{self, Write};

    print!("Paste your API key: ");
    io::stdout().flush()?;

    let mut api_key = String::new();
    io::stdin().read_line(&mut api_key)?;
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err(anyhow!("API key cannot be empty"));
    }

    save(paths, &AuthConfig { api_key })?;
    println!("Saved API key to ~/.via/auth.json.");
    Ok(())
}

#[cfg(unix)]
fn write_owner_only(paths: &ViaPaths, raw: &str) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&paths.auth_config)
        .with_context(|| format!("failed to write {}", paths.auth_config.display()))?;
    file.write_all(raw.as_bytes())?;
    std::fs::set_permissions(&paths.auth_config, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_owner_only(paths: &ViaPaths, raw: &str) -> Result<()> {
    std::fs::write(&paths.auth_config, raw)
        .with_context(|| format!("failed to write {}", paths.auth_config.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, resolve_api_key_with_env, save, AuthConfig};
    use crate::paths::ViaPaths;
    use tempfile::TempDir;

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

    #[test]
    fn save_then_load_round_trip() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        let config = AuthConfig {
            api_key: "via_test_key".to_string(),
        };

        save(&paths, &config).unwrap();

        assert_eq!(load(&paths).unwrap(), Some(config));
    }

    #[cfg(unix)]
    #[test]
    fn auth_config_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);

        save(
            &paths,
            &AuthConfig {
                api_key: "via_test_key".to_string(),
            },
        )
        .unwrap();

        let mode = std::fs::metadata(&paths.auth_config)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn resolver_prefers_env_var_over_stored_config() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        save(
            &paths,
            &AuthConfig {
                api_key: "stored_key".to_string(),
            },
        )
        .unwrap();

        let resolved = resolve_api_key_with_env(&paths, |name| {
            (name == "VIA_API_KEY").then(|| "env_key".to_string())
        })
        .unwrap();

        assert_eq!(resolved.as_deref(), Some("env_key"));
    }

    #[test]
    fn resolver_uses_stored_config_when_env_unset() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);
        save(
            &paths,
            &AuthConfig {
                api_key: "stored_key".to_string(),
            },
        )
        .unwrap();

        let resolved = resolve_api_key_with_env(&paths, |_| None).unwrap();

        assert_eq!(resolved.as_deref(), Some("stored_key"));
    }

    #[test]
    fn resolver_returns_none_without_env_or_config() {
        let temp = TempDir::new().unwrap();
        let paths = temp_paths(&temp);

        let resolved = resolve_api_key_with_env(&paths, |_| None).unwrap();

        assert_eq!(resolved, None);
    }
}
