use async_trait::async_trait;
use futures::{future::FutureExt, StreamExt};
use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, DateTime, Document, Regex},
    options::{
        FindOneAndUpdateOptions, FindOptions, IndexOptions, ReturnDocument, UpdateOptions,
    },
    Collection, Database, IndexModel,
};
use regex as re;
use std::{borrow::Cow, str::FromStr, sync::Arc};
use tokio::runtime::Runtime;
use zenoh::bytes::{Encoding, ZBytes};
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::time::Timestamp;
use zenoh::Result as ZResult;
use zenoh_backend_traits::{Storage, StorageInsertionResult, StoredData};
use zenoh_util::ffi::JsonValue;

use crate::types::{PageRequest, StorageEntry};

pub struct MongoDbStorage {
    collection: Collection<Document>,
    collection_name: String,
    runtime: Arc<Runtime>,
    wal_collection: Collection<Document>,
    wal_seq_collection: Collection<Document>,
    indexes_ready: tokio::sync::Mutex<bool>,
}

impl MongoDbStorage {
    pub fn new(database: &Database, collection_name: &str, runtime: Arc<Runtime>) -> Self {
        let collection = database.collection::<Document>(collection_name);
        let wal_collection = database.collection::<Document>("wal_log");
        let wal_seq_collection = database.collection::<Document>("wal_seq");
        MongoDbStorage {
            collection,
            collection_name: collection_name.to_string(),
            runtime,
            wal_collection,
            wal_seq_collection,
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

    fn read_i64_field(doc: &Document, field: &str) -> Option<i64> {
        doc.get_i64(field)
            .ok()
            .or_else(|| doc.get_i32(field).ok().map(|v| v as i64))
            .or_else(|| {
                doc.get_str(field)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
            })
    }

    async fn append_wal(
        &self,
        op: &str,
        key: &Option<OwnedKeyExpr>,
        ts: &Timestamp,
        payload: Option<&[u8]>,
        enc: Option<&str>,
    ) -> ZResult<()> {
        let seq_id = self.next_wal_seq().await?;
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
        let mut filter = doc! { "key": { "$regex": regex }, "deleted": { "$ne": true } };
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

    async fn next_wal_seq(&self) -> ZResult<i64> {
        let wal_seq_collection = self.wal_seq_collection.clone();
        let updated = self
            .run_on_runtime(async move {
                let opts = FindOneAndUpdateOptions::builder()
                    .upsert(true)
                    .return_document(ReturnDocument::After)
                    .build();
                wal_seq_collection
                    .find_one_and_update(
                        doc! { "_id": "wal_seq" },
                        doc! { "$inc": { "seq_id": 1_i64 } },
                        opts,
                    )
                    .await
            })
            .await?;
        updated
            .and_then(|doc| {
                doc.get_i64("seq_id")
                    .ok()
                    .or_else(|| doc.get_i32("seq_id").ok().map(|v| v as i64))
            })
            .ok_or_else(|| "MongoDB WAL sequence fetch failed".into())
    }
}

#[async_trait]
impl Storage for MongoDbStorage {
    fn get_admin_status(&self) -> JsonValue {
        serde_json::json!({
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
            "deleted": false,
        };
        if let Some(text) = value_text {
            set_doc.insert("value_text", text);
        }

        let collection = self.collection.clone();
        let filter = doc! {
            "key": key_filter.clone(),
            "$or": [
                { "timestamp_raw": { "$lte": incoming_ts_raw } },
                { "timestamp_raw": { "$exists": false } }
            ]
        };
        let update = doc! { "$set": set_doc, "$setOnInsert": { "key": key_filter.clone() } };

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
                    "deleted": false,
                };
                if let Some(text) = std::str::from_utf8(&payload).map(|s| s.to_owned()).ok() {
                    set_doc.insert("value_text", text);
                }
                let update = doc! { "$set": set_doc };
                let res = self
                    .run_on_runtime(async move { collection.update_one(filter, update, None).await })
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
            if document.get_bool("deleted").unwrap_or(false) {
                return Ok(vec![]);
            }
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
        self.ensure_indexes().await?;

        let key_bson = match key {
            Some(ref k) => Bson::String(k.to_string()),
            None => Bson::Null,
        };
        let incoming_ts_raw = Self::timestamp_to_i64(&timestamp);
        let key_filter = key_bson.clone();
        let filter = doc! {
            "key": key_filter.clone(),
            "$or": [
                { "timestamp_raw": { "$lte": incoming_ts_raw } },
                { "timestamp_raw": { "$exists": false } }
            ]
        };
        let collection = self.collection.clone();
        self.append_wal("DELETE", &key, &timestamp, None, None)
            .await?;
        let set_doc = doc! {
            "deleted": true,
            "timestamp": timestamp.to_string(),
            "timestamp_raw": incoming_ts_raw,
        };
        let update = doc! {
            "$set": set_doc,
            "$unset": { "value": "", "value_text": "", "encoding": "" },
            "$setOnInsert": { "key": key_filter.clone() }
        };
        let opts = UpdateOptions::builder().upsert(true).build();
        let result = self
            .run_on_runtime(async move { collection.update_one(filter, update, opts).await })
            .await;
        match result {
            Ok(res) if res.upserted_id.is_some() || res.matched_count > 0 => {
                Ok(StorageInsertionResult::Deleted)
            }
            Ok(_) => {
                // Fallback: fetch current doc and decide based on timestamp.
                let collection = self.collection.clone();
                let key_filter = doc! { "key": key_bson };
                let key_filter_clone = key_filter.clone();
                let current = self
                    .run_on_runtime(async move { collection.find_one(key_filter, None).await })
                    .await?;
                if let Some(doc) = current {
                    if let Some(ts_raw) = Self::read_i64_field(&doc, "timestamp_raw") {
                        if ts_raw > incoming_ts_raw {
                            return Ok(StorageInsertionResult::Outdated);
                        }
                    }
                }
                // Either no document or not newer: force tombstone without timestamp gating.
                let collection = self.collection.clone();
                let update = doc! {
                    "$set": {
                        "deleted": true,
                        "timestamp": timestamp.to_string(),
                        "timestamp_raw": incoming_ts_raw,
                    },
                    "$unset": { "value": "", "value_text": "", "encoding": "" },
                    "$setOnInsert": { "key": key_filter_clone.clone() }
                };
                self.run_on_runtime(async move {
                    collection
                        .update_one(doc! { "key": key_filter_clone }, update, UpdateOptions::builder().upsert(true).build())
                        .await
                })
                .await?;
                Ok(StorageInsertionResult::Deleted)
            }
            Err(e) if format!("{e}").contains("E11000") => {
                // Duplicate key: there is already a doc for this key. Decide by timestamp then force tombstone.
                let collection = self.collection.clone();
                let key_doc = doc! { "key": key_bson.clone() };
                let existing = self
                    .run_on_runtime(async move { collection.find_one(key_doc, None).await })
                    .await?;
                if let Some(doc) = existing {
                    if let Some(ts_raw) = Self::read_i64_field(&doc, "timestamp_raw") {
                        if ts_raw > incoming_ts_raw {
                            return Ok(StorageInsertionResult::Outdated);
                        }
                    }
                }
                let collection = self.collection.clone();
                let key_filter = doc! { "key": key_bson };
                let update = doc! {
                    "$set": {
                        "deleted": true,
                        "timestamp": timestamp.to_string(),
                        "timestamp_raw": incoming_ts_raw,
                    },
                    "$unset": { "value": "", "value_text": "", "encoding": "" },
                    "$setOnInsert": { "key": key_filter.clone() }
                };
                self.run_on_runtime(async move {
                    collection
                        .update_one(key_filter, update, UpdateOptions::builder().upsert(true).build())
                        .await
                })
                .await?;
                Ok(StorageInsertionResult::Deleted)
            }
            Err(e) => Err(format!("MongoDB DELETE failed: {e}").into()),
        }
    }

    async fn get_all_entries(&self) -> ZResult<Vec<(Option<OwnedKeyExpr>, Timestamp)>> {
        let collection = self.collection.clone();
        let docs = self
            .run_on_runtime(async move {
                let mut cursor = collection
                    .find(
                        doc! { "deleted": { "$ne": true } },
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

