use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::task::JoinSet;
use tungstenite::Message;

use crate::common;

const NUM_KEYS: usize = 50;
const EVENTS_PER_KEY: usize = 200;

#[tokio::test]
#[ignore]
async fn stress_write_throughput() {
    let relay = common::TestRelay::start_open().await;
    let addr = relay.addr;
    let total = NUM_KEYS * EVENTS_PER_KEY;

    let keys: Vec<String> = (0..NUM_KEYS)
        .map(|_| hex::encode(rand::random::<[u8; 32]>()))
        .collect();

    let stored = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let ts_counter = Arc::new(AtomicU64::new(1_000_000));

    eprintln!("\n=== {NUM_KEYS} keys × {EVENTS_PER_KEY} events = {total} ===\n");

    let start = Instant::now();
    let mut tasks = JoinSet::new();

    for key in &keys {
        let sk = key.clone();
        let stored = Arc::clone(&stored);
        let rejected = Arc::clone(&rejected);
        let ts_counter = Arc::clone(&ts_counter);

        tasks.spawn(async move {
            let url = format!("ws://{addr}/");
            let (ws, _) = tokio_tungstenite::connect_async(&url)
                .await
                .expect("connect");
            let (mut sink, mut stream) = ws.split();
            let _ = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;

            let mut events = Vec::with_capacity(EVENTS_PER_KEY);
            for i in 0..EVENTS_PER_KEY {
                let ts = ts_counter.fetch_add(1, Ordering::Relaxed);
                let tags = vec![
                    vec!["t".to_string(), "stress".to_string()],
                    vec!["i".to_string(), i.to_string()],
                ];
                let event = common::sign_event_at(&sk, 1, &format!("s{i}"), tags, ts);
                events.push(serde_json::to_string(&("EVENT", &event)).unwrap());
            }

            for msg in &events {
                sink.feed(Message::Text(msg.clone().into())).await.unwrap();
            }
            sink.flush().await.unwrap();

            let mut ok_count = 0usize;
            while ok_count < EVENTS_PER_KEY {
                match tokio::time::timeout(Duration::from_secs(30), stream.next()).await {
                    Ok(Some(Ok(Message::Text(text)))) => {
                        if let Ok(val) = serde_json::from_str::<Value>(&text) {
                            if val[0] == "OK" {
                                ok_count += 1;
                                if val[2] == true {
                                    stored.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    rejected.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    Ok(Some(Ok(_))) => {}
                    _ => break,
                }
            }
        });
    }

    while tasks.join_next().await.is_some() {}

    let elapsed = start.elapsed();
    let stored_count = stored.load(Ordering::Relaxed);
    let rejected_count = rejected.load(Ordering::Relaxed);
    let rate = stored_count as f64 / elapsed.as_secs_f64();

    eprintln!("--- writes ---");
    eprintln!("  {:.2?} elapsed", elapsed);
    eprintln!("  {stored_count} stored, {rejected_count} rejected");
    eprintln!("  {rate:.0} events/sec\n");

    let url = format!("ws://{addr}/");
    let (ws, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    let (mut sink, mut stream) = ws.split();

    let queries = [
        (
            "kind=1 limit=500",
            serde_json::json!({"kinds": [1], "limit": 500}),
        ),
        (
            "#t=stress limit=500",
            serde_json::json!({"#t": ["stress"], "limit": 500}),
        ),
        ("#i=42", serde_json::json!({"#i": ["42"], "limit": 500})),
    ];

    eprintln!("--- queries ---");
    for (label, filter) in &queries {
        let req = serde_json::to_string(&("REQ", label, filter)).unwrap();
        let q_start = Instant::now();
        sink.send(Message::Text(req.into())).await.unwrap();

        let mut count = 0usize;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        if val[0] == "EOSE" {
                            break;
                        }
                        if val[0] == "EVENT" {
                            count += 1;
                        }
                    }
                }
                _ => break,
            }
        }
        eprintln!("  {label}: {count} results in {:.2?}", q_start.elapsed());
    }

    eprintln!();
    assert!(stored_count > total / 2, "{stored_count}/{total} stored");
}
