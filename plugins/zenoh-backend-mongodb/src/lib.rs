mod mongo_backend;
mod storage;
mod types;

pub use mongo_backend::{MongoDbBackend, MongoDbVolume};
pub use storage::MongoDbStorage;
pub use types::{PageRequest, StorageEntry};

#[cfg(all(feature = "dynamic_plugin", not(test)))]
zenoh_plugin_trait::declare_plugin!(MongoDbBackend);
