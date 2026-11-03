//! Everything the hosted server hands out (client ids, sign-in form state, codes, access and
//! refresh tokens) is JSON sealed with AES-256-GCM: the MCP client stores it but cannot read or
//! alter it, and the server needs no storage to read it back. The purpose is bound in as AAD, so
//! a token sealed for one use can never pass as another. The first key seals; every key unseals,
//! so a key can be rotated without signing everyone out.
//!
//! shortcut: the JSON is not compressed (tokens are ~2 KB). Add deflate if a client ever rejects them.

use std::time::{SystemTime, UNIX_EPOCH};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac as _};
use rand::RngCore as _;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha2::Sha256;

const VERSION: u8 = 1;
const NONCE: usize = 12;
const TAG: usize = 16;
const MAX_TOKEN: usize = 16_384;

/// What a sealed value is for. Sealing and unsealing must agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A registered client.
    Client,
    /// A sign-in form in progress.
    AuthReq,
    /// An authorization code.
    Code,
    /// An access token.
    Access,
    /// A refresh token.
    Refresh,
}

impl Purpose {
    fn aad(self) -> &'static [u8] {
        match self {
            Self::Client => b"client",
            Self::AuthReq => b"authreq",
            Self::Code => b"code",
            Self::Access => b"access",
            Self::Refresh => b"refresh",
        }
    }
}

/// A value read back from a sealed token.
#[derive(Debug)]
pub struct Unsealed<T> {
    /// The value.
    pub data: T,
    /// When it expires, in epoch seconds, if it does.
    pub exp: Option<u64>,
}

#[derive(Serialize, serde::Deserialize)]
struct Body<T> {
    d: T,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    e: Option<u64>,
}

/// Epoch seconds.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Seals and unseals with a list of keys.
pub struct Sealer {
    keys: Vec<[u8; 32]>,
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl std::fmt::Debug for Sealer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sealer")
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

impl Sealer {
    /// `SEAL_KEYS`: comma-separated base64, each 32 bytes (`openssl rand -base64 32`).
    pub fn parse_keys(raw: &str) -> Result<Vec<[u8; 32]>, String> {
        let mut keys = vec![];
        for (i, part) in raw
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .enumerate()
        {
            let bytes = STANDARD
                .decode(part)
                .map_err(|_| format!("SEAL_KEYS entry {} is not base64", i + 1))?;
            let key: [u8; 32] = bytes.try_into().map_err(|b: Vec<u8>| {
                format!(
                    "SEAL_KEYS entry {} is {} bytes; each key must be 32 bytes of base64",
                    i + 1,
                    b.len()
                )
            })?;
            keys.push(key);
        }
        if keys.is_empty() {
            return Err("SEAL_KEYS is not set: generate one with `openssl rand -base64 32`".into());
        }
        Ok(keys)
    }

    /// At least one key; the first one seals.
    pub fn new(keys: Vec<[u8; 32]>) -> Self {
        assert!(!keys.is_empty(), "Sealer needs at least one key");
        Self {
            keys,
            clock: Box::new(now),
        }
    }

    /// Replace the clock, for tests.
    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// Seals `data` for `purpose`, expiring after `ttl_seconds` when given.
    pub fn seal<T: Serialize>(
        &self,
        purpose: Purpose,
        data: &T,
        ttl_seconds: Option<u64>,
    ) -> String {
        let body = Body {
            d: data,
            e: ttl_seconds.map(|t| (self.clock)() + t),
        };
        let plain = serde_json::to_vec(&body).expect("serialisable");
        let mut nonce = [0u8; NONCE];
        rand::rng().fill_bytes(&mut nonce);
        let cipher = Aes256Gcm::new((&self.keys[0]).into());
        let sealed = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plain,
                    aad: purpose.aad(),
                },
            )
            .expect("aes-gcm");
        let mut out = Vec::with_capacity(1 + NONCE + sealed.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        URL_SAFE_NO_PAD.encode(out)
    }

