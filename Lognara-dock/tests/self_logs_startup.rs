#![cfg(unix)]

#[test]
fn failed_startup_flushes_its_diagnostic_before_exit() {
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lognara-dock"))
        .env_clear()
        .env("LOGNARA_DOCK_PUBLIC_URL", "http://localhost:7403")
        .env("LOGNARA_DOCK_CORE_URL", "http://127.0.0.1:9")
        .env("LOGNARA_DOCK_CORE_TOKEN", "test-query")
        .env("LOGNARA_DOCK_CORE_ADMIN_TOKEN", "test-admin")
        .env("LOGNARA_DOCK_DATA_DIR", dir.path())
        .env(
            "LOGNARA_DOCK_HTPASSWD_FILE",
            dir.path().join("missing-htpasswd"),
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let mut records = Vec::new();
    for group in std::fs::read_dir(dir.path().join("self-log-spool"))
        .unwrap()
        .flatten()
    {
        if group.path().extension().is_some_and(|ext| ext == "group") {
            for part in std::fs::read_dir(group.path()).unwrap().flatten() {
                records.extend(
                    serde_json::from_slice::<Vec<lognara_selflog::Record>>(
                        &std::fs::read(part.path()).unwrap(),
                    )
                    .unwrap(),
                );
            }
        }
    }
    assert!(
        records.iter().any(|record| record.service == "lognara-dock"
            && record.level == "ERROR"
            && record.message.contains("missing-htpasswd")),
        "stdout: {:?}, stderr: {:?}",
        output.stdout,
        output.stderr
    );
}
