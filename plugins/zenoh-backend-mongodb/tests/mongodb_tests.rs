use std::{str::FromStr, sync::Arc};

use mongodb::{bson::doc, Client};
use testcontainers::{core::WaitFor, runners::SyncRunner, GenericImage};
use tokio::runtime::Runtime;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh_backend_traits::config::{GarbageCollectionConfig, StorageConfig, VolumeConfig};
use zenoh_backend_traits::{Storage, StorageInsertionResult, StoredData};
use zenoh_backend_mongodb::{metrics_snapshot, MongoDbBackend, MongoDbStorage, PageRequest};
use zenoh_plugin_trait::Plugin;

fn docker_available() -> bool {
    std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn start_mongo() -> (testcontainers::Container<GenericImage>, String) {
    let image = GenericImage::new("mongo", "7.0.8")
        .with_wait_for(WaitFor::message_on_stdout("Waiting for connections"))
        .with_exposed_port(27017);
    let container = image.start();
    let port = container.get_host_port_ipv4(27017);
    let uri = format!("mongodb://127.0.0.1:{port}");
    (container, uri)
}

fn create_storage_direct(
    uri: &str,
    database: &str,
    collection: &str,
) -> (Arc<Runtime>, mongodb::Database, MongoDbStorage) {
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let client = runtime
        .block_on(async { Client::with_uri_str(uri).await })
        .expect("connect mongo");
    let db = client.database(database);
    let storage = MongoDbStorage::new(&db, collection, runtime.clone());
    (runtime, db, storage)
}

#[test]
fn tc8_enumeration_correctness() {
    if !docker_available() {
        eprintln!("Skipping tc8_enumeration_correctness: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let (runtime, _db, mut storage) =
        create_storage_direct(&uri, "zenoh_tc8_db", "zenoh_tc8_coll");

    let base = "demo/mongo/list";
    let t1 = Timestamp::from_str("7054123000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    let t2 = Timestamp::from_str("7054124000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    let t3 = Timestamp::from_str("7054125000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    runtime.block_on(async {
        for (k, v, ts) in [
            (format!("{base}/a"), "v1", t1),
            (format!("{base}/b"), "v2", t2),
            (format!("{base}/c"), "v3", t3),
        ] {
            storage
                .put(
                    Some(OwnedKeyExpr::try_from(k.as_str()).unwrap()),
                    ZBytes::from(v),
                    Encoding::from("text/plain"),
                    ts,
                )
                .await
                .unwrap();
        }
    });

    let entries = runtime
        .block_on(async { storage.enumerate(base).await })
        .expect("enumerate succeeds");
    assert_eq!(entries.len(), 3);
    let ts_list: Vec<Timestamp> = entries.iter().map(|e| e.timestamp).collect();
    assert!(ts_list[0] > ts_list[1] && ts_list[1] > ts_list[2]);
}

#[test]
fn tc9_range_query() {
    if !docker_available() {
        eprintln!("Skipping tc9_range_query: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let (runtime, _db, mut storage) =
        create_storage_direct(&uri, "zenoh_tc9_db", "zenoh_tc9_coll");

    let base = "demo/mongo/range";
    let t1 = Timestamp::from_str("7054123000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    let t2 = Timestamp::from_str("7054124000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    let t3 = Timestamp::from_str("7054125000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    println!("\n[FR7/TC9] Inserting 3 entries under prefix '{base}' (t1<t2<t3)...");
    runtime.block_on(async {
        for (k, v, ts) in [
            (format!("{base}/a"), "v1", t1),
            (format!("{base}/b"), "v2", t2),
            (format!("{base}/c"), "v3", t3),
        ] {
            println!("  PUT key='{k}' ts='{ts}' payload='{v}'");
            storage
                .put(
                    Some(OwnedKeyExpr::try_from(k.as_str()).unwrap()),
                    ZBytes::from(v),
                    Encoding::from("text/plain"),
                    ts,
                )
                .await
                .unwrap();
        }
    });

    println!("[FR7/TC9] Querying range: from_ts='{t1}' to_ts='{t2}'");
    let entries = runtime
        .block_on(async { storage.enumerate_range(base, Some(t1), Some(t2)).await })
        .expect("range succeeds");
    println!("[FR7/TC9] Returned {} entries (sorted by timestamp desc):", entries.len());
    for (idx, e) in entries.iter().enumerate() {
        let key = e
            .key
            .as_ref()
            .map(|k| k.to_string())
            .unwrap_or_else(|| "<null>".to_string());
        println!("  [{idx}] key='{key}' ts='{}'", e.timestamp);
    }
    assert_eq!(entries.len(), 2);
    let keys: Vec<String> = entries
        .iter()
        .filter_map(|e| e.key.as_ref().map(|k| k.to_string()))
        .collect();
    assert!(keys.contains(&format!("{base}/a")));
    assert!(keys.contains(&format!("{base}/b")));
}

#[test]
fn tc10_pagination() {
    if !docker_available() {
        eprintln!("Skipping tc10_pagination: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let (runtime, _db, mut storage) =
        create_storage_direct(&uri, "zenoh_tc10_db", "zenoh_tc10_coll");

    let base = "demo/mongo/paging";
    println!("\n[FR8/TC10] Inserting 20 entries under prefix '{base}'...");
    for i in 0..20 {
        let ts = Timestamp::from_str(&format!(
            "7054123{}000000000/BC779A06D7E049BD88C3FF3DB0C17FCC",
            i + 10
        ))
        .unwrap();
        let key = Some(OwnedKeyExpr::try_from(format!("{base}/{i}").as_str()).unwrap());
        let payload: ZBytes = format!("v{i}").into();
        println!(
            "  PUT key='{}' ts='{ts}' payload='v{i}'",
            key.as_ref().unwrap()
        );
        runtime
            .block_on(async { storage.put(key, payload, Encoding::from("text/plain"), ts).await })
            .unwrap();
    }

    let page = PageRequest { limit: 5, offset: 5 };
    println!(
        "[FR8/TC10] Querying page: limit={} offset={}",
        page.limit, page.offset
    );
    let entries = runtime
        .block_on(async { storage.enumerate_paged(base, page).await })
        .unwrap();
    println!(
        "[FR8/TC10] Returned {} entries (sorted by timestamp desc):",
        entries.len()
    );
    for (idx, e) in entries.iter().enumerate() {
        let key = e
            .key
            .as_ref()
            .map(|k| k.to_string())
            .unwrap_or_else(|| "<null>".to_string());
        println!("  [{idx}] key='{key}' ts='{}'", e.timestamp);
    }
    assert_eq!(entries.len(), 5);
    assert!(entries.iter().all(|e| e.key.as_ref().unwrap().to_string().starts_with(base)));
}

#[test]
fn tc11_wal_logs_put() {
    if !docker_available() {
        eprintln!("Skipping tc11_wal_logs_put: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let (runtime, db, mut storage) =
        create_storage_direct(&uri, "zenoh_tc11_db", "zenoh_tc11_coll");

    let wal = db.collection::<mongodb::bson::Document>("wal_log");
    runtime
        .block_on(async { wal.delete_many(doc! {}, None).await })
        .expect("clear wal");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/wal-put").unwrap());
    let payload: ZBytes = "wal-payload".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7054123333333333333/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    runtime
        .block_on(async { storage.put(key.clone(), payload.clone(), encoding.clone(), ts).await })
        .unwrap();

    let wal_entry = runtime
        .block_on(async {
            wal.find_one(doc! { "op": "PUT", "key_expr": key.unwrap().to_string() }, None)
                .await
        })
        .expect("query wal");
    assert!(wal_entry.is_some(), "wal entry for PUT missing");
    let entry = wal_entry.unwrap();
    assert_eq!(entry.get_i64("payload_size").unwrap(), payload.len() as i64);
    assert_eq!(entry.get_str("encoding").unwrap(), "text/plain");
}

#[test]
fn tc12_wal_logs_delete() {
    if !docker_available() {
        eprintln!("Skipping tc12_wal_logs_delete: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let (runtime, db, mut storage) =
        create_storage_direct(&uri, "zenoh_tc12_db", "zenoh_tc12_coll");

    let wal = db.collection::<mongodb::bson::Document>("wal_log");
    runtime
        .block_on(async { wal.delete_many(doc! {}, None).await })
        .expect("clear wal");

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/wal-delete").unwrap());
    let payload: ZBytes = "wal-del".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7054123444444444444/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    runtime
        .block_on(async { storage.put(key.clone(), payload, encoding, ts).await })
        .unwrap();

    runtime
        .block_on(async { storage.delete(key.clone(), ts).await })
        .unwrap();

    let wal_entry = runtime
        .block_on(async {
            wal.find_one(doc! { "op": "DELETE", "key_expr": key.unwrap().to_string() }, None)
                .await
        })
        .expect("query wal");
    assert!(wal_entry.is_some(), "wal entry for DELETE missing");
}

#[test]
fn tc13_metrics_counters_increment_correctly() {
    if !docker_available() {
        eprintln!("Skipping tc13_metrics_counters_increment_correctly: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let (runtime, _db, mut storage) =
        create_storage_direct(&uri, "zenoh_tc13_db", "zenoh_tc13_coll");

    let before = metrics_snapshot();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/metrics").unwrap());
    let payload: ZBytes = "metrics-payload".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7054123555555555555/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    runtime.block_on(async {
        storage
            .put(key.clone(), payload, encoding, ts)
            .await
            .expect("put succeeds");
        let got = storage.get(key.clone(), "").await.expect("get succeeds");
        assert_eq!(got.len(), 1);
        storage.delete(key.clone(), ts).await.expect("delete succeeds");
    });

    let after = metrics_snapshot();
    assert!(after.puts >= before.puts + 1);
    assert!(after.gets >= before.gets + 1);
    assert!(after.deletes >= before.deletes + 1);
    assert_eq!(after.errors, before.errors);
}

// Legacy tests retained
#[test]
fn legacy_round_trip_put_get() {
    if !docker_available() {
        eprintln!("Skipping legacy_round_trip_put_get: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_rt".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_rt_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/test-key").unwrap());
    let payload: ZBytes = "{'sensor_id':123,'temp':25.5}".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a").unwrap();
    runtime
        .block_on(async { storage.put(key.clone(), payload.clone(), encoding.clone(), ts).await })
        .unwrap();
    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].encoding, encoding);
    assert_eq!(fetched[0].payload, payload);
}

#[test]
fn legacy_delete_removes_data() {
    if !docker_available() {
        eprintln!("Skipping legacy_delete_removes_data: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_del".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_del_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/delete-key").unwrap());
    let payload: ZBytes = "to be deleted".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a").unwrap();

    runtime
        .block_on(async { storage.put(key.clone(), payload, encoding, ts).await })
        .unwrap();
    let deleted = runtime
        .block_on(async { storage.delete(key.clone(), ts).await })
        .unwrap();
    assert!(matches!(deleted, StorageInsertionResult::Deleted));
    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert!(fetched.is_empty());
}

#[test]
fn legacy_round_trip_non_utf8_payload() {
    if !docker_available() {
        eprintln!("Skipping legacy_round_trip_non_utf8_payload: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_nonutf8".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_nonutf8_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/non-utf8").unwrap());
    let payload: ZBytes = vec![0xFF, 0xFE, 0xFD].into();
    let encoding = Encoding::from("application/octet-stream");
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a").unwrap();

    runtime
        .block_on(async { storage.put(key.clone(), payload.clone(), encoding.clone(), ts).await })
        .unwrap();
    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].payload, payload);
    assert_eq!(fetched[0].encoding, encoding);
}

#[test]
fn legacy_rejects_older_timestamp() {
    if !docker_available() {
        eprintln!("Skipping legacy_rejects_older_timestamp: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_lww".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_lww_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/last-write-wins").unwrap());
    let newer_payload: ZBytes = "newer-payload".into();
    let older_payload: ZBytes = "older-payload".into();
    let encoding = Encoding::from("text/plain");
    let newer_ts =
        Timestamp::from_str("7054123832858541151/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    let older_ts =
        Timestamp::from_str("7054123566570568799/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    runtime
        .block_on(async {
            storage
                .put(key.clone(), newer_payload.clone(), encoding.clone(), newer_ts)
                .await?;
            storage
                .put(key.clone(), older_payload.clone(), encoding.clone(), older_ts)
                .await
        })
        .unwrap();
    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].payload, newer_payload);
}

#[test]
fn legacy_put_is_idempotent() {
    if !docker_available() {
        eprintln!("Skipping legacy_put_is_idempotent: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_idem".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_idem_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/idempotent-key").unwrap());
    let payload: ZBytes = "{'sensor_id':123,'temp':25.5}".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a").unwrap();

    runtime
        .block_on(async {
            storage
                .put(key.clone(), payload.clone(), encoding.clone(), ts)
                .await?;
            storage
                .put(key.clone(), payload.clone(), encoding.clone(), ts)
                .await
        })
        .unwrap();
    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].payload, payload);
}

#[test]
fn legacy_delete_unknown_key() {
    if !docker_available() {
        eprintln!("Skipping legacy_delete_unknown_key: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_del_unknown".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_del_unknown_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/delete-key").unwrap());
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a").unwrap();
    let res = runtime
        .block_on(async { storage.delete(key.clone(), ts).await })
        .unwrap();
    assert!(matches!(res, StorageInsertionResult::Deleted));
}

#[test]
fn legacy_get_unknown_key() {
    if !docker_available() {
        eprintln!("Skipping legacy_get_unknown_key: Docker unavailable");
        return;
    }
    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.clone().into());
    rest.insert("database".into(), "zenoh_legacy_get_unknown".into());
    let vol_cfg = VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    };
    let storage_cfg = StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: vol_cfg.name.clone(),
        volume_cfg: serde_json::json!({ "collection": "zenoh_legacy_get_unknown_coll" }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    };
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/unknown-key").unwrap());
    let fetched: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert!(fetched.is_empty());
}
