use std::{str::FromStr, sync::Arc, time::Instant};

use futures::future::try_join_all;
use mongodb::Client;
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
        .map(|o| o.status.success())
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

fn build_volume_config(uri: &str, database: &str) -> VolumeConfig {
    let mut rest = serde_json::Map::new();
    rest.insert("mongodb_uri".into(), uri.into());
    rest.insert("database".into(), database.into());
    VolumeConfig {
        name: "mongo_volume".into(),
        backend: Some("zenoh_backend_mongodb".into()),
        paths: None,
        required: true,
        rest: rest.into(),
    }
}

fn build_storage_config(volume_id: &str, collection: &str) -> StorageConfig {
    StorageConfig {
        name: "mongo_storage".into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: volume_id.into(),
        volume_cfg: serde_json::json!({ "collection": collection }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    }
}

fn create_storage_direct(
    uri: &str,
    database: &str,
    collection: &str,
    runtime: &Arc<Runtime>,
) -> MongoDbStorage {
    let client = runtime
        .block_on(async { Client::with_uri_str(uri).await })
        .expect("connect mongo");
    let db = client.database(database);
    MongoDbStorage::new(&db, collection, runtime.clone())
}

fn print_header(name: &str) {
    println!("\n========== {name} ==========");
}

fn insertion_result_label(res: &StorageInsertionResult) -> &'static str {
    match res {
        StorageInsertionResult::Inserted => "Inserted",
        StorageInsertionResult::Replaced => "Replaced",
        StorageInsertionResult::Deleted => "Deleted",
        StorageInsertionResult::Outdated => "Outdated",
    }
}

#[test]
fn it1_end_to_end_crud() {
    if !docker_available() {
        eprintln!("Skipping it1_end_to_end_crud: Docker unavailable");
        return;
    }
    print_header("IT1 End-to-End CRUD (PUT -> GET -> DELETE)");
    let (_container, uri) = start_mongo();
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let vol_cfg = build_volume_config(&uri, "zenoh_it1_db");
    let storage_cfg = build_storage_config(&vol_cfg.name, "zenoh_it1_coll");
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();
    println!(
        "[IT1] Mongo uri='{uri}', db='{}', collection='{}'",
        "zenoh_it1_db", "zenoh_it1_coll"
    );

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/it1").unwrap());
    let payload: ZBytes = "it1-data".into();
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7054123999999999999/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    println!(
        "[IT1] PUT key='{}' ts='{ts}' payload='{}'",
        key.as_ref().unwrap(),
        std::str::from_utf8(payload.to_bytes().as_ref()).unwrap_or("<non-utf8>")
    );
    let t0 = Instant::now();
    let res = runtime
        .block_on(async { storage.put(key.clone(), payload.clone(), encoding.clone(), ts).await })
        .unwrap();
    println!(
        "[IT1] PUT result={} (elapsed {:?})",
        insertion_result_label(&res),
        t0.elapsed()
    );
    assert!(matches!(res, StorageInsertionResult::Inserted));

    println!("[IT1] GET key='{}'", key.as_ref().unwrap());
    let t0 = Instant::now();
    let got: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    println!(
        "[IT1] GET returned {} sample(s) (elapsed {:?})",
        got.len(),
        t0.elapsed()
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].payload, payload);

    println!("[IT1] DELETE key='{}' ts='{ts}'", key.as_ref().unwrap());
    let t0 = Instant::now();
    let del = runtime
        .block_on(async { storage.delete(key.clone(), ts).await })
        .unwrap();
    println!(
        "[IT1] DELETE result={} (elapsed {:?})",
        insertion_result_label(&del),
        t0.elapsed()
    );
    assert!(matches!(del, StorageInsertionResult::Deleted));
    let after: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert!(after.is_empty());
}

