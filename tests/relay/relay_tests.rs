use crate::common::{
    self, TEST_RELAY_URL, keypair_from_hex, sign_auth_event, sign_event, sign_event_at,
};
use serde_json::json;

const ALICE_SK: &str = "9a1e56de76e09e3e44e9e98afd6a6ad92f7df08d73c0edf1a0e5b22355870002";
const BOB_SK: &str = "3c2e0f6d7f4e8a9b1c5d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e1f2a3b";

#[tokio::test]
async fn test_end_to_end() {
    let relay = common::TestRelay::start().await;
    let mut alice = relay.authed_client(ALICE_SK).await;

    let event = sign_event(ALICE_SK, 1, "hello nostr", vec![]);
    let event_id = alice.publish(&event).await;

    let events = alice.query("sub1", json!({"kinds": [1]})).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], event_id);
}

#[tokio::test]
async fn test_auth_enforcement() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    // No auth -> rejected
    let ok = client
        .publish_ok(&sign_event(ALICE_SK, 1, "no auth", vec![]))
        .await;
    assert!(!ok.accepted);
    assert!(ok.message.contains("auth-required"));

    // Auth but not whitelisted -> rejected
    client.authenticate(ALICE_SK).await;
    let ok = client
        .publish_ok(&sign_event(ALICE_SK, 1, "no whitelist", vec![]))
        .await;
    assert!(!ok.accepted);
    assert!(ok.message.contains("not whitelisted"));

    // Auth + whitelisted -> accepted
    relay.admin_add(&alice_pubkey).await;
    let ok = client
        .publish_ok(&sign_event(ALICE_SK, 1, "should work", vec![]))
        .await;
    assert!(ok.accepted);
}

#[tokio::test]
async fn test_admin_auth() {
    let relay = common::TestRelay::start().await;
    let dummy = "a".repeat(64);

    assert_eq!(relay.admin_request_no_auth().await, 401, "no auth -> 401");
    assert_eq!(
        relay.admin_request("POST", "wrong-key", &dummy).await,
        401,
        "bad key -> 401"
    );
    assert_eq!(relay.admin_add(&dummy).await, 200, "correct key -> 200");
}

#[tokio::test]
async fn test_auth_challenge_on_connect() {
    let relay = common::TestRelay::start().await;
    let client = relay.connect().await;

    assert_eq!(
        client.challenge.len(),
        64,
        "challenge should be 64 hex chars"
    );
    assert!(client.challenge.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn test_auth_wrong_challenge() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    relay.admin_add(&alice_pubkey).await;

    let mut client = relay.connect().await;
    let bad_auth = sign_auth_event(ALICE_SK, "wrong_challenge", TEST_RELAY_URL);
    let msg = serde_json::to_string(&("AUTH", &bad_auth)).unwrap();
    let replies = client.send(&msg).await;

    assert_eq!(replies[0][2], false, "wrong challenge should fail");
    assert!(replies[0][3].as_str().unwrap().contains("challenge"));
}

#[tokio::test]
async fn test_auth_wrong_relay_url() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    relay.admin_add(&alice_pubkey).await;

    let mut client = relay.connect().await;
    let bad_auth = sign_auth_event(ALICE_SK, &client.challenge, "wss://wrong.relay/");
    let msg = serde_json::to_string(&("AUTH", &bad_auth)).unwrap();
    let replies = client.send(&msg).await;

    assert_eq!(replies[0][2], false, "wrong relay URL should fail");
    assert!(replies[0][3].as_str().unwrap().contains("relay"));
}

#[tokio::test]
async fn test_auth_expired_timestamp() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    relay.admin_add(&alice_pubkey).await;

    let mut client = relay.connect().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let old_auth = sign_event_at(
        ALICE_SK,
        22242,
        "",
        vec![
            vec!["relay".into(), TEST_RELAY_URL.into()],
            vec!["challenge".into(), client.challenge.clone()],
        ],
        now - 700,
    );
    let msg = serde_json::to_string(&("AUTH", &old_auth)).unwrap();
    let replies = client.send(&msg).await;

    assert_eq!(replies[0][2], false, "expired timestamp should fail");
    assert!(replies[0][3].as_str().unwrap().contains("timestamp"));
}

