use async_trait::async_trait;
use futures::future::FutureExt;
use mongodb::{
    bson::{doc, spec::BinarySubtype, Binary, Bson, Document},
    Client, Collection, Database,
};
use serde_json::json;
use std::{borrow::Cow, str::FromStr, sync::Arc};
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

#[cfg(feature = "dynamic_plugin")]
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
        let collection = self.database.collection::<Document>(&collection_name);
        Ok(Box::new(MongoDbStorage {
            collection,
            collection_name,
            runtime: self.runtime.clone(),
        }))
    }
}

impl PluginControl for MongoDbVolume {}

pub struct MongoDbStorage {
    collection: Collection<Document>,
    collection_name: String,
    runtime: Arc<Runtime>,
}

impl MongoDbStorage {
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
        let key_bson = match key {
            Some(k) => Bson::String(k.to_string()),
            None => Bson::Null,
        };
        let payload = value.to_bytes().into_owned();
        let value_text = std::str::from_utf8(&payload).map(|s| s.to_owned()).ok();
        let encoding_str: Cow<'static, str> = (&encoding).into();
        let mut document = doc! {
            "key": key_bson,
            "value": Bson::Binary(Binary { subtype: BinarySubtype::Generic, bytes: payload.clone() }),
            "encoding": encoding_str.as_ref(),
            "timestamp": timestamp.to_string(),
        };
        if let Some(text) = value_text {
            document.insert("value_text", text);
        }
        let collection = self.collection.clone();
        self.run_on_runtime(async move {
            collection
                .insert_one(document, None)
                .await
                .map(|_| StorageInsertionResult::Inserted)
        })
        .await
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
        _timestamp: Timestamp,
    ) -> ZResult<StorageInsertionResult> {
        let key_bson = match key {
            Some(k) => Bson::String(k.to_string()),
            None => Bson::Null,
        };
        let filter = doc! { "key": key_bson };
        let collection = self.collection.clone();
        self.run_on_runtime(async move {
            collection
                .delete_one(filter, None)
                .await
                .map(|_| StorageInsertionResult::Deleted)
        })
        .await
    }

    async fn get_all_entries(&self) -> ZResult<Vec<(Option<OwnedKeyExpr>, Timestamp)>> {
        Ok(vec![]) // Placeholder: could be extended to iterate all documents
    }
}