#[test]
fn it2_lww_conflict_resolution() {
    if !docker_available() {
        eprintln!("Skipping it2_lww_conflict_resolution: Docker unavailable");
        return;
    }
    print_header("IT2 LWW Conflict Resolution (older PUT then newer PUT)");
    let (_container, uri) = start_mongo();
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let vol_cfg = build_volume_config(&uri, "zenoh_it2_db");
    let storage_cfg = build_storage_config(&vol_cfg.name, "zenoh_it2_coll");
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();
    let mut storage = runtime
        .block_on(async { volume.create_storage(storage_cfg).await })
        .unwrap();
    println!(
        "[IT2] Mongo uri='{uri}', db='{}', collection='{}'",
        "zenoh_it2_db", "zenoh_it2_coll"
    );

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/it2").unwrap());
    let older = Timestamp::from_str("7054123000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    let newer = Timestamp::from_str("7054125000000000000/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();

    println!("[IT2] PUT (older) key='{}' ts='{older}' payload='older'", key.as_ref().unwrap());
    println!("[IT2] PUT (newer) key='{}' ts='{newer}' payload='newer'", key.as_ref().unwrap());
    runtime
        .block_on(async {
            storage
                .put(key.clone(), "older".into(), Encoding::from("text/plain"), older)
                .await?;
            storage
                .put(key.clone(), "newer".into(), Encoding::from("text/plain"), newer)
                .await
        })
        .unwrap();

    println!("[IT2] GET key='{}' (expect newest payload)", key.as_ref().unwrap());
    let got: Vec<StoredData> = runtime
        .block_on(async { storage.get(key.clone(), "").await })
        .unwrap();
    assert_eq!(got.len(), 1);
    let body = got[0].payload.to_bytes().into_owned();
    println!(
        "[IT2] Final value ts='{}' payload='{}'",
        got[0].timestamp,
        std::str::from_utf8(&body).unwrap_or("<non-utf8>")
    );
    assert_eq!(std::str::from_utf8(&body).unwrap(), "newer");
}

#[test]
fn it3_concurrency_latest_wins() {
    if !docker_available() {
        eprintln!("Skipping it3_concurrency_latest_wins: Docker unavailable");
        return;
    }
    print_header("IT3 Concurrency (multiple PUTs, latest timestamp wins)");
    let (_container, uri) = start_mongo();
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let db_name = "zenoh_it3_db";
    let coll = "zenoh_it3_coll";
    let vol_cfg = build_volume_config(&uri, db_name);
    let volume = MongoDbBackend::start("mongo_volume", &vol_cfg).unwrap();

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/it3").unwrap());
    let workers = 10usize;
    println!("[IT3] Mongo uri='{uri}', db='{db_name}', collection='{coll}', workers={workers}");
    println!("[IT3] Key='{}' (strip_prefix will store it as 'it3')", key.as_ref().unwrap());
    runtime.block_on(async {
        let mut tasks = Vec::new();
        for i in 0..workers {
            let storage_cfg = build_storage_config(&vol_cfg.name, coll);
            let mut storage = volume.create_storage(storage_cfg).await.unwrap();
            let key = key.clone();
            let ts = Timestamp::from_str(&format!(
                "{}/BC779A06D7E049BD88C3FF3DB0C17FCC",
                7054123000000000000u64 + i as u64 * 1000
            ))
            .unwrap();
            let payload: ZBytes = format!("v{i}").into();
            tasks.push(tokio::spawn(async move {
                storage
                    .put(key, payload, Encoding::from("text/plain"), ts)
                    .await
                    .unwrap();
                ts
            }));
        }
        let results = try_join_all(tasks).await.unwrap();
        let max_ts = results.into_iter().max().unwrap();
        println!("[IT3] Expected final timestamp (max) = '{max_ts}'");

        // Use a fresh storage on the base collection to read final value.
        let base_storage_cfg = build_storage_config(&vol_cfg.name, coll);
        let mut final_storage = volume.create_storage(base_storage_cfg).await.unwrap();
        let got = final_storage.get(key.clone(), "").await.unwrap();
        assert_eq!(got.len(), 1);
        let body = got[0].payload.to_bytes().into_owned();
        println!(
            "[IT3] Final stored ts='{}' payload='{}'",
            got[0].timestamp,
            std::str::from_utf8(&body).unwrap_or("<non-utf8>")
        );
        assert_eq!(got[0].timestamp, max_ts);
    });
}

#[test]
fn it4_persistence_after_recreate() {
    if !docker_available() {
        eprintln!("Skipping it4_persistence_after_recreate: Docker unavailable");
        return;
    }
    print_header("IT4 Persistence After Recreate (drop storage, recreate, read)");
    let (_container, uri) = start_mongo();
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let db_name = "zenoh_it4_db";
    let coll = "zenoh_it4_coll";
    let storage = create_storage_direct(&uri, db_name, coll, &runtime);
    let mut storage1 = storage;

    let key = Some(OwnedKeyExpr::try_from("demo/mongo/it4").unwrap());
    let payload: ZBytes = "persist".into();
    let ts = Timestamp::from_str("7054123888888888888/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    println!("[IT4] Mongo uri='{uri}', db='{db_name}', collection='{coll}'");
    println!("[IT4] PUT key='{}' ts='{ts}' payload='persist'", key.as_ref().unwrap());
    runtime
        .block_on(async { storage1.put(key.clone(), payload.clone(), Encoding::from("text/plain"), ts).await })
        .unwrap();

    // Drop and recreate storage (simulate restart/reconnect).
    println!("[IT4] Dropping storage instance and recreating it...");
    drop(storage1);
    let mut storage2 = create_storage_direct(&uri, db_name, coll, &runtime);
    println!("[IT4] GET key='{}' after recreate (expect persisted value)", key.as_ref().unwrap());
    let got: Vec<StoredData> = runtime
        .block_on(async { storage2.get(key.clone(), "").await })
        .unwrap();
    println!("[IT4] GET returned {} sample(s)", got.len());
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].payload, payload);
}

