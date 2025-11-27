# 🍃 Zenoh Storage Backend for MongoDB
## 🔗 Project Location

**The core implementation for this course project is located in the following directory:**

👉 [**plugins/zenoh-backend-mongodb**](/plugins/zenoh-backend-mongodb)

> *Note: This directory contains the complete source code (`src/`), integration tests (`tests/`), and build configurations (`Cargo.toml`) developed by our team.*

---

> **A high-performance, robust storage plugin that bridges [Zenoh](https://zenoh.io/) with MongoDB.**

This plugin enables seamless data persistence for Zenoh applications, leveraging MongoDB's document structure to store unstructured IoT data with industrial-grade reliability.

---

## 📖 Overview

This project implements a **Storage Backend Plugin** for the Zenoh network protocol. It allows Zenoh to persist data (Key-Value pairs) directly into MongoDB collections and retrieve them transparently.

Built using **Rust**, **Tokio**, and the official **MongoDB Async Driver**, this plugin is designed for distributed systems requiring eventual consistency, high throughput, and unstructured data storage.

---

## 🏗 Architecture

This plugin strictly adheres to Zenoh's **3-Layer Storage Architecture**, ensuring separation of concerns and efficient resource management:

| Layer | Component | Responsibility |
| :--- | :--- | :--- |
| **1. Backend** | `MongoDbBackend` | **The Factory:** Initializes the plugin and spins up a dedicated **Tokio Runtime** to isolate database I/O from Zenoh's main routing threads. |
| **2. Volume** | `MongoDbVolume` | **The Connector:** Manages the connection pool (`Client`) to a specific MongoDB Database. Supports multiple volume instances for connecting to different DBs simultaneously. |
| **3. Storage** | `MongoDbStorage` | **The Worker:** Handles the actual CRUD operations (`put`, `get`, `delete`) on a specific Collection, enforcing data consistency rules. |

---

## 🚀 Key Features

### 🛡️ Distributed Consistency (Last-Write-Wins)
The plugin implements **LWW (Last-Write-Wins)** logic. It compares the incoming Zenoh timestamp with the stored document's timestamp. Older messages arriving late due to network latency are **rejected** to ensure the database always reflects the latest state.

### 🔄 Idempotency
Write operations use `replace_one` with `upsert=true`. This guarantees that publishing the same message multiple times results in a **single, consistent record** in the database, preventing duplicates.

### 📦 Hybrid Data Storage
* **Binary Payload:** Stores the raw Zenoh payload as BSON Binary (supports images, protobuf, etc.).
* **Smart Metadata:** Automatically attempts to decode UTF-8 payloads and stores them in a `value_text` field, making data human-readable in MongoDB Compass or Atlas.

### ⚡ Async Performance
Powered by a **dedicated Tokio Runtime**, ensuring that heavy database operations do not block the Zenoh router's critical path/event loop.

---

## 🛠 Tech Stack

* **Language:** Rust 🦀
* **Framework:** Zenoh Backend Traits
* **Database:** MongoDB (Official `mongodb` crate)
* **Runtime:** Tokio (Async/Await)
* **Testing:** Testcontainers (Docker-based integration tests)

---

## 📋 Prerequisites

Before running the plugin, ensure you have the following installed:

* **Rust Toolchain:** (Latest stable)
* **Zenoh Router (`zenohd`):** Compatible version.
* **Docker:** Required for running integration tests.
* **MongoDB:** A running instance (Local or Atlas).

---

## ⚙️ Build & Installation

### 1. Clone the Repository
```bash
git clone [https://github.com/Volianwt/zenoh-project.git](https://github.com/Volianwt/zenoh-project.git)
cd zenoh-project/plugins/zenoh-backend-mongodb
2. Build the Plugin
Compile the project in release mode to generate the dynamic library (.so on Linux, .dylib on macOS, .dll on Windows).

Bash

cargo build --release
3. Locate the Library
After building, the library will be located in the target directory (relative to the plugin root):

Linux: ../../target/release/libzenoh_backend_mongodb.so

MacOS: ../../target/release/libzenoh_backend_mongodb.dylib

📝 Configuration
To use this plugin, you must configure zenohd to load it. Add the following to your zenoh.json5 configuration file:

Code snippet

{
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
            // Replace with your actual connection string
            mongodb_uri: "mongodb+srv://<user>:<password>@cluster.mongodb.net/?retryWrites=true&w=majority",
            database: "zenoh_app_db",
            collection: "sensor_data"
          }
        }
      }
    }
  }
}
🏃 Usage Example
1. Start Zenoh Router
Start the router, pointing to your config file. You may need to specify the plugin directory using the -l flag if it's not in the system path.

Bash

# Example assuming you are in the project root
zenohd -c zenoh.json5 -l target/release
2. Publish Data (Put)
Use the Zenoh CLI or any client to send data.

Bash

z_put demo/mongo/home/temp "{'value': 23.5, 'unit': 'C'}"
Effect: A document is upserted into MongoDB. If the key exists, it updates only if the new timestamp is newer.

3. Query Data (Get)
Retrieve the data back from storage.

Bash

z_get demo/mongo/home/temp
Effect: Zenoh fetches the payload directly from MongoDB and returns it.

4. Delete Data
Remove the key from storage.

Bash

z_delete demo/mongo/home/temp
Effect: The document is physically removed from the collection.

🧪 Testing Strategy
This project includes a comprehensive integration test suite using Testcontainers.

Functional Tests
Verifies CRUD operations, UTF-8 handling, and Timestamp (LWW) logic.

Bash

cargo test
Performance Benchmarks
Runs Stress (Throughput) and Latency (P99) tests.

⚠️ Note: These tests spawn a real MongoDB container via Docker and simulate high load.

Bash

cargo test -- --ignored
Stress Test: Simulates 50 concurrent workers flooding the database.

Latency Test: Measures P95 and P99 latency percentiles to ensure responsiveness.