#[tokio::test]
async fn test_auth_multiple_pubkeys() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let (bob_pubkey, _) = keypair_from_hex(BOB_SK);
    relay.admin_add(&alice_pubkey).await;
    relay.admin_add(&bob_pubkey).await;

    let mut client = relay.connect().await;
    client.authenticate(ALICE_SK).await;
    client.authenticate(BOB_SK).await;

    let ok_a = client
        .publish_ok(&sign_event(ALICE_SK, 1, "from alice", vec![]))
        .await;
    assert!(ok_a.accepted, "Alice should publish");

    let ok_b = client
        .publish_ok(&sign_event(BOB_SK, 1, "from bob", vec![]))
        .await;
    assert!(ok_b.accepted, "Bob should publish");
}

#[tokio::test]
async fn test_kind_22242_via_event_rejected() {
    let relay = common::TestRelay::start().await;
    let mut alice = relay.authed_client(ALICE_SK).await;

    let auth_event = sign_auth_event(ALICE_SK, &alice.challenge, TEST_RELAY_URL);
    let ok = alice.publish_ok(&auth_event).await;
    assert!(!ok.accepted, "kind 22242 via EVENT should be rejected");
    assert!(ok.message.contains("22242"));
}

#[tokio::test]
async fn test_reads_open_without_auth() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut alice = relay.authed_client(ALICE_SK).await;

    let event = sign_event(ALICE_SK, 1, "readable without auth", vec![]);
    let event_id = alice.publish(&event).await;

    let mut reader = relay.connect().await;
    let events = reader
        .query("read", json!({"kinds": [1], "authors": [&alice_pubkey]}))
        .await;
    assert!(!events.is_empty(), "should read without auth");
    assert_eq!(events[0]["id"], event_id);
}

