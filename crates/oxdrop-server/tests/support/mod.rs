//! The engine test harness: a real Sender engine on loopback, Items built from
//! real files in a temporary directory, and a real HTTP client playing the Receiver.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

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

    /// Writes a file and adds it to the open Transfer as an Item.
    pub fn add_file(&self, name: &str, contents: &[u8]) -> ItemId {
        let path = self.file(name, contents);
        self.engine.add_path(&path).expect("add Item")
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
}
