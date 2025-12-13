use async_trait::async_trait;
use futures::{future::FutureExt, StreamExt};
use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, DateTime, Document, Regex},
    options::{FindOptions, IndexOptions, UpdateOptions},
    Client, Collection, Database, IndexModel,
};
use regex as re;
use serde_json::json;
use std::{
    borrow::Cow,
    str::FromStr,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::runtime::{Builder, Runtime};
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh::Result as ZResult;
use zenoh_backend_traits::config::{StorageConfig, VolumeConfig};
use zenoh_backend_traits::{Capability, Storage, StorageInsertionResult, StoredData, Volume};
use zenoh_plugin_trait::{Plugin, PluginControl};
use zenoh_util::ffi::JsonValue;

/// MongoDB backend main struct
pub struct MongoDbBackend {}

#[cfg(all(feature = "dynamic_plugin", not(test)))]
zenoh_plugin_trait::declare_plugin!(MongoDbBackend);

impl Plugin for MongoDbBackend {
    type StartArgs = VolumeConfig;
    type Instance = Box<dyn Volume>;

    const DEFAULT_NAME: &'static str = "mongodb_backend";
    const PLUGIN_VERSION: &'static str = "1.0.0";
    const PLUGIN_LONG_VERSION: &'static str = "1.0.0";

    fn start(_name: &str, config: &Self::StartArgs) -> ZResult<Self::Instance> {
        // Read MongoDB connection parameters from the config
        let cfg = config.rest.into_serde_map();
        let uri = cfg
            .get("mongodb_uri")
            .and_then(|v| v.as_str())
            .unwrap_or("mongodb://localhost:27017")
            .to_string();
        let database_name = cfg
            .get("database")
            .and_then(|v| v.as_str())
            .unwrap_or("zenoh_db")
            .to_string();

        let runtime = Arc::new(
            Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| format!("Failed to create Tokio runtime: {e}"))?,
        );
        let client = runtime
            .block_on(Client::with_uri_str(&uri))
            .map_err(|e| format!("Failed to connect to MongoDB: {e}"))?;
        let database = client.database(&database_name);
        Ok(Box::new(MongoDbVolume { database, runtime }))
    }
}

pub struct MongoDbVolume {
    database: Database,
    runtime: Arc<Runtime>,
}

#[async_trait]
impl Volume for MongoDbVolume {
    fn get_admin_status(&self) -> JsonValue {
        json!({
            "status": "mongodb backend",
            "database": self.database.name(),
        })
        .into()
    }

    fn get_capability(&self) -> Capability {
        Capability {
            persistence: zenoh_backend_traits::Persistence::Durable,
            history: zenoh_backend_traits::History::Latest,
        }
    }

    async fn create_storage(&self, config: StorageConfig) -> ZResult<Box<dyn Storage>> {
        let mut collection_name = "zenoh_data".to_string();
        if let serde_json::Value::Object(map) = config.volume_cfg.into_serde_value() {
            if let Some(serde_json::Value::String(name)) = map.get("collection") {
                collection_name = name.clone();
            }
        }
        let storage =
            MongoDbStorage::new(&self.database, &collection_name, self.runtime.clone());
        Ok(Box::new(storage))
    }
}

impl PluginControl for MongoDbVolume {}

pub struct MongoDbStorage {
    collection: Collection<Document>,
    collection_name: String,
    runtime: Arc<Runtime>,
    wal_collection: Collection<Document>,
    wal_seq: Arc<AtomicI64>,
    indexes_ready: tokio::sync::Mutex<bool>,
}

impl MongoDbStorage {
    pub fn new(database: &Database, collection_name: &str, runtime: Arc<Runtime>) -> Self {
        let collection = database.collection::<Document>(collection_name);
        let wal_collection = database.collection::<Document>("wal_log");
        // Avoid `block_on()` in constructors: this type is often created inside an existing Tokio runtime
        // (e.g., `Volume::create_storage()`), and nesting runtimes would panic.
        let wal_seq_start = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
            .saturating_mul(1_000);
        MongoDbStorage {
            collection,
            collection_name: collection_name.to_string(),
            runtime,
            wal_collection,
            wal_seq: Arc::new(AtomicI64::new(wal_seq_start)),
            indexes_ready: tokio::sync::Mutex::new(false),
        }
    }

