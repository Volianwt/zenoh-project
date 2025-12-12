use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use criterion::{criterion_group, criterion_main, Criterion};
use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, Document},
    options::ReplaceOptions,
    Client,
};
use testcontainers::{core::WaitFor, runners::SyncRunner, GenericImage};
use tokio::runtime::Runtime;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh_backend_mongodb::MongoDbStorage;

fn start_mongo() -> (testcontainers::Container<GenericImage>, String) {
    let image = GenericImage::new("mongo", "7.0.8")
        .with_wait_for(WaitFor::message_on_stdout("Waiting for connections"))
        .with_exposed_port(27017);
    let container = image.start();
    let port = container.get_host_port_ipv4(27017);
    let uri = format!("mongodb://127.0.0.1:{port}");
    (container, uri)
}

fn setup_storage(db_name: &str, coll: &str) -> (testcontainers::Container<GenericImage>, Arc<Runtime>, MongoDbStorage, String) {
    let (container, uri) = start_mongo();
    let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));
    let client = runtime
        .block_on(async { Client::with_uri_str(&uri).await })
        .expect("connects to mongo");
    let db = client.database(db_name);
    let storage = MongoDbStorage::new(&db, coll, runtime.clone());
    (container, runtime, storage, uri)
}

fn bench_put_latency(c: &mut Criterion) {
    let (_container, runtime, mut storage, _uri) =
        setup_storage("zenoh_bench_put", "zenoh_bench_put_coll");
    let counter = AtomicU64::new(7054123000000000000);
    let key = Some(OwnedKeyExpr::try_from("bench/put").unwrap());
    let encoding = Encoding::from("text/plain");

    c.bench_function("put_latency_wal", |b| {
        b.iter(|| {
            let ts_val = counter.fetch_add(1, Ordering::SeqCst);
            let ts = Timestamp::from_str(&format!(
                "{}/BC779A06D7E049BD88C3FF3DB0C17FCC",
                ts_val
            ))
            .unwrap();
            runtime
                .block_on(async {
                    storage
                        .put(key.clone(), ZBytes::from("payload"), encoding.clone(), ts)
                        .await
                })
                .unwrap();
        })
    });
    drop(runtime);
}

fn bench_get_latency(c: &mut Criterion) {
    let (_container, runtime, mut storage, _uri) =
        setup_storage("zenoh_bench_get", "zenoh_bench_get_coll");
    let key = Some(OwnedKeyExpr::try_from("bench/get").unwrap());
    let encoding = Encoding::from("text/plain");
    let ts = Timestamp::from_str("7054123555555555555/BC779A06D7E049BD88C3FF3DB0C17FCC").unwrap();
    runtime
        .block_on(async { storage.put(key.clone(), ZBytes::from("payload"), encoding, ts).await })
        .unwrap();

    c.bench_function("get_latency", |b| {
        b.iter(|| {
            runtime
                .block_on(async { storage.get(key.clone(), "").await })
                .unwrap();
        })
    });
    drop(runtime);
}

fn bench_wal_overhead(c: &mut Criterion) {
    let (_container, runtime, mut storage, uri) =
        setup_storage("zenoh_bench_wal", "zenoh_bench_wal_coll");
    let client = runtime
        .block_on(async { Client::with_uri_str(&uri).await })
        .expect("connect mongo");
    let db = client.database("zenoh_bench_wal");
    let raw_collection = db.collection::<Document>("zenoh_bench_wal_coll_raw");
    let key = "bench/wal";
    let encoding = "text/plain";
    let counter = AtomicU64::new(7054123777777777777);

    c.bench_function("put_with_wal", |b| {
        b.iter(|| {
            let ts_val = counter.fetch_add(1, Ordering::SeqCst);
            let ts = Timestamp::from_str(&format!(
                "{}/BC779A06D7E049BD88C3FF3DB0C17FCC",
                ts_val
            ))
            .unwrap();
            runtime
                .block_on(async {
                    storage
                        .put(
                            Some(OwnedKeyExpr::try_from(key).unwrap()),
                            ZBytes::from("payload"),
                            Encoding::from(encoding),
                            ts,
                        )
                        .await
                })
                .unwrap();
        })
    });

    c.bench_function("put_without_wal_raw_replace", |b| {
        b.iter(|| {
            let ts_val = counter.fetch_add(1, Ordering::SeqCst);
            let ts = Timestamp::from_str(&format!(
                "{}/BC779A06D7E049BD88C3FF3DB0C17FCC",
                ts_val
            ))
            .unwrap();
            let payload = ZBytes::from("payload").to_bytes().into_owned();
            let doc = doc! {
                "key": key,
                "value": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: payload.clone() }),
                "encoding": encoding,
                "timestamp": ts.to_string(),
                "timestamp_raw": 0_i64,
            };
            let opts = ReplaceOptions::builder().upsert(true).build();
            runtime
                .block_on(async {
                    raw_collection
                        .replace_one(doc! { "key": key }, doc, opts)
                        .await
                })
                .unwrap();
        })
    });
    drop(runtime);
}

fn criterion_configuration() -> Criterion {
    Criterion::default()
}

criterion_group!(
    name = benches;
    config = criterion_configuration();
    targets = bench_put_latency, bench_get_latency, bench_wal_overhead
);
criterion_main!(benches);
