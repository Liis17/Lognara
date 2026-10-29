#![cfg(unix)]

#[test]
fn occupied_port_error_is_durable_before_core_exit() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lognara-core"))
        .env_clear()
        .env(
            "LOGNARA_LISTEN_ADDR",
            listener.local_addr().unwrap().to_string(),
        )
        .env("LOGNARA_DATA_DIR", dir.path())
        .env("LOGNARA_ADMIN_TOKEN", "test-admin")
        .env("LOGNARA_QUERY_TOKEN", "test-query")
        .env("LOGNARA_DISK_RESERVE_BYTES", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let queue = lognara_spool::Queue::open_limited(
        &dir.path().join("self-log-spool"),
        32 << 20,
        100_000,
        4096,
    )
    .unwrap();
    let records: Vec<lognara_selflog::Record> = queue
        .entries()
        .iter()
        .flat_map(|entry| {
            serde_json::from_slice::<Vec<lognara_selflog::Record>>(
                &queue.read(entry, lognara_selflog::MAX_BODY).unwrap(),
            )
            .unwrap()
        })
        .collect();
    assert!(
        records.iter().any(|record| record.service == "lognara-core"
            && record.level == "ERROR"
            && record.message == "core HTTP server failed"),
        "stdout: {:?}, stderr: {:?}",
        output.stdout,
        output.stderr
    );
}