    async fn run_on_runtime<F, T>(&self, fut: F) -> ZResult<T>
    where
        F: std::future::Future<Output = mongodb::error::Result<T>> + Send + 'static,
        T: Send + 'static,
    {
        let runtime = self.runtime.clone();
        let (task, handle) = fut.remote_handle();
        runtime.spawn(task);
        handle
            .await
            .map_err(|e| format!("MongoDB operation failed: {e}").into())
    }

    fn timestamp_to_i64(ts: &Timestamp) -> i64 {
        let text = ts.to_string();
        if let Some(num) = text.split('/').next() {
            num.parse::<i64>().unwrap_or(0)
        } else {
            0
        }
    }

    fn build_regex_from_prefix(prefix: &str) -> Regex {
        let escaped = re::escape(prefix);
        let pattern = escaped.replace("\\*\\*", ".*");
        Regex {
            pattern: format!("^{pattern}"),
            options: "i".into(),
        }
    }

    fn owned_key_from_bson(b: &Bson) -> Option<OwnedKeyExpr> {
        match b {
            Bson::String(s) => OwnedKeyExpr::try_from(s.as_str()).ok(),
            _ => None,
        }
    }

    async fn append_wal(
        &self,
        op: &str,
        key: &Option<OwnedKeyExpr>,
        ts: &Timestamp,
        payload: Option<&[u8]>,
        enc: Option<&str>,
    ) -> ZResult<()> {
        let seq_id = self.wal_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let key_str = key.as_ref().map(|k| k.to_string()).unwrap_or_default();
        let payload_size = payload.map(|p| p.len() as i64).unwrap_or(0);
        let ts_raw = Self::timestamp_to_i64(ts);
        let ts_string = ts.to_string();
        let wal_collection = self.wal_collection.clone();
        let op_owned = op.to_string();
        let enc_owned = enc.map(|e| e.to_string());
        self.run_on_runtime(async move {
            wal_collection
                .insert_one(
                    doc! {
                        "seq_id": seq_id,
                        "op": op_owned,
                        "key_expr": key_str,
                        "timestamp": ts_string,
                        "timestamp_raw": ts_raw,
                        "payload_size": payload_size,
                        "encoding": enc_owned,
                        "created_at": DateTime::now(),
                    },
                    None,
                )
                .await
                .map(|_| ())
        })
        .await
    }

    async fn ensure_indexes(&self) -> ZResult<()> {
        let mut guard = self.indexes_ready.lock().await;
        if *guard {
            return Ok(());
        }

        let collection = self.collection.clone();
        let wal_collection = self.wal_collection.clone();
        self.run_on_runtime(async move {
            collection
                .create_index(
                    IndexModel::builder()
                        .keys(doc! { "key": 1 })
                        .options(IndexOptions::builder().unique(true).build())
                        .build(),
                    None,
                )
                .await?;
            collection
                .create_index(
                    IndexModel::builder()
                        .keys(doc! { "timestamp_raw": -1 })
                        .build(),
                    None,
                )
                .await?;
            wal_collection
                .create_index(
                    IndexModel::builder().keys(doc! { "seq_id": 1 }).build(),
                    None,
                )
                .await?;
            wal_collection
                .create_index(
                    IndexModel::builder()
                        .keys(doc! { "created_at": 1 })
                        .build(),
                    None,
                )
                .await?;
            Ok(())
        })
        .await?;

        *guard = true;
        Ok(())
    }

    pub async fn enumerate(&self, key_expr: &str) -> ZResult<Vec<StorageEntry>> {
        self.enumerate_internal(key_expr, None, None, None).await
    }

    pub async fn enumerate_range(
        &self,
        key_expr: &str,
        from_ts: Option<Timestamp>,
        to_ts: Option<Timestamp>,
    ) -> ZResult<Vec<StorageEntry>> {
        self.enumerate_internal(key_expr, from_ts, to_ts, None).await
    }

    pub async fn enumerate_paged(
        &self,
        key_expr: &str,
        page: PageRequest,
    ) -> ZResult<Vec<StorageEntry>> {
        self.enumerate_internal(
            key_expr,
            None,
            None,
            Some((page.limit as i64, page.offset as i64)),
        )
        .await
    }

