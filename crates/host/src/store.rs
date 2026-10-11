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
            None => directories::ProjectDirs::from("com", "forwardedu", "freemonplay")
                .context("no home directory")?
                .config_dir()
                .join("host"),
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

    pub fn cert_fingerprint(&self) -> String {
        hex(&Sha256::digest(&self.cert_der))
    }

    /// Checks a pairing code. On success returns a fresh token the client should keep.
    /// Too many wrong guesses rotate the code.
    pub fn pair(&self, client_id: &str, client_name: &str, code: &str) -> Result<Vec<u8>, String> {
        {
            let mut p = self.pairing.lock().unwrap();
            if code.trim() != p.code {
                p.failures += 1;
                if p.failures >= MAX_CODE_FAILURES {
                    p.code = new_code();
                    p.failures = 0;
                    tracing::warn!("too many wrong pairing codes, new code is {}", p.code);
                }
                return Err("wrong pairing code".into());
            }
            p.failures = 0;
        }
        let token = rand::random::<[u8; 32]>().to_vec();
        let mut t = self.trusted.lock().unwrap();
        t.clients.insert(
            client_id.to_string(),
            TrustedClient { name: client_name.to_string(), token_sha256: hex(&Sha256::digest(&token)) },
        );
        if let Err(e) = self.save(&t) {
            tracing::error!("could not save trusted clients: {e:#}");
        }
        Ok(token)
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
