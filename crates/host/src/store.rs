//! Persistent host identity (id + TLS certificate) and the list of paired clients.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize, Deserialize)]
struct Identity {
    host_id: String,
    cert_der_hex: String,
    key_der_hex: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Trusted {
    clients: BTreeMap<String, TrustedClient>,
}

#[derive(Serialize, Deserialize)]
struct TrustedClient {
    name: String,
    /// We only keep a hash, so a leaked config file doesn't let anyone impersonate a client.
    token_sha256: String,
}

pub struct HostStore {
    dir: PathBuf,
    pub host_id: String,
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    trusted: Mutex<Trusted>,
    pairing: Mutex<PairingState>,
}

struct PairingState {
    code: String,
    failures: u32,
}

const MAX_CODE_FAILURES: u32 = 5;

impl HostStore {
    pub fn open(dir: Option<PathBuf>) -> Result<Self> {
        let dir = match dir {
            Some(d) => d,
            None => default_config_dir("host")?,
        };
        std::fs::create_dir_all(&dir)?;

        let id_path = dir.join("identity.json");
        let identity: Identity = match std::fs::read(&id_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("corrupt identity.json")?,
            Err(_) => {
                let key = rcgen::generate_simple_self_signed(vec!["freemonplay".to_string()])?;
                let identity = Identity {
                    host_id: hex(&rand::random::<[u8; 8]>()),
                    cert_der_hex: hex(key.cert.der()),
                    key_der_hex: hex(&key.signing_key.serialize_der()),
                };
                write_private(&id_path, &serde_json::to_vec_pretty(&identity)?)?;
                identity
            }
        };
        let trusted = std::fs::read(dir.join("trusted_clients.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        Ok(Self {
            host_id: identity.host_id,
            cert_der: unhex(&identity.cert_der_hex)?,
            key_der: unhex(&identity.key_der_hex)?,
            dir,
            trusted: Mutex::new(trusted),
            pairing: Mutex::new(PairingState { code: new_code(), failures: 0 }),
        })
    }

    pub fn pairing_code(&self) -> String {
        self.pairing.lock().unwrap().code.clone()
    }

    /// Replaces the pairing code (e.g. after showing it to someone you don't want pairing).
    pub fn new_pairing_code(&self) -> String {
        let mut p = self.pairing.lock().unwrap();
        p.code = new_code();
        p.failures = 0;
        p.code.clone()
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    pub fn paired_count(&self) -> usize {
        self.trusted.lock().unwrap().clients.len()
    }

    pub fn cert_fingerprint(&self) -> String {
        hex(&Sha256::digest(&self.cert_der))
    }

    /// Checks a pairing code. On success returns a fresh token the client should keep.
    /// Too many wrong guesses rotate the code.
    /// Counts a pairing attempt that didn't prove knowledge of the code. Too many rotate the
    /// code, so guessing it online is hopeless.
    pub fn pairing_failed(&self) {
        let mut p = self.pairing.lock().unwrap();
        p.failures += 1;
        if p.failures >= MAX_CODE_FAILURES {
            p.code = new_code();
            p.failures = 0;
            tracing::warn!("too many failed pairing attempts, new code is {}", p.code);
        }
    }

    /// Records a client that proved it knew the code, and returns the token it should keep.
    pub fn complete_pairing(&self, client_id: &str, client_name: &str) -> Vec<u8> {
        self.pairing.lock().unwrap().failures = 0;
        let token = rand::random::<[u8; 32]>().to_vec();
        let mut t = self.trusted.lock().unwrap();
        t.clients.insert(
            client_id.to_string(),
            TrustedClient { name: client_name.to_string(), token_sha256: hex(&Sha256::digest(&token)) },
        );
        if let Err(e) = self.save(&t) {
            tracing::error!("could not save trusted clients: {e:#}");
        }
        token
    }

    pub fn check_token(&self, client_id: &str, token: &[u8]) -> bool {
        let t = self.trusted.lock().unwrap();
        t.clients.get(client_id).is_some_and(|c| c.token_sha256 == hex(&Sha256::digest(token)))
    }

    pub fn forget_all(&self) -> Result<()> {
        let mut t = self.trusted.lock().unwrap();
        t.clients.clear();
        self.save(&t)
    }

    fn save(&self, t: &Trusted) -> Result<()> {
        write_private(&self.dir.join("trusted_clients.json"), &serde_json::to_vec_pretty(t)?)
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

fn new_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000))
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).context("odd hex length")?, 16).map_err(Into::into))
        .collect()
}