    async fn enumerate_internal(
        &self,
        key_expr: &str,
        from_ts: Option<Timestamp>,
        to_ts: Option<Timestamp>,
        paging: Option<(i64, i64)>,
    ) -> ZResult<Vec<StorageEntry>> {
        let regex = Self::build_regex_from_prefix(key_expr);
        let mut filter = doc! { "key": { "$regex": regex } };
        if from_ts.is_some() || to_ts.is_some() {
            let mut ts_filter = doc! {};
            if let Some(f) = from_ts {
                ts_filter.insert("$gte", Self::timestamp_to_i64(&f));
            }
            if let Some(t) = to_ts {
                ts_filter.insert("$lte", Self::timestamp_to_i64(&t));
            }
            filter.insert("timestamp_raw", ts_filter);
        }

        let mut opts = FindOptions::builder()
            .sort(doc! { "timestamp_raw": -1 })
            .build();
        if let Some((limit, offset)) = paging {
            opts.limit = Some(limit);
            opts.skip = Some(offset as u64);
        }

        let collection = self.collection.clone();
        let docs = self
            .run_on_runtime(async move {
                let mut cursor = collection.find(filter, opts).await?;
                let mut out = Vec::new();
                while let Some(doc) = cursor.next().await.transpose()? {
                    out.push(doc);
                }
                Ok(out)
            })
            .await?;

        docs.into_iter()
            .map(|document| {
                let key_bson = document
                    .get("key")
                    .ok_or_else(|| "MongoDB enumerate missing 'key'".to_string())?;
                let key = Self::owned_key_from_bson(key_bson);
                let value = document
                    .get_binary_generic("value")
                    .map_err(|e| format!("MongoDB enumerate failed to read 'value': {e}"))?
                    .to_vec();
                let encoding = document
                    .get_str("encoding")
                    .map(|s| Encoding::from(s))
                    .map_err(|e| format!("MongoDB enumerate failed to read 'encoding': {e}"))?;
                let ts_str = document
                    .get_str("timestamp")
                    .map_err(|e| format!("MongoDB enumerate failed to read 'timestamp': {e}"))?;
                let timestamp = Timestamp::from_str(ts_str)
                    .map_err(|e| format!("MongoDB enumerate failed to parse 'timestamp': {}", e.cause))?;
                Ok(StorageEntry {
                    key,
                    payload: value.into(),
                    encoding,
                    timestamp,
                })
            })
            .collect()
    }
}

#[async_trait]
impl Storage for MongoDbStorage {
    fn get_admin_status(&self) -> JsonValue {
        json!({
            "collection": self.collection_name.clone(),
        })
        .into()
    }

