use std::{str::FromStr, sync::Arc};

use mongodb::{bson::doc, Client};
use testcontainers::{core::WaitFor, runners::SyncRunner, GenericImage};
use tokio::runtime::Runtime;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh_backend_traits::config::{GarbageCollectionConfig, StorageConfig, VolumeConfig};
use zenoh_backend_traits::{Storage, StorageInsertionResult, StoredData};
use zenoh_backend_mongodb::{MongoDbBackend, MongoDbStorage, PageRequest};
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
        .block_on(async { storage.enumerate_range(base, Some(t1), Some(t2)).await })
        .expect("range succeeds");
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
    for i in 0..20 {
        let ts = Timestamp::from_str(&format!(
            "7054123{}000000000/BC779A06D7E049BD88C3FF3DB0C17FCC",
            i + 10
        ))
        .unwrap();
        let key = Some(OwnedKeyExpr::try_from(format!("{base}/{i}").as_str()).unwrap());
        let payload: ZBytes = format!("v{i}").into();
        runtime
            .block_on(async { storage.put(key, payload, Encoding::from("text/plain"), ts).await })
            .unwrap();
    }

    let page = PageRequest { limit: 5, offset: 5 };
    let entries = runtime
        .block_on(async { storage.enumerate_paged(base, page).await })
        .unwrap();
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
