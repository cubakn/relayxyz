use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tungstenite::Message;

use crate::common::{self, sign_event_at};

const ABUSER_SK: &str = "9a1e56de76e09e3e44e9e98afd6a6ad92f7df08d73c0edf1a0e5b22355870002";

#[tokio::test]
async fn test_abuse_disconnect_and_snapshot() {
    let strike_limit = 5u32;
    let relay = common::TestRelay::start_with_overrides(false, |cfg| {
        cfg.min_event_interval_ms = 1000; // 1s rate limit
        cfg.abuse_strike_limit = strike_limit;
        cfg.abuse_strike_window_secs = 60;
        cfg.abuse_suspend_secs = 300;
    })
    .await;

    let url = format!("ws://{}/", relay.addr);
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    // Send strike_limit + 1 events rapidly (all will be rate-limited except the first)
    let total = strike_limit + 2;
    for i in 0..total {
        let event = sign_event_at(
            ABUSER_SK,
            1,
            &format!("spam{i}"),
            vec![],
            1_000_000 + i as u64,
        );
        let msg = serde_json::to_string(&("EVENT", &event)).unwrap();
        sink.send(Message::Text(msg.into())).await.unwrap();
    }

    // Collect replies until stream ends or timeout
    let mut replies = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(val) = serde_json::from_str::<Value>(&text) {
                    replies.push(val);
                }
            }
            Ok(Some(Ok(Message::Close(_)))) => break,
            Ok(Some(Ok(_))) => continue,
            _ => break,
        }
    }

    // First event should succeed
    let first_ok = replies.iter().find(|r| r[0] == "OK" && r[2] == true);
    assert!(first_ok.is_some(), "first event should be accepted");

    // Should have rate-limited replies
    let rate_limited: Vec<_> = replies
        .iter()
        .filter(|r| {
            r[0] == "OK" && r[2] == false && r[3].as_str().unwrap_or("").contains("rate-limited")
        })
        .collect();
    assert!(
        !rate_limited.is_empty(),
        "should have rate-limited responses"
    );

    // The last OK should be the disconnect message
    let disconnect_msg = replies.iter().rfind(|r| r[0] == "OK" && r[2] == false);
    assert!(
        disconnect_msg.is_some_and(|m| m[3].as_str().unwrap_or("").contains("disconnecting")),
        "final message should indicate disconnect, got: {replies:?}"
    );

    // Check snapshot shows abuse data
    let (status, body) = relay.admin_snapshot().await;
    assert_eq!(status, 200);
    let snapshot: Value = serde_json::from_str(&body).unwrap();
    let abuse = snapshot["abuse"].as_array().expect("abuse array");
    assert!(!abuse.is_empty(), "abuse array should not be empty");

    let record = &abuse[0];
    assert!(record["violations"].as_u64().unwrap() >= strike_limit as u64);
    assert_eq!(record["suspended"], true);
    assert!(record["suspend_remaining_secs"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn test_suspended_pubkey_rejected_before_sig_verify() {
    let relay = common::TestRelay::start_with_overrides(false, |cfg| {
        cfg.min_event_interval_ms = 1000;
        cfg.abuse_strike_limit = 3;
        cfg.abuse_strike_window_secs = 60;
        cfg.abuse_suspend_secs = 300;
    })
    .await;

    // First connection: trigger suspension
    {
        let url = format!("ws://{}/", relay.addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
        let (mut sink, mut stream) = ws.split();

        for i in 0..6u32 {
            let event = sign_event_at(ABUSER_SK, 1, &format!("x{i}"), vec![], 2_000_000 + i as u64);
            let msg = serde_json::to_string(&("EVENT", &event)).unwrap();
            sink.send(Message::Text(msg.into())).await.unwrap();
        }

        // Drain replies
        loop {
            match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                Ok(Some(Ok(Message::Text(_)))) => continue,
                _ => break,
            }
        }
    }

    // Second connection: suspended pubkey should be rejected immediately
    let mut client = relay.connect().await;
    let event = sign_event_at(ABUSER_SK, 1, "after suspend", vec![], 3_000_000);
    let ok = client.publish_ok(&event).await;
    assert!(!ok.accepted);
    assert!(
        ok.message.contains("suspended"),
        "should say suspended, got: {}",
        ok.message
    );
}
