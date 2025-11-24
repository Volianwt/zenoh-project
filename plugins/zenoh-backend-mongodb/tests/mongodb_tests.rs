use std::str::FromStr;

use testcontainers::{core::WaitFor, runners::SyncRunner, GenericImage};
use tokio::runtime::Runtime;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh_backend_traits::config::{GarbageCollectionConfig, StorageConfig, VolumeConfig};
use zenoh_backend_traits::{StorageInsertionResult, StoredData};
use zenoh_backend_mongodb::MongoDbBackend;
use zenoh_plugin_trait::Plugin;

fn start_mongo() -> (testcontainers::Container<GenericImage>, String) {
    let image = GenericImage::new("mongo", "7.0.8")
        .with_wait_for(WaitFor::message_on_stdout("Waiting for connections"))
        .with_exposed_port(27017);
    let container = image.start();
    let port = container.get_host_port_ipv4(27017);
    let uri = format!("mongodb://127.0.0.1:{port}");
    (container, uri)
}

fn build_volume_config(uri: &str) -> VolumeConfig {
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.into());
    rest.insert("database".into(), "zenoh_test_db".into());

    VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    }
}

fn build_storage_config(volume_id: &str) -> StorageConfig {
    StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: volume_id.into(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_backend_tests" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    }
}

#[test]
fn mongo_backend_round_trip() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_round_trip: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");

    let volume_config = build_volume_config(&uri);
    let storage_config = build_storage_config(&volume_config.name);

    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_config).await })
        .expect("storage should be created");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/test-key").unwrap());
    let payload: ZBytes = "{'sensor_id':123,'temp':25.5}".into();
    let encoding = Encoding::from("text/plain");
    let timestamp = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a")
        .expect("timestamp parses");

    runtime.block_on(async {
        let result = storage
            .put(key.clone(), payload.clone(), encoding.clone(), timestamp)
            .await
            .expect("put should succeed");
        assert!(matches!(result, StorageInsertionResult::Inserted));
    });

    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .expect("get should succeed");

    assert_eq!(fetched.len(), 1);
    let stored = &fetched[0];
    assert_eq!(stored.encoding, encoding);
    assert_eq!(stored.payload, payload);
}

#[test]
fn mongo_backend_delete_removes_data() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_delete_removes_data: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");

    let volume_config = build_volume_config(&uri);
    let storage_config = build_storage_config(&volume_config.name);

    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_config).await })
        .expect("storage should be created");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/delete-key").unwrap());
    let payload: ZBytes = "to be deleted".into();
    let encoding = Encoding::from("text/plain");
    let timestamp = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a")
        .expect("timestamp parses");

    runtime.block_on(async {
        let inserted = storage
            .put(key.clone(), payload.clone(), encoding.clone(), timestamp)
            .await
            .expect("put should succeed");
        assert!(matches!(inserted, StorageInsertionResult::Inserted));

        let deleted = storage
            .delete(key.clone(), timestamp)
            .await
            .expect("delete should succeed");
        // If this ever stops returning Deleted, we risk leaving stale docs behind.
        assert!(matches!(deleted, StorageInsertionResult::Deleted));
    });

    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .expect("get should succeed");
    // Confirm the document is actually gone, not just marked.
    assert!(fetched.is_empty());
}

#[test]
fn mongo_backend_round_trip_non_utf8_payload() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_round_trip_non_utf8_payload: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");

    let volume_config = build_volume_config(&uri);
    let storage_config = build_storage_config(&volume_config.name);

    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_config).await })
        .expect("storage should be created");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/non-utf8").unwrap());
    let payload: ZBytes = vec![0xFF, 0xFE, 0xFD].into(); // Intentionally invalid UTF-8 bytes.
    let encoding = Encoding::from("application/octet-stream");
    let timestamp = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a")
        .expect("timestamp parses");

    runtime.block_on(async {
        let result = storage
            .put(key.clone(), payload.clone(), encoding.clone(), timestamp)
            .await
            .expect("put should succeed");
        assert!(matches!(result, StorageInsertionResult::Inserted));
    });

    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .expect("get should succeed");

    assert_eq!(fetched.len(), 1);
    let stored = &fetched[0];
    // Even without value_text (because bytes are non-UTF8), the raw bytes must round-trip intact.
    assert_eq!(stored.payload, payload);
    assert_eq!(stored.encoding, encoding);
}

#[test]
fn mongo_backend_rejects_older_timestamp() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_rejects_older_timestamp: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");

    let volume_config = build_volume_config(&uri);
    let storage_config = build_storage_config(&volume_config.name);

    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_config).await })
        .expect("storage should be created");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/last-write-wins").unwrap());
    let newer_payload: ZBytes = "newer-payload".into();
    let older_payload: ZBytes = "older-payload".into();
    let encoding = Encoding::from("text/plain");
    // Use timestamps with different logical times; newer_ts > older_ts.
    let newer_ts = Timestamp::from_str("7054123832858541151/BC779A06D7E049BD88C3FF3DB0C17FCC")
        .expect("timestamp parses");
    let older_ts = Timestamp::from_str("7054123566570568799/BC779A06D7E049BD88C3FF3DB0C17FCC")
        .expect("timestamp parses");

    runtime.block_on(async {
        let first = storage
            .put(key.clone(), newer_payload.clone(), encoding.clone(), newer_ts)
            .await
            .expect("first put should succeed");
        assert!(matches!(first, StorageInsertionResult::Inserted));

        let second = storage
            .put(key.clone(), older_payload.clone(), encoding.clone(), older_ts)
            .await
            .expect("second put should succeed");

        // Expect an older timestamp to be rejected; if this returns Replaced the backend is violating last-write-wins.
        assert!(
            matches!(second, StorageInsertionResult::Outdated),
            "older timestamp must not overwrite newer data"
        );
    });

    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .expect("get should succeed");

    assert_eq!(fetched.len(), 1);
    let stored = &fetched[0];
    // The newer payload should survive if older writes are properly rejected.
    assert_eq!(stored.payload, newer_payload);
    assert_eq!(stored.encoding, encoding);
}

#[test]
fn mongo_backend_put_is_idempotent() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_put_is_idempotent: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");

    let volume_config = build_volume_config(&uri);
    let storage_config = build_storage_config(&volume_config.name);

    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_config).await })
        .expect("storage should be created");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/idempotent-key").unwrap());
    let payload: ZBytes = "{'sensor_id':123,'temp':25.5}".into();
    let encoding = Encoding::from("text/plain");
    let timestamp = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a")
        .expect("timestamp parses");

    runtime.block_on(async {
        let first = storage
            .put(key.clone(), payload.clone(), encoding.clone(), timestamp)
            .await
            .expect("first put should succeed");
        assert!(matches!(first, StorageInsertionResult::Inserted));

        let second = storage
            .put(key.clone(), payload.clone(), encoding.clone(), timestamp)
            .await
            .expect("second put should succeed");

        // Expect the second put to not create a duplicate entry.
        assert!(
            matches!(second, StorageInsertionResult::Replaced | StorageInsertionResult::Outdated),
            "idempotent put should not return Inserted"
        );
    });

    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .expect("get should succeed");

    assert_eq!(
        fetched.len(),
        1,
        "idempotent put should not create duplicate records"
    );
    let stored = &fetched[0];
    assert_eq!(stored.encoding, encoding);
    assert_eq!(stored.payload, payload);
}