#[tokio::test]
async fn test_protected_event_accepted_with_auth() {
    let relay = common::TestRelay::start().await;
    let mut alice = relay.authed_client(ALICE_SK).await;

    let event = sign_event(ALICE_SK, 1, "protected post", vec![vec!["-".into()]]);
    let event_id = alice.publish(&event).await;

    let events = alice.query("prot", json!({"ids": [&event_id]})).await;
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn test_replaceable_events() {
    let relay = common::TestRelay::start_open().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    for kind in [0u32, 3, 10000, 10001, 10002] {
        let event_a = sign_event_at(ALICE_SK, kind, "", vec![], 1000);
        client.publish(&event_a).await;

        let event_b = sign_event_at(ALICE_SK, kind, "", vec![], 2000);
        let event_b_id = client.publish(&event_b).await;

        let events = client
            .query(
                &format!("k{kind}"),
                json!({"kinds": [kind], "authors": [&alice_pubkey]}),
            )
            .await;
        assert_eq!(
            events.len(),
            1,
            "kind {kind}: should have one event after replacement"
        );
        assert_eq!(
            events[0]["id"], event_b_id,
            "kind {kind}: should be the newer event"
        );
    }
}

#[tokio::test]
async fn test_replaceable_tiebreak() {
    let relay = common::TestRelay::start_open().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    let event_a = sign_event_at(ALICE_SK, 0, r#"{"name":"first"}"#, vec![], 5000);
    let event_a_id = event_a["id"].as_str().unwrap().to_string();
    client.publish(&event_a).await;

    let event_b = sign_event_at(ALICE_SK, 0, r#"{"name":"second"}"#, vec![], 5000);
    let event_b_id = event_b["id"].as_str().unwrap().to_string();
    client.publish(&event_b).await;

    let events = client
        .query("tie", json!({"kinds": [0], "authors": [&alice_pubkey]}))
        .await;
    assert_eq!(events.len(), 1, "should have one event after tiebreak");

    let winner = events[0]["id"].as_str().unwrap();
    let expected = if event_a_id < event_b_id {
        &event_a_id
    } else {
        &event_b_id
    };
    assert_eq!(winner, expected, "lowest id should win");
}

#[tokio::test]
async fn test_regular_kinds_stored() {
    let relay = common::TestRelay::start_open().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    for (kind, content, tags) in [
        (6u32, "", vec![vec!["e".to_string(), "a".repeat(64)]]),
        (16, "", vec![vec!["e".to_string(), "a".repeat(64)]]),
        (
            1111,
            "a comment",
            vec![
                vec!["K".to_string(), "1".to_string()],
                vec!["E".to_string(), "a".repeat(64)],
                vec!["k".to_string(), "1".to_string()],
                vec!["e".to_string(), "a".repeat(64)],
            ],
        ),
    ] {
        let event = sign_event(ALICE_SK, kind, content, tags);
        let event_id = client.publish(&event).await;

        let events = client
            .query(
                &format!("k{kind}"),
                json!({"kinds": [kind], "authors": [&alice_pubkey]}),
            )
            .await;
        assert_eq!(events.len(), 1, "kind {kind}: should be stored");
        assert_eq!(events[0]["id"], event_id, "kind {kind}: should match");
    }
}

#[tokio::test]
async fn test_ephemeral_not_stored() {
    let relay = common::TestRelay::start_open().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 20000, "ephemeral", vec![]);
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "ephemeral event should be OK'd");

    let events = client
        .query("eph", json!({"kinds": [20000], "authors": [&alice_pubkey]}))
        .await;
    assert_eq!(events.len(), 0, "ephemeral should not be stored");
}

#[tokio::test]
async fn test_ephemeral_broadcast() {
    let relay = common::TestRelay::start_open().await;

    let mut sub = relay.connect().await;
    let req = serde_json::to_string(&("REQ", "eph_live", json!({"kinds": [20000]}))).unwrap();
    let replies = sub.send(&req).await;
    assert!(replies.iter().any(|r| r[0] == "EOSE"));

    let mut pub_ = relay.connect().await;
    let event = sign_event(BOB_SK, 20000, "ephemeral broadcast", vec![]);
    let event_id = event["id"].as_str().unwrap().to_string();
    let ok = pub_.publish_ok(&event).await;
    assert!(ok.accepted);

    let received = sub.expect_event("eph_live").await;
    assert_eq!(received["id"], event_id);
}

#[tokio::test]
async fn test_live_broadcast() {
    let relay = common::TestRelay::start_open().await;

    let mut sub = relay.connect().await;
    let req = serde_json::to_string(&("REQ", "live", json!({"kinds": [1]}))).unwrap();
    let replies = sub.send(&req).await;
    assert!(replies.iter().any(|r| r[0] == "EOSE"));

    let mut pub_ = relay.connect().await;
    let event = sign_event(BOB_SK, 1, "broadcast test", vec![]);
    let event_id = event["id"].as_str().unwrap().to_string();
    pub_.publish(&event).await;

    let received = sub.expect_event("live").await;
    assert_eq!(received["id"], event_id);
}

#[tokio::test]
async fn test_nip09_deletion() {
    let relay = common::TestRelay::start_open().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 1, "to be deleted", vec![]);
    let event_id = client.publish(&event).await;

    let del = sign_event(
        ALICE_SK,
        5,
        "delete",
        vec![vec!["e".into(), event_id.clone()]],
    );
    let del_id = client.publish(&del).await;

    let events = client.query("check", json!({"ids": [&event_id]})).await;
    assert_eq!(events.len(), 0, "deleted event should not be returned");

    let events = client
        .query("del", json!({"kinds": [5], "authors": [&alice_pubkey]}))
        .await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], del_id);
}

#[tokio::test]
async fn test_nip09_wrong_pubkey() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 1, "alice's event", vec![]);
    let event_id = client.publish(&event).await;

    let del = sign_event(
        BOB_SK,
        5,
        "delete",
        vec![vec!["e".into(), event_id.clone()]],
    );
    client.publish(&del).await;

    let events = client.query("check", json!({"ids": [&event_id]})).await;
    assert_eq!(events.len(), 1, "Alice's event should survive Bob's delete");
}

