use std::{str::FromStr, time::{Duration, Instant}};

use futures::future::try_join_all;
use mongodb::{bson::doc, Client};
use testcontainers::{core::WaitFor, runners::SyncRunner, GenericImage};
use tokio::runtime::Runtime;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh_backend_traits::config::{GarbageCollectionConfig, StorageConfig, VolumeConfig};
use zenoh_backend_traits::StorageInsertionResult;
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

fn build_storage_config(volume_id: &str, collection: &str, name: &str) -> StorageConfig {
    StorageConfig {
        name: name.into(),
        key_expr: OwnedKeyExpr::try_from("demo/mongo/**").unwrap(),
        complete: false,
        strip_prefix: Some(OwnedKeyExpr::try_from("demo/mongo").unwrap()),
        volume_id: volume_id.into(),
        volume_cfg: serde_json::json!({ "collection": collection }).into(),
        garbage_collection_config: GarbageCollectionConfig::default(),
        replication: None,
    }
}

#[test]
#[ignore = "manual stress run; heavy and requires Docker"]
fn mongo_backend_stress_puts() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_stress_puts: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let volume_config = build_volume_config(&uri);
    let collection_name = "zenoh_backend_tests_stress";

    // Spin up the backend.
    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    // Prepare multiple storages to drive concurrency (each task owns one).
    // Tuned down further for a realistic laptop run while still meaningful.
    let workers = 50usize;
    let total_puts = 10_000usize; // keeps runtime reasonable; adjust as needed.
    let per_worker = (total_puts + workers - 1) / workers;
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a")
        .expect("timestamp parses");

    let mut storages = Vec::with_capacity(workers);
    for i in 0..workers {
        let storage_cfg =
            build_storage_config(&volume_config.name, collection_name, &format!("worker-{i}"));
        let storage = runtime
            .block_on(async { volume.create_storage(storage_cfg).await })
            .expect("storage should be created");
        storages.push(storage);
    }

    let start_time = std::time::Instant::now();
    let tasks = storages.into_iter().enumerate().map(|(worker_idx, mut storage)| {
        let encoding = encoding.clone();
        let ts = ts;
        let start = worker_idx * per_worker;
        let end = ((worker_idx + 1) * per_worker).min(total_puts);
        tokio::spawn(async move {
            for i in start..end {
                let key = Some(
                    OwnedKeyExpr::try_from(format!("demo/mongo/stress-{i}"))
                        .expect("key_expr parses"),
                );
                let payload: ZBytes = format!("payload-{i}").into();
                let res = storage
                    .put(key, payload, encoding.clone(), ts)
                    .await
                    .expect("put should succeed");
                assert!(
                    matches!(res, StorageInsertionResult::Inserted | StorageInsertionResult::Replaced),
                    "unexpected insertion result in stress run"
                );
            }
            Ok::<(), String>(())
        })
    });

    runtime
        .block_on(async { try_join_all(tasks).await })
        .expect("all workers should finish");
    let elapsed = start_time.elapsed();

    // Verify row count in Mongo equals attempted inserts.
    let count = runtime
        .block_on(async {
            let client = Client::with_uri_str(&uri)
                .await
                .expect("connect to mongo to verify count");
            let collection = client
                .database("zenoh_test_db")
                .collection::<mongodb::bson::Document>(collection_name);
            collection
                .count_documents(doc! {}, None)
                .await
                .expect("count_documents should succeed")
        });

    assert_eq!(
        count as usize, total_puts,
        "stored document count must match sent puts"
    );

    let puts_per_sec = total_puts as f64 / elapsed.as_secs_f64();
    // Soft performance guard: expect at least ~500 puts/s on modest hardware; adjust if needed.
    assert!(
        puts_per_sec >= 500.0,
        "throughput below target: {:.0} puts/s over {:.2?}",
        puts_per_sec,
        elapsed
    );

    println!(
        "mongo_backend_stress_puts: total_puts={}, workers={}, elapsed={:.2?}, throughput={:.0} puts/s",
        total_puts, workers, elapsed, puts_per_sec
    );
}

#[test]
#[ignore = "manual latency run; heavy and requires Docker"]
fn mongo_backend_latency_percentiles() {
    let docker_available = std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if !docker_available {
        eprintln!("Skipping mongo_backend_latency_percentiles: Docker is not available.");
        return;
    }

    let (_container, uri) = start_mongo();
    let runtime = Runtime::new().expect("Tokio runtime must start");
    let volume_config = build_volume_config(&uri);
    let collection_name = "zenoh_backend_tests_latency";

    let volume = MongoDbBackend::start("mongo_volume", &volume_config)
        .expect("backend should start");

    // Focused on latency, so fewer ops than throughput test but still concurrent.
    let workers = 20usize;
    let total_puts = 2_000usize;
    let per_worker = (total_puts + workers - 1) / workers;
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7568996723869121120/9787a2e42432ae5da9f0ece1fb81975a")
        .expect("timestamp parses");

    let mut storages = Vec::with_capacity(workers);
    for i in 0..workers {
        let storage_cfg =
            build_storage_config(&volume_config.name, collection_name, &format!("latency-worker-{i}"));
        let storage = runtime
            .block_on(async { volume.create_storage(storage_cfg).await })
            .expect("storage should be created");
        storages.push(storage);
    }

    let tasks = storages.into_iter().enumerate().map(|(worker_idx, mut storage)| {
        let encoding = encoding.clone();
        let ts = ts;
        let start = worker_idx * per_worker;
        let end = ((worker_idx + 1) * per_worker).min(total_puts);
        tokio::spawn(async move {
            let mut durations = Vec::with_capacity(end.saturating_sub(start));
            for i in start..end {
                let key = Some(
                    OwnedKeyExpr::try_from(format!("demo/mongo/latency-{i}"))
                        .expect("key_expr parses"),
                );
                let payload: ZBytes = format!("payload-{i}").into();
                let put_start = Instant::now();
                let res = storage
                    .put(key, payload, encoding.clone(), ts)
                    .await
                    .expect("put should succeed");
                durations.push(put_start.elapsed());
                assert!(
                    matches!(res, StorageInsertionResult::Inserted | StorageInsertionResult::Replaced),
                    "unexpected insertion result in latency run"
                );
            }
            Ok::<Vec<Duration>, String>(durations)
        })
    });

    let worker_results = runtime
        .block_on(async { try_join_all(tasks).await })
        .expect("all workers should finish");
    let mut durations: Vec<Duration> = worker_results
        .into_iter()
        .map(|res| res.expect("worker should succeed"))
        .flatten()
        .collect();

    assert_eq!(durations.len(), total_puts, "all puts should be recorded");
    durations.sort_unstable();
    let percentile = |p: f64, data: &Vec<Duration>| -> Duration {
        let len = data.len();
        let rank = ((p * len as f64).ceil().max(1.0) as usize).saturating_sub(1);
        data[rank.min(len - 1)]
    };

    let p50 = percentile(0.50, &durations);
    let p95 = percentile(0.95, &durations);
    let p99 = percentile(0.99, &durations);

    // Soft guard: keep p95 under 20ms, p99 under 30ms on typical dev hardware.
    assert!(
        p95 < Duration::from_millis(20),
        "p95 latency too high: {:.2?}",
        p95
    );
    assert!(
        p99 < Duration::from_millis(30),
        "p99 latency too high: {:.2?}",
        p99
    );

    println!(
        "mongo_backend_latency_percentiles: total_puts={}, workers={}, p50={:.2?}, p95={:.2?}, p99={:.2?}",
        total_puts, workers, p50, p95, p99
    );
}
