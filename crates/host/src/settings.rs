//! User settings that the menu bar app changes and that persist between launches.
//! Command line flags override them for that run only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fmp_proto::Edge;

use serde::{Deserialize, Serialize};

use crate::encoder::CodecChoice;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub codec: CodecChoice,
    /// Frame rate cap; each client also caps it at its own refresh rate.
    pub max_fps: u32,
    /// Apply mouse, keyboard and touch from clients.
    pub allow_input: bool,
    /// Keyboard and mouse mode: which side of the main display each client's monitors sit
    /// on, by client id (default: right).
    pub kvm_sides: BTreeMap<String, Edge>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { codec: CodecChoice::Auto, max_fps: 60, allow_input: true, kvm_sides: BTreeMap::new() }
    }
}

impl Settings {
    fn path(dir: &Path) -> PathBuf {
        dir.join("settings.json")
    }

    pub fn load(dir: &Path) -> Self {
        std::fs::read(Self::path(dir)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) {
        let result = serde_json::to_vec_pretty(self)
            .map_err(anyhow::Error::from)
            .and_then(|json| std::fs::write(Self::path(dir), json).map_err(Into::into));
        if let Err(e) = result {
            tracing::error!("couldn't save settings: {e:#}");
        }
    }
}
