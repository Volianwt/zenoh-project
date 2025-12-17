use std::{str::FromStr, sync::Arc};

use futures::StreamExt;
use mongodb::bson::doc;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use testcontainers::{core::WaitFor, runners::SyncRunner, GenericImage};
use tokio::runtime::Runtime;
use uuid::Uuid;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh_backend_mongodb::MongoDbStorage;
use zenoh_backend_traits::Storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    Put,
    Delete,
}

#[derive(Debug, Clone)]
struct Op {
    ts_raw: i64,
    kind: OpKind,
    nonce: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FinalState {
    Tombstone(i64),
    Value(i64, ZBytes),
}

fn apply_ops_model(ops: &[Op]) -> FinalState {
    // ops is generated with len >= 1
    let max_ts = ops.iter().map(|o| o.ts_raw).max().unwrap();
    let winner = ops.iter().rev().find(|o| o.ts_raw == max_ts).unwrap();
    match winner.kind {
        OpKind::Put => {
            FinalState::Value(max_ts, ZBytes::from(format!("v{}-{}", max_ts, winner.nonce)))
        }
        OpKind::Delete => FinalState::Tombstone(max_ts),
    }
}

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

fn create_storage_direct(
    uri: &str,
    database: &str,
    collection: &str,
    runtime: &Arc<Runtime>,
) -> MongoDbStorage {
    let client = runtime
        .block_on(async { mongodb::Client::with_uri_str(uri).await })
        .expect("connect mongo");
    let db = client.database(database);
    MongoDbStorage::new(&db, collection, runtime.clone())
}

prop_compose! {
    fn arb_op()(
        ts in 0i64..1_000_000,
        kind in prop_oneof![Just(OpKind::Put), Just(OpKind::Delete)],
        nonce in any::<u32>(),
    ) -> Op {
        Op { ts_raw: ts, kind, nonce }
    }
}

fn make_ts(ts_raw: i64) -> Timestamp {
    // Build a Timestamp string with ts_raw as the seconds/nanos part; suffix arbitrary.
    Timestamp::from_str(&format!("{}/BC779A06D7E049BD88C3FF3DB0C17FCC", ts_raw)).unwrap()
}

fn read_i64_field(doc: &mongodb::bson::Document, field: &str) -> Option<i64> {
    doc.get_i64(field)
        .ok()
        .or_else(|| doc.get_i32(field).ok().map(|v| v as i64))
        .or_else(|| {
            doc.get_str(field)
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
        })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("proptest-regressions"))),
        .. ProptestConfig::default()
    })]
    #[test]
    fn lww_matches_model(ops in prop::collection::vec(arb_op(), 1..30)) {
        if std::env::var("RUN_PROP_MONGO").ok().as_deref() != Some("1") {
            // Opt-in only.
            return Ok(());
        }
        if !docker_available() {
            return Ok(());
        }

        let (_container, uri) = start_mongo();
        let runtime = Arc::new(Runtime::new().expect("Tokio runtime must start"));

        // Model result
        let model = apply_ops_model(&ops);

        // Mongo setup per-case to avoid cross contamination (fresh DB each case)
        let db_name = format!("zenoh_prop_{}", Uuid::new_v4());
        let collection = "zenoh_prop_coll";
        let mut storage = create_storage_direct(&uri, &db_name, collection, &runtime);
        let key = Some(OwnedKeyExpr::try_from("demo/mongo/prop").unwrap());
        let key_str = key.as_ref().unwrap().to_string();
        for op in &ops {
            match op.kind {
                OpKind::Put => {
                    let payload: ZBytes = format!("v{}-{}", op.ts_raw, op.nonce).into();
                    runtime
                        .block_on(async {
                            storage
                                .put(key.clone(), payload, Encoding::from("text/plain"), make_ts(op.ts_raw))
                                .await
                        })
                        .unwrap();
                }
                OpKind::Delete => {
                    runtime
                        .block_on(async {
                            storage.delete(key.clone(), make_ts(op.ts_raw)).await
                        })
                        .unwrap();
                }
            }
        }

        let got = runtime.block_on(async { storage.get(key.clone(), "").await }).unwrap();

        match model {
            FinalState::Tombstone(ts) => {
                prop_assert!(got.is_empty(), "expected tombstone, got value");
                // Ensure tombstone is present in DB (deleted=true)
                let client = runtime.block_on(async { mongodb::Client::with_uri_str(&uri).await }).unwrap();
                let db = client.database(&db_name);
                let coll = db.collection::<mongodb::bson::Document>(collection);
                let doc = runtime.block_on(async { coll.find_one(doc! { "key": key.clone().map(|k| k.to_string()).unwrap_or_default() }, None).await }).unwrap();
                prop_assert!(doc.is_some(), "tombstone should exist");
                let d = doc.unwrap();
                prop_assert!(d.get_bool("deleted").unwrap_or(false));
                prop_assert_eq!(read_i64_field(&d, "timestamp_raw"), Some(ts));
                prop_assert!(d.get("value").is_none());
                prop_assert!(d.get("encoding").is_none());
            }
            FinalState::Value(ts, expected_payload) => {
                prop_assert_eq!(got.len(), 1);
                let val = got[0].payload.to_bytes().into_owned();
                prop_assert_eq!(val, expected_payload.to_bytes().into_owned());
                let actual_ts_raw = got[0].timestamp.to_string().split('/').next().and_then(|s| s.parse::<i64>().ok());
                prop_assert_eq!(actual_ts_raw, Some(ts));
            }
        }

        // WAL seq strictly increasing
        let client = runtime.block_on(async { mongodb::Client::with_uri_str(&uri).await }).unwrap();
        let db = client.database(&db_name);
        let wal = db.collection::<mongodb::bson::Document>("wal_log");
        let wal_docs: Vec<_> = runtime
            .block_on(async {
                let mut cursor = wal
                    .find(doc! { "key_expr": &key_str }, mongodb::options::FindOptions::builder().sort(doc! { "seq_id": 1 }).build())
                    .await
                    .unwrap();
                let mut v = Vec::new();
                while let Some(doc) = cursor.next().await.transpose().unwrap() {
                    v.push(doc);
                }
                v
            });
        let seqs: Vec<i64> = wal_docs
            .iter()
            .filter_map(|d| read_i64_field(d, "seq_id"))
            .collect();
        prop_assert_eq!(seqs.len(), ops.len());
        for w in seqs.windows(2) {
            prop_assert!(w[1] > w[0], "seq_id not increasing: {:?}", seqs);
        }
    }
}