#[tokio::test]
async fn test_nip09_delete_delete_noop() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 1, "target", vec![]);
    let event_id = client.publish(&event).await;

    let del1 = sign_event(
        ALICE_SK,
        5,
        "delete original",
        vec![vec!["e".into(), event_id]],
    );
    let del1_id = client.publish(&del1).await;

    let del2 = sign_event(
        ALICE_SK,
        5,
        "delete the delete",
        vec![vec!["e".into(), del1_id.clone()]],
    );
    client.publish(&del2).await;

    let events = client.query("check", json!({"ids": [&del1_id]})).await;
    assert_eq!(events.len(), 1, "kind 5 should survive deletion attempt");
}

#[tokio::test]
async fn test_generic_tag_filter() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(
        ALICE_SK,
        1,
        "tagged",
        vec![vec!["t".into(), "nostr".into()]],
    );
    let event_id = client.publish(&event).await;

    let other = sign_event(
        ALICE_SK,
        1,
        "other",
        vec![vec!["t".into(), "bitcoin".into()]],
    );
    client.publish(&other).await;

    let events = client
        .query("tag", json!({"kinds": [1], "#t": ["nostr"]}))
        .await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], event_id);
}

#[tokio::test]
async fn test_prefix_id_query() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 1, "prefix test", vec![]);
    let event_id = client.publish(&event).await;

    let prefix = &event_id[..8];
    let events = client.query("pfx", json!({"ids": [prefix]})).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["id"], event_id);
}

#[tokio::test]
async fn test_prefix_author_query() {
    let relay = common::TestRelay::start_open().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 1, "author prefix", vec![]);
    let event_id = client.publish(&event).await;

    let prefix = &alice_pubkey[..12];
    let events = client
        .query("apfx", json!({"kinds": [1], "authors": [prefix]}))
        .await;
    assert!(!events.is_empty());
    assert_eq!(events[0]["id"], event_id);
}

#[tokio::test]
async fn test_sub_id_validation() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let replies = client
        .send(&serde_json::to_string(&("REQ", "", json!({"kinds": [1]}))).unwrap())
        .await;
    assert_eq!(replies[0][0], "CLOSED", "empty sub_id -> CLOSED");

    let long_id = "x".repeat(65);
    let replies = client
        .send(&serde_json::to_string(&("REQ", &long_id, json!({"kinds": [1]}))).unwrap())
        .await;
    assert_eq!(replies[0][0], "CLOSED", ">64 char sub_id -> CLOSED");

    let ok_id = "y".repeat(64);
    let replies = client
        .send(&serde_json::to_string(&("REQ", &ok_id, json!({"kinds": [1]}))).unwrap())
        .await;
    assert!(
        replies.iter().any(|r| r[0] == "EOSE"),
        "64-char sub_id should work"
    );
}

#[tokio::test]
async fn test_expired_event_rejected() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(
        ALICE_SK,
        1,
        "already expired",
        vec![vec!["expiration".into(), "1000000000".into()]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted);
    assert!(ok.message.contains("expired"));
}

#[tokio::test]
async fn test_unexpired_event_returned() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let future_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 86400;
    let event = sign_event(
        ALICE_SK,
        1,
        "not expired",
        vec![vec!["expiration".into(), future_ts.to_string()]],
    );
    let event_id = client.publish(&event).await;

    let events = client.query("exp", json!({"ids": [&event_id]})).await;
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn test_expired_event_not_queried() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let event = sign_event(
        ALICE_SK,
        1,
        "will expire",
        vec![vec!["expiration".into(), (now + 1).to_string()]],
    );
    let event_id = client.publish(&event).await;

    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    let events = client.query("expired", json!({"ids": [&event_id]})).await;
    assert_eq!(events.len(), 0, "expired event should not be returned");
}

#[tokio::test]
async fn test_disallowed_kind_rejected() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(ALICE_SK, 9999, "not allowed", vec![]);
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted);
    assert!(ok.message.contains("kind 9999 not accepted"));
}

