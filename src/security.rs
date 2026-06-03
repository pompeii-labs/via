use crate::paths::ViaPaths;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(unix)]
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
};

type HmacSha256 = Hmac<Sha256>;

pub fn ensure_mesh_key(paths: &ViaPaths) -> Result<Vec<u8>> {
    paths.ensure()?;
    if paths.mesh_key.exists() {
        harden_mesh_key_permissions(paths)?;
        return read_mesh_key(paths);
    }
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key)?;
    write_mesh_key(paths, &B64.encode(key))?;
    Ok(key.to_vec())
}

#[cfg(unix)]
fn harden_mesh_key_permissions(paths: &ViaPaths) -> Result<()> {
    fs::set_permissions(&paths.mesh_key, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn harden_mesh_key_permissions(_paths: &ViaPaths) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn write_mesh_key(paths: &ViaPaths, encoded: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&paths.mesh_key)
        .with_context(|| format!("failed to create {}", paths.mesh_key.display()))?;
    file.write_all(encoded.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_mesh_key(paths: &ViaPaths, encoded: &str) -> Result<()> {
    std::fs::write(&paths.mesh_key, encoded)?;
    Ok(())
}

pub fn read_mesh_key(paths: &ViaPaths) -> Result<Vec<u8>> {
    let encoded = std::fs::read_to_string(&paths.mesh_key)
        .with_context(|| format!("failed to read {}", paths.mesh_key.display()))?;
    let key = B64.decode(encoded.trim())?;
    if key.len() != 32 {
        bail!("invalid Via mesh key length");
    }
    Ok(key)
}

pub fn install_mesh_key(paths: &ViaPaths, encoded: &str) -> Result<Vec<u8>> {
    paths.ensure()?;
    let key = B64.decode(encoded.trim())?;
    if key.len() != 32 {
        bail!("invalid Via mesh key length");
    }
    if paths.mesh_key.exists() {
        harden_mesh_key_permissions(paths)?;
        let existing = read_mesh_key(paths)?;
        if existing != key {
            bail!("Via mesh key already exists and does not match invite");
        }
        return Ok(existing);
    }
    write_mesh_key(paths, encoded.trim())?;
    Ok(key)
}

pub fn mesh_key_if_present(paths: &ViaPaths) -> Result<Option<Vec<u8>>> {
    if paths.mesh_key.exists() {
        Ok(Some(read_mesh_key(paths)?))
    } else {
        Ok(None)
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn nonce() -> Result<String> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce)?;
    Ok(B64.encode(nonce))
}

pub fn sign(key: &[u8], payload: &[u8]) -> Result<String> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key)?;
    mac.update(payload);
    Ok(B64.encode(mac.finalize().into_bytes()))
}

pub fn verify(key: &[u8], payload: &[u8], signature: &str) -> Result<()> {
    let expected = B64.decode(signature)?;
    let mut mac = <HmacSha256 as Mac>::new_from_slice(key)?;
    mac.update(payload);
    mac.verify_slice(&expected)
        .map_err(|_| anyhow!("invalid RPC signature"))
}

#[allow(deprecated)]
pub fn encrypt_string(key: &[u8], plaintext: &str) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key)?;
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .map_err(|_| anyhow!("secret encryption failed"))?;
    let mut out = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(B64.encode(out))
}

#[allow(deprecated)]
pub fn decrypt_string(key: &[u8], encoded: &str) -> Result<String> {
    let data = B64.decode(encoded)?;
    if data.len() < 13 {
        bail!("encrypted secret is too short");
    }
    let (nonce_bytes, ciphertext) = data.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key)?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| anyhow!("secret decryption failed"))?;
    Ok(String::from_utf8(plaintext)?)
}

#[cfg(test)]
mod tests {
    use super::{decrypt_string, encrypt_string, ensure_mesh_key, sign, verify};
    use crate::paths::ViaPaths;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    #[test]
    fn hmac_signatures_verify() {
        let key = [7u8; 32];
        let payload = b"payload";
        let sig = sign(&key, payload).unwrap();
        verify(&key, payload, &sig).unwrap();
        assert!(verify(&key, b"other", &sig).is_err());
    }

    #[test]
    fn secrets_round_trip_encrypted() {
        let key = [3u8; 32];
        let encrypted = encrypt_string(&key, "secret-value").unwrap();
        assert!(!encrypted.contains("secret-value"));
        assert_eq!(decrypt_string(&key, &encrypted).unwrap(), "secret-value");
    }

    #[test]
    fn encrypted_secrets_reject_wrong_keys_and_tampering() {
        let key = [3u8; 32];
        let wrong_key = [4u8; 32];
        let encrypted = encrypt_string(&key, "secret-value").unwrap();
        assert!(decrypt_string(&wrong_key, &encrypted).is_err());

        let mut tampered = encrypted.into_bytes();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(decrypt_string(&key, &tampered).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn mesh_key_file_is_owner_only() {
        let temp = TempDir::new().unwrap();
        let paths = ViaPaths {
            root: temp.path().to_path_buf(),
            lux: temp.path().join("lux"),
            logs: temp.path().join("logs"),
            bin: temp.path().join("bin"),
            mesh_key: temp.path().join("mesh.key"),
            hub_config: temp.path().join("hub.json"),
            auth_config: temp.path().join("auth.json"),
        };

        ensure_mesh_key(&paths).unwrap();
        let mode = std::fs::metadata(paths.mesh_key)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}
