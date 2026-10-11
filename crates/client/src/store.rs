//! The client's stable id and the hosts it has paired with.
//!
//! Hosts are keyed by the SHA-256 of their TLS certificate. A host we know is one whose
//! certificate we've seen before, so a different machine claiming the same name or address
//! has to pair again with a code.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Default)]
struct Data {
    client_id: String,
    hosts: BTreeMap<String, KnownHost>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct KnownHost {
    pub name: String,
    pub token_hex: String,
}

pub struct ClientStore {
    path: PathBuf,
    data: Data,
}

impl ClientStore {
    pub fn open(dir: Option<PathBuf>) -> Result<Self> {
        let dir = match dir {
            Some(d) => d,
            None => directories::ProjectDirs::from("com", "forwardedu", "freemonplay")
                .context("no home directory")?
                .config_dir()
                .join("client"),
        };
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("client.json");
        let mut data: Data = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let mut store = Self { path, data: Data::default() };
        if data.client_id.is_empty() {
            data.client_id = hex(&rand::random::<[u8; 16]>());
            store.data = data;
            store.save()?;
        } else {
            store.data = data;
        }
        Ok(store)
    }

    pub fn client_id(&self) -> &str {
        &self.data.client_id
    }

    pub fn host(&self, fingerprint: &str) -> Option<&KnownHost> {
        self.data.hosts.get(fingerprint)
    }

    pub fn remember(&mut self, fingerprint: &str, name: &str, token: &[u8]) -> Result<()> {
        self.data
            .hosts
            .insert(fingerprint.to_string(), KnownHost { name: name.to_string(), token_hex: hex(token) });
        self.save()
    }

    pub fn forget(&mut self, fingerprint: &str) -> Result<()> {
        self.data.hosts.remove(fingerprint);
        self.save()
    }

    fn save(&self) -> Result<()> {
        std::fs::write(&self.path, serde_json::to_vec_pretty(&self.data)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
