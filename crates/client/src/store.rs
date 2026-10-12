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
    /// The host's advertised id, so the host list can show which ones are paired.
    #[serde(default)]
    pub host_id: Option<String>,
}

pub struct ClientStore {
    path: PathBuf,
    data: Data,
}

impl ClientStore {
    pub fn open(dir: Option<PathBuf>) -> Result<Self> {
        let dir = match dir {
            Some(d) => d,
            None => default_config_dir("client")?,
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

    pub fn remember(&mut self, fingerprint: &str, name: &str, host_id: &str, token: &[u8]) -> Result<()> {
        let host = KnownHost { name: name.to_string(), token_hex: hex(token), host_id: Some(host_id.to_string()) };
        self.data.hosts.insert(fingerprint.to_string(), host);
        self.save()
    }

    /// Whether we've paired with the host advertising this id.
    pub fn is_paired(&self, host_id: &str) -> bool {
        self.data.hosts.values().any(|h| h.host_id.as_deref() == Some(host_id))
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

/// This app's config folder. Moves the folder over from the old `com.forwardedu` identifier
/// the first time, so existing pairings and settings carry over.
fn default_config_dir(sub: &str) -> Result<PathBuf> {
    let dir = directories::ProjectDirs::from("io", "github.moridin1138", "freemonplay")
        .context("no home directory")?
        .config_dir()
        .join(sub);
    if !dir.exists()
        && let Some(old) = directories::ProjectDirs::from("com", "forwardedu", "freemonplay")
        && old.config_dir().join(sub).exists()
    {
        let old = old.config_dir().join(sub);
        std::fs::create_dir_all(dir.parent().context("config dir has no parent")?)?;
        std::fs::rename(&old, &dir).with_context(|| format!("moving {} to {}", old.display(), dir.display()))?;
        tracing::info!("moved config from {} to {}", old.display(), dir.display());
    }
    Ok(dir)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
