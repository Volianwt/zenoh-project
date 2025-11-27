

Zenoh Storage Backend for MongoDB
An efficient, robust storage plugin for Zenoh that persists data into MongoDB.

This project implements the zenoh-backend-traits interface, acting as a bridge between the high-performance Zenoh network and MongoDB's document storage. It is designed to handle distributed data consistency, unstructured payloads, and high-concurrency scenarios.

🏗 Architecture
This plugin strictly follows Zenoh's 3-Layer Storage Architecture, ensuring resource efficiency and separation of concerns:

1. Backend Layer (MongoDbBackend)
Role: The Factory & Runtime Manager.

Function: Initializes the plugin and spins up a dedicated Tokio Runtime (Arc<Runtime>).

Why: Zenoh uses its own runtime for routing. We create an isolated Tokio runtime to support the MongoDB driver's async requirements without blocking Zenoh's main threads.

2. Volume Layer (MongoDbVolume)
Role: The Resource Holder.

Function: Manages the connection pool (Client) to a specific MongoDB Database.

Capability: Supports defining multiple Volumes in the configuration to connect to different databases (e.g., Production vs. Test) simultaneously.

3. Storage Layer (MongoDbStorage)
Role: The Executor.

Function: Handles the actual CRUD operations (put, get, delete) on a specific MongoDB Collection. It receives the database connection handle from the Volume layer upon creation.

🚀 Key Features
Idempotency & Upserts: Uses replace_one with upsert=true. Repeated writes of the same key result in a single record, preventing duplicate data entries.

Conflict Resolution (LWW): Implements Last-Write-Wins. The plugin compares the incoming Zenoh timestamp with the stored timestamp. Older messages arriving late (due to network latency) are rejected (StorageInsertionResult::Outdated) to preserve data consistency.

Hybrid Data Storage:

Binary: Stores the raw payload as BSON Binary (supports images, protobuf, etc.).

Text Metadata: Automatically attempts to decode UTF-8 strings and stores them in a value_text field for human readability in MongoDB Compass/Atlas.

Performance Metrics: Includes integration tests for throughput (puts/sec) and latency percentiles (P95/P99).

🛠 Tech Stack
Language: Rust (Safe, Fast, Native to Zenoh)

Database: MongoDB (Flexible JSON/BSON document storage)

Async Runtime: Tokio (Industry-standard async runtime for Rust)

Containerization: testcontainers (For robust integration testing)

⚙️ Configuration
To use this plugin, add it to your zenoh.json5 configuration file.

Code snippet

{
  plugins: {
    // Register the storage manager
    storage_manager: {
      storages: {
        // Define your storage instance
        "my-mongo-storage": {
          // 1. Key Expression: Only data matching this path will be stored
          key_expr: "demo/mongo/**",
          
          // 2. Volume Configuration
          volume: {
            id: "my-atlas-vol",
            factory: "mongodb_backend", // Must match the plugin name
            
            // Backend/Volume settings passed to your Rust code
            mongodb_uri: "mongodb+srv://<user>:<pass>@cluster.mongodb.net/?w=majority",
            database: "zenoh_app_db",
            collection: "sensor_data"
          }
        }
      }
    }
  }
}
🏃 Usage Guide
1. Build the Plugin
Bash

cargo build --release
2. Start Zenoh Router
Run zenohd pointing to your configuration file:

Bash

zenohd -c zenoh.json5
3. Interact via CLI
You can use the standard Zenoh CLI tools (z_put, z_get, z_delete) to interact with the database.

Store Data (Put):

Bash

z_put demo/mongo/sensor/temp "{'val': 23.5, 'unit': 'C'}"
Result: A document is created (or updated) in MongoDB with the payload and timestamp.

Retrieve Data (Get):

Bash

z_get demo/mongo/sensor/temp
Result: Zenoh fetches the latest data directly from MongoDB.

Delete Data:

Bash

z_delete demo/mongo/sensor/temp
Result: The document is removed from the MongoDB collection.

🧪 Testing
This project uses testcontainers for real integration testing (not mocks).

Run Functional Tests
Verifies Put, Get, Delete, Idempotency, and Timestamp logic:

Bash

cargo test
Run Performance Benchmarks
To run the Stress (Throughput) and Latency (P99) tests (requires Docker):

Bash

cargo test -- --ignored
Stress Test: Simulates 50 concurrent workers sending 10,000 messages.

Latency Test: Calculates P50, P95, and P99 latency percentiles to ensure responsiveness.

📂 Project Structure
src/lib.rs: Main library file containing MongoDbBackend, MongoDbVolume, and MongoDbStorage implementations.

tests/: Integration tests using Docker containers.