#[test]
fn it5_query_pagination_end_to_end() {
    if !docker_available() {
        eprintln!("Skipping it5_query_pagination_end_to_end: Docker unavailable");
        return;
    }
    print_header("IT5 Query + Pagination (FR6-FR8 end-to-end)");
    let (_container, uri) = start_mongo();
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let storage = create_storage_direct(&uri, "zenoh_it5_db", "zenoh_it5_coll", &runtime);
    let mut storage = storage;

    let base = "demo/mongo/it5";
    println!(
        "[IT5] Mongo uri='{uri}', db='{}', collection='{}', base_prefix='{base}'",
        "zenoh_it5_db", "zenoh_it5_coll"
    );
    for i in 0..15 {
        let ts = Timestamp::from_str(&format!(
            "7054123{}000000000/BC779A06D7E049BD88C3FF3DB0C17FCC",
            i + 10
        ))
        .unwrap();
        let key = Some(OwnedKeyExpr::try_from(format!("{base}/{i}").as_str()).unwrap());
        let payload: ZBytes = format!("p{i}").into();
        if i < 3 || i >= 12 {
            println!(
                "  [IT5] PUT key='{}' ts='{ts}' payload='p{i}'",
                key.as_ref().unwrap()
            );
        } else if i == 3 {
            println!("  [IT5] ... (omitting middle inserts for readability) ...");
        }
        runtime
            .block_on(async { storage.put(key, payload, Encoding::from("text/plain"), ts).await })
            .unwrap();
    }

    let page = PageRequest { limit: 5, offset: 5 };
    println!(
        "[IT5] enumerate_paged(prefix='{base}', limit={}, offset={})",
        page.limit, page.offset
    );
    let entries = runtime
        .block_on(async { storage.enumerate_paged(base, page).await })
        .unwrap();
    println!("[IT5] Returned {} entries (sorted by timestamp desc):", entries.len());
    for (idx, e) in entries.iter().enumerate() {
        let key = e
            .key
            .as_ref()
            .map(|k| k.to_string())
            .unwrap_or_else(|| "<null>".to_string());
        println!("  [IT5] [{idx}] key='{key}' ts='{}'", e.timestamp);
    }
    assert_eq!(entries.len(), 5);
    assert!(entries.iter().all(|e| e.key.as_ref().unwrap().to_string().starts_with(base)));
}
