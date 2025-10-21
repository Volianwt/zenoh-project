# zenoh-project# Eclipse Zenoh - Course Project Extensions

This repository was developed for a university course to explore and implement various extensions for the [Eclipse Zenoh](https://github.com/eclipse-zenoh/zenoh) platform.

This repository will contain one or more "Phase 1 Implementations" and Proofs-of-Concept (PoCs) for Zenoh extensions.

## Current Extension: Real-Time Dashboard

The first extension implemented in this project is a simple, web-based, real-time dashboard for visualizing data within the Zenoh network.

This implementation leverages the `zenoh-plugin-rest` and `zenoh-plugin-storage-manager` plugins to expose Zenoh data via an HTTP API. This API is then consumed by a simple front-end (HTML/JS).

### Initial Code Files

* `my-dashboard-config.json5`: The configuration file for `zenohd`. It is responsible for loading and configuring the `rest` and `storage_manager` plugins.
* `index.html`: (Static Demo) A minimal HTML/JS file that **fetches and displays** data from the Zenoh storage **one time** on page load.
* `index_realtime.html`: (Real-Time Demo) An enhanced version that uses **"Polling"** (`setInterval`) to **continuously fetch** data, simulating a real-time message log.

---

## How to Run the Real-Time Dashboard Demo

Running this demo requires that you have already **compiled** the main `eclipse-zenoh/zenoh` repository on your local machine.

You will need to **run 3 processes**: 2 in your terminal and 1 in your browser.

### 1. Start the Zenoh Server (Terminal 1)

1.  `cd` into your locally compiled `eclipse-zenoh/zenoh` **main repository** directory.
2.  **Copy** the `my-dashboard-config.json5` file from this `zenoh-project` repo into the root of your `eclipse-zenoh/zenoh` directory.
3.  Run the Zenoh router, loading our config file:

    ```bash
    ./target/release/zenohd -c ./my-dashboard-config.json5
    ```
    *(This starts the Zenoh router, the storage manager, and the REST API on port 8000)*

### 2. Start the Data Publisher (Terminal 2)

1.  Open a **new terminal window**.
2.  `cd` into your `eclipse-zenoh/zenoh` **main repository** directory.
3.  Run the `z_pub` example to start publishing data. The `storage_manager` will automatically capture this data:

    ```bash
    # We publish to a key that matches the "demo/example/**" rule in our config
    ./target/release/examples/z_pub -k "demo/example/zenoh-rs-pub" -p "Pub from Rust!"
    ```
    *(The publisher will run continuously, sending a new message with a counter every second)*

### 3. View the Dashboard (Browser)

1.  You do **not** need a web server.
2.  Simply open the `index_realtime.html` file (from this `zenoh-project` repo) in your **browser** using its **file path**.
3.  **Path Example:** `file:///Users/maxwang/Projects/zenoh-project/index_realtime.html`
4.  You will see the "Zenoh Real-Time Dashboard" load, show "Status: Connected ✓", and **automatically refresh every 500ms** to display the latest message from the `z_pub` client.

---

## Future Extensions

This repository may be updated with other Zenoh extensions in the future, such as:
* New storage backend plugins (e.g., a bridge to MySQL).
* New protocol bridge plugins (e.g., a bridge to WebSockets).
* A more advanced dashboard built with React or Vue.