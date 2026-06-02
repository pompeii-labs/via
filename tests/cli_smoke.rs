use std::process::Command;

#[test]
fn version_flag_reports_package_version() {
    let via = env!("CARGO_BIN_EXE_via");

    let output = Command::new(via).arg("--version").output().unwrap();

    assert!(
        output.status.success(),
        "version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("via {}", env!("CARGO_PKG_VERSION"))),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn update_check_reports_available_version() {
    let temp = tempfile::tempdir().unwrap();
    let via = env!("CARGO_BIN_EXE_via");

    let output = Command::new(via)
        .env("HOME", temp.path())
        .env("VIA_UPDATE_VERSION", "99.0.0")
        .args(["update", "--check"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "update check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("current: {}", env!("CARGO_PKG_VERSION"))),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("latest:  99.0.0"),
        "unexpected stdout: {stdout}"
    );
    assert!(stdout.contains("available"), "unexpected stdout: {stdout}");
}

#[test]
fn init_and_nodes_persist_state() {
    let temp = tempfile::tempdir().unwrap();
    let via = env!("CARGO_BIN_EXE_via");

    let init = Command::new(via)
        .env("HOME", temp.path())
        .args(["init", "--name", "laptop"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let nodes = Command::new(via)
        .env("HOME", temp.path())
        .arg("nodes")
        .output()
        .unwrap();
    assert!(
        nodes.status.success(),
        "nodes failed: {}",
        String::from_utf8_lossy(&nodes.stderr)
    );
    let stdout = String::from_utf8_lossy(&nodes.stdout);
    assert!(
        stdout.contains("laptop"),
        "unexpected nodes output: {stdout}"
    );
}

#[test]
fn secrets_are_encrypted_listed_and_deleted() {
    let temp = tempfile::tempdir().unwrap();
    let via = env!("CARGO_BIN_EXE_via");

    let init = Command::new(via)
        .env("HOME", temp.path())
        .args(["init", "--name", "laptop"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let set = Command::new(via)
        .env("HOME", temp.path())
        .args(["secret", "set", "api_key", "--value", "super-secret-value"])
        .output()
        .unwrap();
    assert!(
        set.status.success(),
        "secret set failed: {}",
        String::from_utf8_lossy(&set.stderr)
    );

    let lux = std::fs::read(temp.path().join(".via/lux/lux.dat")).unwrap();
    let lux = String::from_utf8_lossy(&lux);
    assert!(lux.contains("API_KEY"));
    assert!(!lux.contains("super-secret-value"));

    let list = Command::new(via)
        .env("HOME", temp.path())
        .args(["secret", "list"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "secret list failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("API_KEY"),
        "unexpected list output: {stdout}"
    );
    assert!(
        stdout.contains("UPDATED"),
        "unexpected list output: {stdout}"
    );
    assert!(!stdout.contains("super-secret-value"));

    let delete = Command::new(via)
        .env("HOME", temp.path())
        .args(["secret", "delete", "api_key"])
        .output()
        .unwrap();
    assert!(
        delete.status.success(),
        "secret delete failed: {}",
        String::from_utf8_lossy(&delete.stderr)
    );

    let list = Command::new(via)
        .env("HOME", temp.path())
        .args(["secret", "list"])
        .output()
        .unwrap();
    assert!(
        list.status.success(),
        "secret list after delete failed: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("No secrets."),
        "unexpected list output: {stdout}"
    );

    let logs = Command::new(via)
        .env("HOME", temp.path())
        .arg("logs")
        .output()
        .unwrap();
    assert!(
        logs.status.success(),
        "logs failed: {}",
        String::from_utf8_lossy(&logs.stderr)
    );
    let stdout = String::from_utf8_lossy(&logs.stdout);
    assert!(
        stdout.contains("secret.set"),
        "unexpected logs output: {stdout}"
    );
    assert!(
        stdout.contains("secret.deleted"),
        "unexpected logs output: {stdout}"
    );
    assert!(!stdout.contains("ciphertext"));
    assert!(!stdout.contains("super-secret-value"));
}

#[test]
fn secret_set_requires_initialized_mesh() {
    let temp = tempfile::tempdir().unwrap();
    let via = env!("CARGO_BIN_EXE_via");

    let set = Command::new(via)
        .env("HOME", temp.path())
        .args(["secret", "set", "api_key", "--value", "super-secret-value"])
        .output()
        .unwrap();

    assert!(!set.status.success());
    let stderr = String::from_utf8_lossy(&set.stderr);
    assert!(stderr.contains("via init"), "unexpected stderr: {stderr}");
}

#[test]
fn system_log_follow_is_explicitly_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let via = env!("CARGO_BIN_EXE_via");

    let init = Command::new(via)
        .env("HOME", temp.path())
        .args(["init", "--name", "laptop"])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    let logs = Command::new(via)
        .env("HOME", temp.path())
        .args(["logs", "--follow"])
        .output()
        .unwrap();

    assert!(!logs.status.success());
    let stderr = String::from_utf8_lossy(&logs.stderr);
    assert!(
        stderr.contains("system log follow"),
        "unexpected stderr: {stderr}"
    );
}
