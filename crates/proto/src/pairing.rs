//! First-time pairing with the host's 6 digit code, without sending the code.
//!
//! The client and host run SPAKE2 (a password-authenticated key exchange) with the code as
//! the password, then each proves it derived the same key with an HMAC over a value
//! exported from this connection's TLS session. Consequences:
//! - The code never crosses the network, even encrypted.
//! - Someone in the middle of the first connection (which the client can't yet verify)
//!   gets one online guess per attempt; nothing they see lets them test codes offline.
//!   The host rotates its code after a few failed attempts.
//! - Because the proofs cover the TLS exporter, a relayed connection fails even if the
//!   attacker somehow knew the code.

use anyhow::{Context, Result, anyhow, ensure};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

const CLIENT_ID: &[u8] = b"freemonplay client";
const HOST_ID: &[u8] = b"freemonplay host";
const EXPORTER_LABEL: &[u8] = b"EXPORTER-freemonplay-pairing-v1";

/// One side of a pairing exchange in progress.
pub struct Pairing(Spake2<Ed25519Group>);

impl Pairing {
    /// Client side: returns the state and the message to send in the hello.
    pub fn client(code: &str) -> (Self, Vec<u8>) {
        let (state, msg) = Spake2::<Ed25519Group>::start_a(&password(code), &Identity::new(CLIENT_ID), &Identity::new(HOST_ID));
        (Self(state), msg)
    }

    /// Host side: returns the state and the message to send back.
    pub fn host(code: &str) -> (Self, Vec<u8>) {
        let (state, msg) = Spake2::<Ed25519Group>::start_b(&password(code), &Identity::new(CLIENT_ID), &Identity::new(HOST_ID));
        (Self(state), msg)
    }

    /// Combines with the other side's message. The key only matches the other side's if both
    /// used the same code.
    pub fn finish(self, their_msg: &[u8]) -> Result<SharedKey> {
        let key = self.0.finish(their_msg).map_err(|e| anyhow!("bad pairing message: {e:?}"))?;
        Ok(SharedKey(key))
    }
}

pub struct SharedKey(Vec<u8>);

#[derive(Clone, Copy)]
pub enum Role {
    Client,
    Host,
}

impl SharedKey {
    fn mac(&self, role: Role, binding: &[u8; 32]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.0).expect("HMAC accepts any key length");
        mac.update(match role {
            Role::Client => b"client",
            Role::Host => b"host",
        });
        mac.update(binding);
        mac
    }

    /// Proof that `role` derived this key on this TLS connection.
    pub fn confirmation(&self, role: Role, binding: &[u8; 32]) -> Vec<u8> {
        self.mac(role, binding).finalize().into_bytes().to_vec()
    }

    /// Checks the other side's proof (in constant time).
    pub fn verify(&self, role: Role, binding: &[u8; 32], proof: &[u8]) -> bool {
        self.mac(role, binding).verify_slice(proof).is_ok()
    }
}

/// A value both ends of this TLS session compute identically, and nobody else can: a
/// relayed (man-in-the-middle) connection has a different one on each side.
pub fn channel_binding(conn: &quinn::Connection) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, b"")
        .map_err(|e| anyhow!("TLS exporter unavailable: {e:?}"))
        .context("binding pairing to the connection")?;
    Ok(out)
}

fn password(code: &str) -> Password {
    // Accept "123 456", " 123456 " and so on.
    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    Password::new(digits.as_bytes())
}

/// Runs both sides in memory, for tests.
pub fn simulate(client_code: &str, host_code: &str, client_binding: [u8; 32], host_binding: [u8; 32]) -> Result<()> {
    let (client, client_msg) = Pairing::client(client_code);
    let (host, host_msg) = Pairing::host(host_code);
    let host_key = host.finish(&client_msg)?;
    let host_proof = host_key.confirmation(Role::Host, &host_binding);
    let client_key = client.finish(&host_msg)?;
    ensure!(client_key.verify(Role::Host, &client_binding, &host_proof), "client rejects the host's proof");
    let client_proof = client_key.confirmation(Role::Client, &client_binding);
    ensure!(host_key.verify(Role::Client, &host_binding, &client_proof), "host rejects the client's proof");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_code_same_connection_pairs() {
        simulate("123456", "123456", [7; 32], [7; 32]).unwrap();
        simulate("123 456", "123456", [7; 32], [7; 32]).unwrap();
    }

    #[test]
    fn wrong_code_fails() {
        assert!(simulate("123457", "123456", [7; 32], [7; 32]).is_err());
    }

    #[test]
    fn relayed_connection_fails_even_with_right_code() {
        // A man in the middle has a different TLS session with each side.
        assert!(simulate("123456", "123456", [1; 32], [2; 32]).is_err());
    }

    #[test]
    fn roles_are_not_interchangeable() {
        let (client, cm) = Pairing::client("111111");
        let (host, hm) = Pairing::host("111111");
        let (ck, hk) = (client.finish(&hm).unwrap(), host.finish(&cm).unwrap());
        // A reflected host proof must not pass as a client proof.
        assert!(!hk.verify(Role::Client, &[0; 32], &ck.confirmation(Role::Host, &[0; 32])));
    }
}
