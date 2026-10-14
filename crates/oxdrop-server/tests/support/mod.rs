//! The engine test harness: a real Sender engine on loopback, Items built from
//! real files in a temporary directory, and a real HTTP client playing the Receiver.

// Each test binary uses a different subset of the harness.
#![allow(dead_code)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::Duration;

use oxdrop_core::ItemId;
use oxdrop_server::{BindAddress, EngineConfig, SenderEngine};

pub struct Harness {
    pub engine: SenderEngine,
    pub client: reqwest::Client,
    dir: tempfile::TempDir,
}

impl Harness {
    pub fn new() -> Self {
        let engine = SenderEngine::start(EngineConfig {
            bind: BindAddress::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            port: 0,
        })
        .expect("engine starts on loopback");
        Self {
            engine,
            client: reqwest::Client::new(),
            dir: tempfile::tempdir().expect("temp dir"),
        }
    }

    /// Writes a file into the harness's temporary directory and returns its path.
    pub fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::write(&path, contents).expect("write test file");
        path
    }

    /// Writes a file into a subfolder of the harness's temporary directory.
    pub fn file_in(&self, folder: &str, name: &str, contents: &[u8]) -> PathBuf {
        let dir = self.dir.path().join(folder);
        std::fs::create_dir_all(&dir).expect("create test folder");
        let path = dir.join(name);
        std::fs::write(&path, contents).expect("write test file");
        path
    }

    /// Writes a file and adds it to the open Transfer as an Item.
    pub fn add_file(&self, name: &str, contents: &[u8]) -> ItemId {
        let path = self.file(name, contents);
        self.engine.add_path(&path).expect("add Item").id()
    }

    /// The Item Names in the open Transfer, in order.
    pub fn item_names(&self) -> Vec<String> {
        self.engine
            .state()
            .items
            .into_iter()
            .map(|item| item.name)
            .collect()
    }

    /// The open Transfer's Transfer Link, e.g. `http://127.0.0.1:1234/t/<secret>/`.
    pub fn link(&self) -> String {
        self.engine.state().link.to_string()
    }

    /// GETs a path relative to the Transfer Link, as the Receiver would.
    pub async fn get(&self, relative: &str) -> reqwest::Response {
        let url = format!("{}{}", self.link(), relative);
        self.client.get(url).send().await.expect("request succeeds")
    }

    /// Opens the Receiver page's live event stream.
    pub async fn events(&self) -> Events {
        let response = self.get("events").await;
        assert_eq!(response.status(), 200);
        Events {
            response,
            buffer: String::new(),
        }
    }

    /// Waits until `condition` holds for the engine's state, failing after a few seconds.
    pub async fn eventually(
        &self,
        what: &str,
        condition: impl Fn(&oxdrop_server::TransferState) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !condition(&self.engine.state()) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for: {what}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The Receiver's view of `/events`: server-sent `transfer` events carrying JSON.
pub struct Events {
    response: reqwest::Response,
    buffer: String,
}

impl Events {
    /// The next `transfer` event's data, failing after a few seconds.
    pub async fn next(&mut self) -> serde_json::Value {
        tokio::time::timeout(Duration::from_secs(5), self.next_untimed())
            .await
            .expect("timed out waiting for an event")
    }

    async fn next_untimed(&mut self) -> serde_json::Value {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let mut event = None;
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(name) = line.strip_prefix("event:") {
                        event = Some(name.trim().to_owned());
                    } else if let Some(chunk) = line.strip_prefix("data:") {
                        data.push_str(chunk.trim_start());
                    }
                }
                if event.as_deref() == Some("transfer") {
                    return serde_json::from_str(&data).expect("event data is JSON");
                }
                continue;
            }
            let chunk = self
                .response
                .chunk()
                .await
                .expect("event stream readable")
                .expect("event stream still open");
            self.buffer
                .push_str(std::str::from_utf8(&chunk).expect("UTF-8 events"));
        }
    }

    /// The Item Names in the next `transfer` event.
    pub async fn next_names(&mut self) -> Vec<String> {
        self.next().await["items"]
            .as_array()
            .expect("items array")
            .iter()
            .map(|item| item["name"].as_str().expect("item name").to_owned())
            .collect()
    }
}
