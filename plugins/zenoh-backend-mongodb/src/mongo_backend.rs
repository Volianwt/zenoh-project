use async_trait::async_trait;
use mongodb::{Client, Database};
use serde_json::json;
use std::sync::Arc;
use tokio::runtime::{Builder, Runtime};
use zenoh::Result as ZResult;
use zenoh_backend_traits::config::{StorageConfig, VolumeConfig};
use zenoh_backend_traits::{Capability, Storage, Volume};
use zenoh_plugin_trait::{Plugin, PluginControl};
use zenoh_util::ffi::JsonValue;

use crate::MongoDbStorage;

/// MongoDB backend main struct
pub struct MongoDbBackend {}

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

