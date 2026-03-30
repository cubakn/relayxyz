use std::time::{SystemTime, UNIX_EPOCH};

use secp256k1::{SECP256K1, XOnlyPublicKey, schnorr::Signature};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_segmentation::UnicodeSegmentation;

use crate::error::RelayError;

const MAX_DM_CONTENT_BYTES: usize = 6144;
const MAX_FUTURE_SECONDS: u64 = 600;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl Event {
    pub fn validate(
        &self,
        allowed_kinds: &[u32],
        max_content_graphemes: usize,
    ) -> Result<(), RelayError> {
        if self.kind > 65535 {
            return Err(RelayError::Rejected(format!(
                "kind {} out of range (0-65535)",
                self.kind
            )));
        }
        if !is_ephemeral(self.kind) && !allowed_kinds.contains(&self.kind) {
            return Err(RelayError::Rejected(format!(
                "kind {} not accepted",
                self.kind
            )));
        }
        if self.kind == 1
            && strip_nostr_uris(&self.content).graphemes(true).count() > max_content_graphemes
        {
            return Err(RelayError::Rejected(format!(
                "content exceeds {} grapheme clusters",
                max_content_graphemes
            )));
        }
        if (self.kind == 6 || self.kind == 16) && !self.content.is_empty() {
            let embedded: serde_json::Value =
                serde_json::from_str(&self.content).map_err(|_| {
                    RelayError::Rejected("repost content must be empty or valid event JSON".into())
                })?;
            if let Some(inner) = embedded.get("content").and_then(|v| v.as_str())
                && strip_nostr_uris(inner).graphemes(true).count() > max_content_graphemes
            {
                return Err(RelayError::Rejected(format!(
                    "reposted event content exceeds {} grapheme clusters",
                    max_content_graphemes
                )));
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if self.created_at > now + MAX_FUTURE_SECONDS {
            return Err(RelayError::Rejected(
                "created_at too far in the future".into(),
            ));
        }
        if !is_ephemeral(self.kind) && is_expired(&self.tags, now) {
            return Err(RelayError::Rejected("event expired".into()));
        }
        if self.kind == 4 && self.content.len() > MAX_DM_CONTENT_BYTES {
            return Err(RelayError::Rejected(format!(
                "kind 4 content exceeds {} bytes",
                MAX_DM_CONTENT_BYTES
            )));
        }
        if self.id.len() != 64 || self.pubkey.len() != 64 || self.sig.len() != 128 {
            return Err(RelayError::InvalidEvent("bad field length".into()));
        }
        self.verify_id()?;
        self.verify_sig()?;
        Ok(())
    }

    fn verify_id(&self) -> Result<(), RelayError> {
        let canonical = serde_json::to_string(&(
            0u8,
            &self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &self.content,
        ))?;
        let hash = Sha256::digest(canonical.as_bytes());
        let computed = hex::encode(hash);
        if computed != self.id {
            return Err(RelayError::InvalidEvent("id mismatch".into()));
        }
        Ok(())
    }

    pub fn verify_identity(&self) -> Result<(), RelayError> {
        if self.id.len() != 64 || self.pubkey.len() != 64 || self.sig.len() != 128 {
            return Err(RelayError::InvalidEvent("bad field length".into()));
        }
        self.verify_id()?;
        self.verify_sig()?;
        Ok(())
    }

    fn verify_sig(&self) -> Result<(), RelayError> {
        let id_bytes = hex::decode(&self.id)?;
        let pk_bytes = hex::decode(&self.pubkey)?;
        let pubkey = XOnlyPublicKey::from_slice(&pk_bytes)
            .map_err(|e| RelayError::InvalidEvent(format!("bad pubkey: {e}")))?;
        let sig_bytes = hex::decode(&self.sig)?;
        let sig = Signature::from_slice(&sig_bytes)
            .map_err(|e| RelayError::InvalidEvent(format!("bad sig: {e}")))?;
        SECP256K1
            .verify_schnorr(&sig, &id_bytes, &pubkey)
            .map_err(|_| RelayError::InvalidEvent("signature verification failed".into()))?;
        Ok(())
    }
}

pub fn is_replaceable(kind: u32) -> bool {
    kind == 0 || kind == 3 || (10000..=19999).contains(&kind)
}

pub fn is_ephemeral(kind: u32) -> bool {
    (20000..=29999).contains(&kind)
}

pub fn is_expired(tags: &[Vec<String>], now: u64) -> bool {
    tags.iter().any(|tag| {
        tag.len() >= 2 && tag[0] == "expiration" && tag[1].parse::<u64>().is_ok_and(|ts| now >= ts)
    })
}

pub fn is_protected(tags: &[Vec<String>]) -> bool {
    tags.iter().any(|tag| tag.len() == 1 && tag[0] == "-")
}

fn strip_nostr_uris(content: &str) -> String {
    let mut result = String::new();
    let mut remaining = content;
    while let Some(pos) = remaining.find("nostr:") {
        result.push_str(&remaining[..pos]);
        remaining = &remaining[pos + 6..];
        let end = remaining
            .find(|c: char| !c.is_ascii_lowercase() && !c.is_ascii_digit())
            .unwrap_or(remaining.len());
        remaining = &remaining[end..];
    }
    result.push_str(remaining);
    result
}
