# 🍃 Zenoh Storage Backend for MongoDB

> **A high-performance, robust storage plugin that bridges [Zenoh](https://zenoh.io/) with MongoDB.**

---

## 🔗 Project Location

**The full source code for this project is available here:**

👉 **[INSERT YOUR GITHUB REPOSITORY LINK HERE]**

To get started, clone the repository:
```bash
git clone [https://github.com/](https://github.com/)[YOUR_USERNAME]/[YOUR_REPO_NAME].git
cd [YOUR_REPO_NAME]
📖 OverviewThis project implements a Storage Backend Plugin for the Zenoh network protocol. It allows Zenoh to persist data (Key-Value pairs) directly into MongoDB collections and retrieve them transparently.Built using Rust, Tokio, and the official MongoDB Driver, this plugin is designed for distributed systems requiring eventual consistency, high throughput, and unstructured data storage.🏗 ArchitectureThis plugin strictly adheres to Zenoh's 3-Layer Storage Architecture, ensuring separation of concerns and efficient resource management:LayerComponentResponsibility1. BackendMongoDbBackendThe Factory: Initializes the plugin and spins up a dedicated Tokio Runtime to isolate database I/O from Zenoh's main routing threads.2. VolumeMongoDbVolumeThe Connector: Manages the connection pool (Client) to a specific MongoDB Database. Supports multiple volume instances for connecting to different DBs simultaneously.3. StorageMongoDbStorageThe Worker: Handles the actual CRUD operations (put, get, delete) on a specific Collection, enforcing data consistency rules.🚀 Key Features🛡️ Distributed Consistency (Last-Write-Wins)The plugin implements LWW (Last-Write-Wins) logic. It compares the incoming Zenoh timestamp with the stored document's timestamp. Older messages arriving late due to network latency are rejected to ensure the database always reflects the latest state.🔄 IdempotencyWrite operations use replace_one with upsert=true. This guarantees that publishing the same message multiple times results in a single, consistent record in the database, preventing duplicates.📦 Hybrid Data StorageBinary Payload: Stores the raw Zenoh payload as BSON Binary (supports images, protobuf, etc.).Smart Metadata: Automatically attempts to decode UTF-8 payloads and stores them in a value_text field, making data human-readable in MongoDB Compass or Atlas.⚡ Async PerformancePowered by a dedicated Tokio Runtime, ensuring that heavy database operations do not block the Zenoh router's critical path.🛠 Tech StackLanguage: Rust 🦀Framework: Zenoh Backend TraitsDatabase: MongoDBRuntime: Tokio (Async/Await)Testing: Testcontainers (Docker-based integration tests)⚙️ ConfigurationTo use this plugin, add the following configuration to your zenoh.json5 file:Code snippet{
  plugins: {
    // 1. Register the Storage Manager
    storage_manager: {
      storages: {
        // 2. Define your Storage Instance
        "my-mongo-storage": {
          // Data matching this pattern will be stored in MongoDB
          key_expr: "demo/mongo/**",
          
          // 3. Configure the Volume
          volume: {
            id: "my-atlas-vol",
            factory: "mongodb_backend", // Must match the plugin name
            
            // Connection Settings passed to the Rust Backend
            mongodb_uri: "mongodb+srv://<user>:<password>@cluster.mongodb.net/?retryWrites=true&w=majority",
            database: "zenoh_app_db",
            collection: "sensor_data"
          }
        }
      }
    }
  }
}
🏃 Usage Example1. Start Zenoh RouterLoad the configuration file to start the router with the plugin enabled:Bashzenohd -c zenoh.json5
2. Publish Data (Put)Using the Zenoh CLI to send data:Bashz_put demo/mongo/home/temp "{'value': 23.5, 'unit': 'C'}"
Effect: A document is upserted into MongoDB. If the key exists, it updates it only if the new timestamp is newer.3. Query Data (Get)Retrieve the data back from storage:Bashz_get demo/mongo/home/temp
Effect: Zenoh fetches the payload directly from MongoDB.4. Delete DataRemove the key from storage:Bashz_delete demo/mongo/home/temp
Effect: The document is removed from the collection.🧪 TestingThis project includes a comprehensive integration test suite using Testcontainers.Functional TestsVerifies CRUD operations, UTF-8 handling, and Timestamp logic:Bashcargo test
Performance BenchmarksRuns Stress (Throughput) and Latency (P99) tests (Requires Docker):Bashcargo test -- --ignored
Stress Test: Simulates 50 concurrent workers flooding the database.Latency Test: Measures P95 and P99 latency percentiles to ensure responsiveness.