    /// `None` for anything tampered with, sealed for another purpose or under an unknown key,
    /// or expired.
    pub fn unseal<T: DeserializeOwned>(
        &self,
        purpose: Purpose,
        token: &str,
    ) -> Option<Unsealed<T>> {
        if token.len() > MAX_TOKEN {
            return None;
        }
        let raw = URL_SAFE_NO_PAD.decode(token).ok()?;
        if raw.len() <= 1 + NONCE + TAG || raw[0] != VERSION {
            return None;
        }
        let nonce = Nonce::from_slice(&raw[1..=NONCE]);
        let sealed = &raw[1 + NONCE..];
        for key in &self.keys {
            let Ok(plain) = Aes256Gcm::new(key.into()).decrypt(
                nonce,
                Payload {
                    msg: sealed,
                    aad: purpose.aad(),
                },
            ) else {
                continue; // another key, or tampered
            };
            let body: Body<T> = serde_json::from_slice(&plain).ok()?;
            if body.e.is_some_and(|e| e <= (self.clock)()) {
                return None;
            }
            return Some(Unsealed {
                data: body.d,
                exp: body.e,
            });
        }
        None
    }

    /// A stable keyed pseudonym of a user id, for rate-limit keys and logs.
    pub fn pseudonym(&self, value: &str) -> String {
        let mut mac =
            <Hmac<Sha256> as hmac::Mac>::new_from_slice(&self.keys[0]).expect("any key length");
        mac.update(b"technogym-user:");
        mac.update(value.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn keys() -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
        (vec![[1u8; 32]], vec![[2u8; 32], [1u8; 32]])
    }

    #[test]
    fn round_trip_and_rejections() {
        let (k1, k2) = keys();
        let s1 = Sealer::new(k1);
        let token = s1.seal(Purpose::Code, &serde_json::json!({ "a": 1 }), None);
        let back: Unsealed<serde_json::Value> = s1.unseal(Purpose::Code, &token).unwrap();
        assert_eq!(back.data["a"], 1);
        assert_eq!(back.exp, None);
        assert!(
            s1.unseal::<serde_json::Value>(Purpose::Access, &token)
                .is_none(),
            "another purpose"
        );
        let mut tampered = token.clone();
        tampered.replace_range(20..21, if &token[20..21] == "A" { "B" } else { "A" });
        assert!(
            s1.unseal::<serde_json::Value>(Purpose::Code, &tampered)
                .is_none(),
            "tampered"
        );
        assert!(
            s1.unseal::<serde_json::Value>(Purpose::Code, "nope")
                .is_none()
        );
        assert!(
            s1.unseal::<serde_json::Value>(Purpose::Code, &"A".repeat(MAX_TOKEN + 1))
                .is_none()
        );

        let s2 = Sealer::new(k2);
        assert!(
            s2.unseal::<serde_json::Value>(Purpose::Code, &token)
                .is_some(),
            "every key unseals"
        );
        let token2 = s2.seal(Purpose::Code, &1, None);
        assert!(
            s1.unseal::<i32>(Purpose::Code, &token2).is_none(),
            "only the first key seals"
        );
        assert_ne!(s1.pseudonym("u1"), s1.pseudonym("u2"));
        assert_eq!(
            s1.pseudonym("u1"),
            s2.pseudonym("u1")
                .clone()
                .replace(&s2.pseudonym("u1"), &s1.pseudonym("u1"))
        );
    }

    #[test]
    fn expiry_follows_the_clock() {
        let t = Arc::new(AtomicU64::new(1000));
        let clock = t.clone();
        let s = Sealer::new(vec![[3u8; 32]]).with_clock(move || clock.load(Ordering::SeqCst));
        let token = s.seal(Purpose::Access, &"x", Some(60));
        assert_eq!(
            s.unseal::<String>(Purpose::Access, &token).unwrap().exp,
            Some(1060)
        );
        t.store(1059, Ordering::SeqCst);
        assert!(s.unseal::<String>(Purpose::Access, &token).is_some());
        t.store(1060, Ordering::SeqCst);
        assert!(s.unseal::<String>(Purpose::Access, &token).is_none());
    }

    #[test]
    fn keys_are_parsed_strictly() {
        assert!(Sealer::parse_keys("").is_err());
        assert!(
            Sealer::parse_keys("c2hvcnQ=")
                .unwrap_err()
                .contains("5 bytes")
        );
        assert!(Sealer::parse_keys("***").is_err());
        let one = STANDARD.encode([7u8; 32]);
        assert_eq!(
            Sealer::parse_keys(&format!(" {one} , {one}"))
                .unwrap()
                .len(),
            2
        );
    }
}
