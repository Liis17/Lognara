use lognara_core::wal::{Position, Wal};

#[test]
fn acknowledged_batches_survive_reopen_with_sequences_and_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let mut wal = Wal::open(dir.path(), 1, 1).unwrap();
    let a = wal.append(b"body-one", 3, 123).unwrap();
    let b = wal.append(b"body-two", 2, 456).unwrap();
    assert_eq!((a.batch_id, a.first_sequence, a.events), (1, 1, 3));
    assert_eq!((b.batch_id, b.first_sequence, b.events), (2, 4, 2));
    drop(wal);
    let mut wal = Wal::open(dir.path(), 1, 1).unwrap();
    assert_eq!(wal.read(&a).unwrap(), b"body-one");
    assert_eq!(wal.find(&a.hash).unwrap().received_at, 123);
    let c = wal.append(b"body-three", 1, 789).unwrap();
    assert_eq!((c.batch_id, c.first_sequence), (3, 6));
}

#[test]
fn incomplete_unpublished_record_is_removed_but_corruption_is_fatal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("00000000000000000001.tmp"), b"torn").unwrap();
    let mut wal = Wal::open(dir.path(), 1, 1).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    let entry = wal.append(b"durable", 1, 7).unwrap();
    drop(wal);
    let path = dir.path().join(format!("{:020}.wal", entry.batch_id));
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(Wal::open(dir.path(), 1, 1).is_err());
}

#[test]
fn partial_batch_checkpoint_never_removes_the_remaining_events() {
    let dir = tempfile::tempdir().unwrap();
    let mut wal = Wal::open(dir.path(), 1, 1).unwrap();
    let entry = wal.append(b"three", 3, 7).unwrap();
    wal.prune(Position {
        batch_id: 1,
        offset: 2,
    })
    .unwrap();
    assert_eq!(wal.read(&entry).unwrap(), b"three");
    wal.prune(Position {
        batch_id: 1,
        offset: 3,
    })
    .unwrap();
    assert_eq!(wal.bytes(), 0);
    assert!(wal.find(&entry.hash).is_none());
    let next = wal.append(b"next", 1, 8).unwrap();
    assert_eq!(next.first_sequence, 4);
}
