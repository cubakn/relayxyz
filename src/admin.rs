use std::collections::BTreeMap;

use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::db::Db;
use crate::relay::Relay;

#[derive(Deserialize)]
pub struct PubkeyRequest {
    pub pubkey: String,
}

fn verify_admin_token(auth_header: Option<&str>, admin_key: &str) -> Result<(), (u16, String)> {
    let token = auth_header
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or((401, r#"{"error":"unauthorized"}"#.to_string()))?;

    let keys_match: bool = admin_key.as_bytes().ct_eq(token.as_bytes()).into();
    if !keys_match {
        return Err((401, r#"{"error":"unauthorized"}"#.to_string()));
    }
    Ok(())
}

pub fn handle_admin(
    db: &Db,
    method: &str,
    body: &[u8],
    auth_header: Option<&str>,
    admin_key: &str,
) -> Result<String, (u16, String)> {
    verify_admin_token(auth_header, admin_key)?;

    let req: PubkeyRequest =
        serde_json::from_slice(body).map_err(|e| (400, format!(r#"{{"error":"{e}"}}"#)))?;

    if req.pubkey.len() != 64 || hex::decode(&req.pubkey).is_err() {
        return Err((400, r#"{"error":"invalid pubkey hex"}"#.to_string()));
    }

    match method {
        "POST" => {
            db.add_pubkey(&req.pubkey)
                .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?;
            Ok(r#"{"success":true}"#.to_string())
        }
        "DELETE" => {
            db.remove_pubkey(&req.pubkey)
                .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?;
            Ok(r#"{"success":true}"#.to_string())
        }
        _ => Err((405, r#"{"error":"method not allowed"}"#.to_string())),
    }
}

pub fn handle_snapshot(
    relay: &Relay,
    auth_header: Option<&str>,
    admin_key: &str,
) -> Result<String, (u16, String)> {
    verify_admin_token(auth_header, admin_key)?;

    let db = &relay.db;

    let whitelisted_pubkeys = db
        .list_allowed_pubkeys()
        .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?;

    let unique_authors = db
        .list_unique_authors()
        .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?;

    let kind_counts = db
        .count_events_by_kind()
        .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?;

    let total_events: u64 = kind_counts.iter().map(|(_, c)| c).sum();

    let counts_by_kind: BTreeMap<String, u64> = kind_counts
        .iter()
        .map(|(k, c)| (k.to_string(), *c))
        .collect();

    let kind_0_profiles = deserialize_events(
        db.get_events_by_kind(0)
            .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?,
    );
    let kind_1_events = deserialize_events(
        db.get_events_by_kind(1)
            .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?,
    );
    let kind_5_deletions = deserialize_events(
        db.get_events_by_kind(5)
            .map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))?,
    );

    let abuse: Vec<serde_json::Value> = relay
        .abuse_snapshot()
        .into_iter()
        .map(|a| {
            serde_json::json!({
                "pubkey": a.pubkey,
                "violations": a.violations,
                "suspended": a.suspended,
                "suspend_remaining_secs": a.suspend_remaining_secs,
            })
        })
        .collect();

    let response = serde_json::json!({
        "whitelisted_pubkeys": whitelisted_pubkeys,
        "unique_authors": unique_authors,
        "total_events": total_events,
        "counts_by_kind": counts_by_kind,
        "kind_0_profiles": kind_0_profiles,
        "kind_1_events": kind_1_events,
        "kind_5_deletions": kind_5_deletions,
        "abuse": abuse,
    });

    serde_json::to_string(&response).map_err(|e| (500, format!(r#"{{"error":"{e}"}}"#)))
}

fn deserialize_events(raw_events: Vec<Vec<u8>>) -> Vec<serde_json::Value> {
    raw_events
        .into_iter()
        .filter_map(
            |raw| match serde_json::from_slice::<serde_json::Value>(&raw) {
                Ok(val) => Some(val),
                Err(e) => {
                    eprintln!("snapshot: corrupt event: {e}");
                    None
                }
            },
        )
        .collect()
}