#[tokio::test]
async fn test_nostr_uri_excluded_from_grapheme_count() {
    let relay = common::TestRelay::start_with_overrides(false, |c| {
        c.max_content_graphemes = 20;
    })
    .await;
    let mut client = relay.connect().await;

    let content = "hello test nostr:nevent1qqsabcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz done";
    let event = sign_event(ALICE_SK, 1, content, vec![]);
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "nostr: URIs should not count toward grapheme limit: {}", ok.message);
}

#[tokio::test]
async fn test_nostr_uri_only_text_counted() {
    let relay = common::TestRelay::start_with_overrides(false, |c| {
        c.max_content_graphemes = 10;
    })
    .await;
    let mut client = relay.connect().await;

    let content = "this is too long nostr:note1abc";
    let event = sign_event(ALICE_SK, 1, content, vec![]);
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted, "real text exceeding limit should still be rejected");
    assert!(ok.message.contains("content exceeds"));
}

#[tokio::test]
async fn test_nostr_uri_multiple_refs_stripped() {
    let relay = common::TestRelay::start_with_overrides(false, |c| {
        c.max_content_graphemes = 20;
    })
    .await;
    let mut client = relay.connect().await;

    let content = "cc nostr:npub1abcdefghijk nostr:note1zyxwvutsrqponmlkjihgfedcba";
    let event = sign_event(ALICE_SK, 1, content, vec![]);
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "multiple nostr: URIs should all be stripped: {}", ok.message);
}

#[tokio::test]
async fn test_nostr_uri_at_end_of_content() {
    let relay = common::TestRelay::start_with_overrides(false, |c| {
        c.max_content_graphemes = 10;
    })
    .await;
    let mut client = relay.connect().await;

    let content = "hey nostr:nevent1qqsabcdef0123456789abcdefghijklmnop";
    let event = sign_event(ALICE_SK, 1, content, vec![]);
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "trailing nostr: URI should be stripped: {}", ok.message);
}

#[tokio::test]
async fn test_repost_embedded_nostr_uri_stripped() {
    let relay = common::TestRelay::start_with_overrides(false, |c| {
        c.max_content_graphemes = 20;
    })
    .await;
    let mut client = relay.connect().await;

    let embedded = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1700000000u64,
        "kind": 1,
        "tags": [],
        "content": "short nostr:nevent1qqsabcdefghijklmnopqrstuvwxyz0123456789abcdef",
        "sig": "c".repeat(128),
    });
    let event = sign_event(
        ALICE_SK,
        6,
        &serde_json::to_string(&embedded).unwrap(),
        vec![vec!["e".to_string(), "a".repeat(64)]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "nostr: URIs in reposted content should be stripped: {}", ok.message);
}

#[tokio::test]
async fn test_repost_short_content_accepted() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let embedded = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1700000000u64,
        "kind": 1,
        "tags": [],
        "content": "short note",
        "sig": "c".repeat(128),
    });
    let event = sign_event(
        ALICE_SK,
        6,
        &serde_json::to_string(&embedded).unwrap(),
        vec![vec!["e".to_string(), "a".repeat(64)]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "repost of short note should be accepted");
}

#[tokio::test]
async fn test_repost_empty_content_accepted() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(
        ALICE_SK,
        6,
        "",
        vec![vec!["e".to_string(), "a".repeat(64)]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(ok.accepted, "tag-only repost should be accepted");
}

#[tokio::test]
async fn test_repost_long_content_rejected() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let long_content = "a".repeat(281);
    let embedded = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1700000000u64,
        "kind": 1,
        "tags": [],
        "content": long_content,
        "sig": "c".repeat(128),
    });
    let event = sign_event(
        ALICE_SK,
        6,
        &serde_json::to_string(&embedded).unwrap(),
        vec![vec!["e".to_string(), "a".repeat(64)]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted, "repost of long note should be rejected");
    assert!(ok.message.contains("reposted event content exceeds"));
}

