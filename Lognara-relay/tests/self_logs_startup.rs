#![cfg(unix)]

#[test]
fn occupied_port_error_is_durable_before_exit() {
    let dir = tempfile::tempdir().unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_lognara-relay"))
        .env_clear()
        .env(
            "LOGNARA_LISTEN_ADDR",
            listener.local_addr().unwrap().to_string(),
        )
        .env("LOGNARA_CORE_URL", "http://127.0.0.1:9/v1/batches")
        .env("LOGNARA_CORE_TOKEN", "test-relay")
        .env("LOGNARA_SPOOL_DIR", dir.path())
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
        records
            .iter()
            .any(|record| record.service == "lognara-relay"
                && record.level == "ERROR"
                && record.message.contains("failed to listen")),
        "stdout: {:?}, stderr: {:?}",
        output.stdout,
        output.stderr
    );
}
