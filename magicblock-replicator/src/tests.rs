use std::{io::Write, path::Path, time::Duration};

use tempfile::TempDir;
use tokio::io::AsyncReadExt;

use crate::watcher::*;

#[tokio::test]
async fn test_watcher_detects_new_snapshot() {
    let temp_dir = TempDir::new().unwrap();
    let mut watcher = SnapshotWatcher::new(temp_dir.path()).unwrap();

    let test_data = b"test archive contents";
    let snapshot_path = temp_dir.path().join("snapshot-000000000001.tar.gz");
    std::fs::File::create(&snapshot_path)
        .unwrap()
        .write_all(test_data)
        .unwrap();

    let (mut file, slot) =
        tokio::time::timeout(Duration::from_secs(2), watcher.recv())
            .await
            .expect("Timeout waiting for snapshot")
            .expect("Channel closed");

    assert_eq!(slot, 1);
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, test_data);
}

#[tokio::test]
async fn test_watcher_ignores_non_snapshots() {
    let temp_dir = TempDir::new().unwrap();
    let mut watcher = SnapshotWatcher::new(temp_dir.path()).unwrap();

    let other_path = temp_dir.path().join("other.txt");
    std::fs::File::create(&other_path).unwrap();

    let test_data = b"test archive";
    let snapshot_path = temp_dir.path().join("snapshot-000000000002.tar.gz");
    std::fs::File::create(&snapshot_path)
        .unwrap()
        .write_all(test_data)
        .unwrap();

    let (mut file, slot) =
        tokio::time::timeout(Duration::from_secs(2), watcher.recv())
            .await
            .expect("Timeout waiting for snapshot")
            .expect("Channel closed");

    assert_eq!(slot, 2);
    let mut contents = Vec::new();
    file.read_to_end(&mut contents).await.unwrap();
    assert_eq!(contents, test_data);
}

#[test]
fn test_parse_slot() {
    assert_eq!(
        parse_slot(Path::new("snapshot-000000000001.tar.gz")),
        Some(1)
    );
    assert_eq!(
        parse_slot(Path::new("/some/path/snapshot-000000000123.tar.gz")),
        Some(123)
    );
    assert_eq!(parse_slot(Path::new("other.txt")), None);
    assert_eq!(parse_slot(Path::new("snapshot-invalid.tar.gz")), None);
}