#[tokio::test]
async fn test_generic_repost_long_content_rejected() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let long_content = "a".repeat(281);
    let embedded = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1700000000u64,
        "kind": 1,
        "tags": [],
        "content": long_content,
        "sig": "c".repeat(128),
    });
    let event = sign_event(
        ALICE_SK,
        16,
        &serde_json::to_string(&embedded).unwrap(),
        vec![vec!["e".to_string(), "a".repeat(64)]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted, "generic repost of long note should be rejected");
    assert!(ok.message.contains("reposted event content exceeds"));
}

#[tokio::test]
async fn test_repost_invalid_json_rejected() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let event = sign_event(
        ALICE_SK,
        6,
        "not valid json",
        vec![vec!["e".to_string(), "a".repeat(64)]],
    );
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted, "repost with invalid JSON content should be rejected");
    assert!(ok.message.contains("valid event JSON"));
}

#[tokio::test]
async fn test_snapshot_auth() {
    let relay = common::TestRelay::start().await;

    let (status, _) = common::admin_snapshot_request(relay.addr, "").await;
    assert_eq!(status, 401);

    let (status, _) = common::admin_snapshot_request(relay.addr, "wrong-key").await;
    assert_eq!(status, 401);

    let (status, body) = relay.admin_snapshot().await;
    assert_eq!(status, 200);
    let val: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    assert!(val["total_events"].is_number());
}

#[tokio::test]
async fn test_snapshot_contents() {
    let relay = common::TestRelay::start_open().await;
    let mut client = relay.connect().await;

    let k0 = sign_event(ALICE_SK, 0, r#"{"name":"alice"}"#, vec![]);
    client.publish(&k0).await;

    let k1 = sign_event(ALICE_SK, 1, "hello snapshot", vec![]);
    client.publish(&k1).await;

    let k7 = sign_event(ALICE_SK, 7, "+", vec![vec!["e".into(), "a".repeat(64)]]);
    client.publish(&k7).await;

    let k5 = sign_event(
        ALICE_SK,
        5,
        "delete",
        vec![vec!["e".into(), "b".repeat(64)]],
    );
    client.publish(&k5).await;

    let k1b = sign_event(BOB_SK, 1, "bob here", vec![]);
    client.publish(&k1b).await;

    let (status, body) = relay.admin_snapshot().await;
    assert_eq!(status, 200);

    let val: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");

    assert_eq!(val["total_events"].as_u64().unwrap(), 5);

    let counts = &val["counts_by_kind"];
    assert_eq!(counts["0"].as_u64().unwrap(), 1);
    assert_eq!(counts["1"].as_u64().unwrap(), 2);
    assert_eq!(counts["5"].as_u64().unwrap(), 1);
    assert_eq!(counts["7"].as_u64().unwrap(), 1);

    assert_eq!(val["kind_0_profiles"].as_array().unwrap().len(), 1);
    assert_eq!(val["kind_1_events"].as_array().unwrap().len(), 2);
    assert_eq!(val["kind_5_deletions"].as_array().unwrap().len(), 1);

    let authors = val["unique_authors"].as_array().unwrap();
    assert_eq!(authors.len(), 2);

    let whitelisted = val["whitelisted_pubkeys"].as_array().unwrap();
    assert_eq!(whitelisted.len(), 0);
}

#[tokio::test]
async fn test_snapshot_with_whitelist() {
    let relay = common::TestRelay::start().await;
    let (alice_pubkey, _) = keypair_from_hex(ALICE_SK);
    let (bob_pubkey, _) = keypair_from_hex(BOB_SK);

    relay.admin_add(&alice_pubkey).await;
    relay.admin_add(&bob_pubkey).await;

    let (status, body) = relay.admin_snapshot().await;
    assert_eq!(status, 200);

    let val: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
    let whitelisted = val["whitelisted_pubkeys"].as_array().unwrap();
    assert_eq!(whitelisted.len(), 2);

    let pubkeys: Vec<&str> = whitelisted.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(pubkeys.contains(&alice_pubkey.as_str()));
    assert!(pubkeys.contains(&bob_pubkey.as_str()));
}
