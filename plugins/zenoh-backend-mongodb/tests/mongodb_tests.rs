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