    async fn put(
        &mut self,
        key: Option<OwnedKeyExpr>,
        value: ZBytes,
        encoding: Encoding,
        timestamp: Timestamp,
    ) -> ZResult<StorageInsertionResult> {
        self.ensure_indexes().await?;

        let key_bson = match key {
            Some(ref k) => Bson::String(k.to_string()),
            None => Bson::Null,
        };
        let payload = value.to_bytes().into_owned();
        let value_text = std::str::from_utf8(&payload).map(|s| s.to_owned()).ok();
        let encoding_str: Cow<'static, str> = (&encoding).into();
        let incoming_ts_raw = Self::timestamp_to_i64(&timestamp);
        let incoming_ts_str = timestamp.to_string();
        self.append_wal("PUT", &key, &timestamp, Some(&payload), Some(encoding_str.as_ref()))
            .await?;

        let key_filter = key_bson.clone();
        let mut set_doc = doc! {
            "value": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: payload.clone() }),
            "encoding": encoding_str.as_ref(),
            "timestamp": incoming_ts_str,
            "timestamp_raw": incoming_ts_raw,
        };
        if let Some(text) = value_text {
            set_doc.insert("value_text", text);
        }

        let collection = self.collection.clone();
        let filter = doc! {
            "key": key_filter.clone(),
            "$or": [
                { "timestamp_raw": { "$lt": incoming_ts_raw } },
                { "timestamp_raw": { "$exists": false } }
            ]
        };
        let update = doc! {
            "$set": set_doc,
            "$setOnInsert": { "key": key_filter.clone() }
        };

        let upsert_opts = UpdateOptions::builder().upsert(true).build();
        let result = self
            .run_on_runtime(async move { collection.update_one(filter, update, upsert_opts).await })
            .await;

        match result {
            Ok(res) => {
                if res.upserted_id.is_some() {
                    Ok(StorageInsertionResult::Inserted)
                } else if res.matched_count > 0 {
                    Ok(StorageInsertionResult::Replaced)
                } else {
                    Ok(StorageInsertionResult::Outdated)
                }
            }
            Err(e) if format!("{e}").contains("E11000") => {
                // Concurrent writer likely inserted; retry once without upsert to avoid duplicates.
                let key_filter = key_bson.clone();
                let collection = self.collection.clone();
                let filter = doc! {
                    "key": key_filter.clone(),
                    "$or": [
                        { "timestamp_raw": { "$lt": incoming_ts_raw } },
                        { "timestamp_raw": { "$exists": false } }
                    ]
                };
                let mut set_doc = doc! {
                    "value": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: payload.clone() }),
                    "encoding": encoding_str.as_ref(),
                    "timestamp": timestamp.to_string(),
                    "timestamp_raw": incoming_ts_raw,
                };
                if let Some(text) = std::str::from_utf8(&payload).map(|s| s.to_owned()).ok() {
                    set_doc.insert("value_text", text);
                }
                let update = doc! { "$set": set_doc };
                let res = self
                    .run_on_runtime(async move {
                        collection.update_one(filter, update, None).await
                    })
                    .await?;
                if res.matched_count > 0 {
                    Ok(StorageInsertionResult::Replaced)
                } else {
                    Ok(StorageInsertionResult::Outdated)
                }
            }
            Err(e) => Err(format!("MongoDB PUT failed: {e}").into()),
        }
    }

    async fn get(
        &mut self,
        key: Option<OwnedKeyExpr>,
        _parameters: &str,
    ) -> ZResult<Vec<StoredData>> {
        let key_bson = match key {
            Some(k) => Bson::String(k.to_string()),
            None => Bson::Null,
        };
        let filter = doc! { "key": key_bson };
        let collection = self.collection.clone();
        if let Some(document) = self
            .run_on_runtime(async move { collection.find_one(filter, None).await })
            .await?
        {
            let value = document
                .get_binary_generic("value")
                .map_err(|e| format!("MongoDB GET failed to read 'value': {e}"))?
                .to_vec();
            let encoding = document
                .get_str("encoding")
                .map(|s| Encoding::from(s))
                .map_err(|e| format!("MongoDB GET failed to read 'encoding': {e}"))?;
            let timestamp_str = document
                .get_str("timestamp")
                .map_err(|e| format!("MongoDB GET failed to read 'timestamp': {e}"))?;
            let timestamp = Timestamp::from_str(timestamp_str)
                .map_err(|e| format!("MongoDB GET failed to parse 'timestamp': {}", e.cause))?;
            Ok(vec![StoredData {
                payload: value.into(),
                encoding,
                timestamp,
            }])
        } else {
            Ok(vec![])
        }
    }

    async fn delete(
        &mut self,
        key: Option<OwnedKeyExpr>,
        timestamp: Timestamp,
    ) -> ZResult<StorageInsertionResult> {
        let key_bson = match key {
            Some(ref k) => Bson::String(k.to_string()),
            None => Bson::Null,
        };
        let filter = doc! { "key": key_bson };
        let collection = self.collection.clone();
        self.append_wal("DELETE", &key, &timestamp, None, None)
            .await?;
        self.run_on_runtime(async move {
            collection
                .delete_one(filter, None)
                .await
                .map(|_| StorageInsertionResult::Deleted)
        })
        .await
    }

    async fn get_all_entries(&self) -> ZResult<Vec<(Option<OwnedKeyExpr>, Timestamp)>> {
        let collection = self.collection.clone();
        let docs = self
            .run_on_runtime(async move {
                let mut cursor = collection
                    .find(
                        doc! {},
                        FindOptions::builder()
                            .projection(doc! { "key": 1, "timestamp": 1 })
                            .build(),
                    )
                    .await?;
                let mut out = Vec::new();
                while let Some(doc) = cursor.next().await.transpose()? {
                    out.push(doc);
                }
                Ok(out)
            })
            .await?;

        docs.into_iter()
            .map(|document| {
                let key_bson = document
                    .get("key")
                    .ok_or_else(|| "MongoDB get_all_entries missing 'key'".to_string())?;
                let key = Self::owned_key_from_bson(key_bson);
                let ts_str = document
                    .get_str("timestamp")
                    .map_err(|e| format!("MongoDB get_all_entries failed to read 'timestamp': {e}"))?;
                let timestamp = Timestamp::from_str(ts_str)
                    .map_err(|e| format!("MongoDB get_all_entries failed to parse 'timestamp': {}", e.cause))?;
                Ok((key, timestamp))
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct StorageEntry {
    pub key: Option<OwnedKeyExpr>,
    pub payload: ZBytes,
    pub encoding: Encoding,
    pub timestamp: Timestamp,
}

#[derive(Debug, Clone, Copy)]
pub struct PageRequest {
    pub limit: u32,
    pub offset: u32,
}
