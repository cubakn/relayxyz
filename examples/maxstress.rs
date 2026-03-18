use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt, stream::SplitSink, stream::SplitStream};
use secp256k1::{Keypair, SECP256K1, SecretKey};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio::task::JoinSet;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tungstenite::Message;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

const EVENTS_PER_KEY: usize = 5000;

fn sign_event_at(
    secret_hex: &str,
    kind: u32,
    content: &str,
    tags: Vec<Vec<String>>,
    created_at: u64,
) -> Value {
    let secret_bytes = hex::decode(secret_hex).unwrap();
    let sk = SecretKey::from_slice(&secret_bytes).unwrap();
    let kp = Keypair::from_secret_key(SECP256K1, &sk);
    let (xonly, _) = kp.x_only_public_key();
    let pubkey_hex = hex::encode(xonly.serialize());

    let canonical =
        serde_json::to_string(&(0u8, &pubkey_hex, created_at, kind, &tags, content)).unwrap();
    let hash = Sha256::digest(canonical.as_bytes());
    let id_hex = hex::encode(hash);
    let sig = SECP256K1.sign_schnorr_no_aux_rand(hash.as_slice(), &kp);
    let sig_hex = hex::encode(sig.to_byte_array());

    serde_json::json!({
        "id": id_hex,
        "pubkey": pubkey_hex,
        "created_at": created_at,
        "kind": kind,
        "tags": tags,
        "content": content,
        "sig": sig_hex,
    })
}

struct Stats {
    sent: AtomicU64,
    oks: AtomicU64,
    ok_true: AtomicU64,
    ok_false: AtomicU64,
    errors: AtomicU64,
    queries: AtomicU64,
    query_results: AtomicU64,
}

impl Stats {
    fn new() -> Self {
        Self {
            sent: AtomicU64::new(0),
            oks: AtomicU64::new(0),
            ok_true: AtomicU64::new(0),
            ok_false: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            queries: AtomicU64::new(0),
            query_results: AtomicU64::new(0),
        }
    }
}

async fn connect(url: &str) -> Option<WsStream> {
    match tokio_tungstenite::connect_async(url).await {
        Ok((ws, _)) => Some(ws),
        Err(e) => {
            eprintln!("connect failed: {e}");
            None
        }
    }
}

/// Writer send-half: blast pre-signed events as fast as possible.
/// Zero CPU work during blast — just feeding pre-computed strings.
async fn writer_send(
    mut sink: SplitSink<WsStream, Message>,
    events: Arc<Vec<String>>,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
) {
    const FLUSH_EVERY: usize = 50;
    let mut unflushed = 0usize;

    // Loop over pre-signed events, wrapping around if we exhaust them
    let mut idx = 0;
    while running.load(Ordering::Relaxed) {
        let msg = &events[idx % events.len()];
        idx += 1;

        if sink.feed(Message::Text(msg.clone().into())).await.is_err() {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
        unflushed += 1;
        stats.sent.fetch_add(1, Ordering::Relaxed);

        if unflushed >= FLUSH_EVERY {
            if sink.flush().await.is_err() {
                stats.errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
            unflushed = 0;
        }
    }

    let _ = sink.flush().await;
}

/// Writer recv-half: drain OK messages and count them.
async fn writer_recv(
    mut stream: SplitStream<WsStream>,
    stats: Arc<Stats>,
    running: Arc<AtomicBool>,
) {
    while running.load(Ordering::Relaxed) {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if text.starts_with("[\"OK\"") {
                    stats.oks.fetch_add(1, Ordering::Relaxed);
                    if text.contains(",true,") {
                        stats.ok_true.fetch_add(1, Ordering::Relaxed);
                    } else {
                        stats.ok_false.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(_))) | Ok(None) => break,
            Err(_) => {}
        }
    }

    // Drain remaining OKs after shutdown (2s grace)
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(200), stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                if text.starts_with("[\"OK\"") {
                    stats.oks.fetch_add(1, Ordering::Relaxed);
                    if text.contains(",true,") {
                        stats.ok_true.fetch_add(1, Ordering::Relaxed);
                    } else {
                        stats.ok_false.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            _ => break,
        }
    }
}

/// Reader connection: subscribe, drain results, close sub, repeat.
async fn reader_loop(url: String, stats: Arc<Stats>, running: Arc<AtomicBool>) {
    let mut sub_id: u64 = 0;

    while running.load(Ordering::Relaxed) {
        let ws = match connect(&url).await {
            Some(ws) => ws,
            None => {
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let (mut sink, mut stream) = ws.split();

        let _ = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;

        while running.load(Ordering::Relaxed) {
            sub_id += 1;
            let sub = format!("r{sub_id}");
            let req = serde_json::to_string(&(
                "REQ",
                &sub,
                serde_json::json!({"kinds": [1], "limit": 100}),
            ))
            .unwrap();

            if sink.send(Message::Text(req.into())).await.is_err() {
                break;
            }
            stats.queries.fetch_add(1, Ordering::Relaxed);

            loop {
                match tokio::time::timeout(Duration::from_secs(5), stream.next()).await {
                    Ok(Some(Ok(Message::Text(text)))) => {
                        if text.starts_with("[\"EVENT\"") {
                            stats.query_results.fetch_add(1, Ordering::Relaxed);
                        } else if text.starts_with("[\"EOSE\"") {
                            break;
                        }
                    }
                    Ok(Some(Ok(_))) => {}
                    _ => break,
                }
            }

            let close = serde_json::to_string(&("CLOSE", &sub)).unwrap();
            if sink.send(Message::Text(close.into())).await.is_err() {
                break;
            }
        }
    }
}

/// Reporter: prints stats every second.
async fn reporter(stats: Arc<Stats>, running: Arc<AtomicBool>, start: Instant) {
    let mut prev_sent: u64 = 0;
    let mut prev_oks: u64 = 0;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.tick().await;

    while running.load(Ordering::Relaxed) {
        interval.tick().await;

        let elapsed = start.elapsed().as_secs_f64();
        let sent = stats.sent.load(Ordering::Relaxed);
        let oks = stats.oks.load(Ordering::Relaxed);
        let ok_true = stats.ok_true.load(Ordering::Relaxed);
        let ok_false = stats.ok_false.load(Ordering::Relaxed);
        let errors = stats.errors.load(Ordering::Relaxed);
        let queries = stats.queries.load(Ordering::Relaxed);

        let sent_rate = sent - prev_sent;
        let ok_rate = oks - prev_oks;
        prev_sent = sent;
        prev_oks = oks;

        eprintln!(
            "[{elapsed:5.1}s] sent {sent:>8} ({sent_rate:>6}/s) | ok {oks:>8} ({ok_rate:>6}/s) \
             stored {ok_true} rej {ok_false} err {errors} | queries {queries}"
        );
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let url = args
        .get(1)
        .map(|s| s.as_str())
        .unwrap_or("ws://127.0.0.1:7777");
    let num_connections: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(50);
    let duration_secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(30);

    let num_writers = num_connections * 9 / 10;
    let num_readers = num_connections - num_writers;

    eprintln!("=== maxstress ===");
    eprintln!("target:      {url}");
    eprintln!("connections: {num_connections} ({num_writers} writers, {num_readers} readers)");
    eprintln!("duration:    {duration_secs}s");
    eprintln!();

    // --- Phase 1: Pre-sign all events (CPU-intensive, done before timing) ---
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut ts = now - 86400 * 30; // Start 30 days in the past

    let keys: Vec<String> = (0..num_writers)
        .map(|_| hex::encode(rand::random::<[u8; 32]>()))
        .collect();

    eprintln!(
        "pre-signing {} events ({} keys x {})...",
        num_writers * EVENTS_PER_KEY,
        num_writers,
        EVENTS_PER_KEY
    );
    let sign_start = Instant::now();

    let mut all_events: Vec<Arc<Vec<String>>> = Vec::with_capacity(num_writers);
    for key in &keys {
        let mut events = Vec::with_capacity(EVENTS_PER_KEY);
        for i in 0..EVENTS_PER_KEY {
            let tags = vec![vec!["t".into(), "stress".into()]];
            let event = sign_event_at(key, 1, &format!("s{i}"), tags, ts);
            events.push(serde_json::to_string(&("EVENT", &event)).unwrap());
            ts += 1;
        }
        all_events.push(Arc::new(events));
    }

    eprintln!(
        "pre-signed in {:.2?} ({} events)",
        sign_start.elapsed(),
        num_writers * EVENTS_PER_KEY
    );
    eprintln!();

    // --- Phase 2: Blast (client does near-zero CPU work) ---
    let stats = Arc::new(Stats::new());
    let running = Arc::new(AtomicBool::new(true));

    let start = Instant::now();

    // Spawn reporter
    let r_stats = Arc::clone(&stats);
    let r_running = Arc::clone(&running);
    let reporter_handle = tokio::spawn(reporter(r_stats, r_running, start));

    let mut tasks = JoinSet::new();

    for (i, events) in all_events.iter().enumerate() {
        let url = url.to_string();
        let events = Arc::clone(events);
        let stats = Arc::clone(&stats);
        let running = Arc::clone(&running);

        tasks.spawn(async move {
            // Stagger connects slightly
            if i > 0 {
                tokio::time::sleep(Duration::from_millis(i as u64 * 5)).await;
            }

            let ws = match connect(&url).await {
                Some(ws) => ws,
                None => {
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            let (sink, mut stream) = ws.split();

            let _ = tokio::time::timeout(Duration::from_millis(100), stream.next()).await;

            let send_stats = Arc::clone(&stats);
            let send_running = Arc::clone(&running);
            let send_handle = tokio::spawn(writer_send(sink, events, send_stats, send_running));

            let recv_stats = Arc::clone(&stats);
            let recv_running = Arc::clone(&running);
            let recv_handle = tokio::spawn(writer_recv(stream, recv_stats, recv_running));

            let _ = send_handle.await;
            let _ = recv_handle.await;
        });
    }

    // Spawn reader connections
    for i in 0..num_readers {
        let url = url.to_string();
        let stats = Arc::clone(&stats);
        let running = Arc::clone(&running);

        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_millis((num_writers + i) as u64 * 5)).await;
            reader_loop(url, stats, running).await;
        });
    }

    // Wait for duration
    tokio::time::sleep(Duration::from_secs(duration_secs)).await;
    running.store(false, Ordering::Relaxed);
    eprintln!();
    eprintln!("--- shutting down ---");

    let shutdown_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < shutdown_deadline {
        if tasks.is_empty() {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(200), tasks.join_next()).await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => {}
        }
    }
    tasks.abort_all();

    let _ = reporter_handle.await;

    // Final summary
    let elapsed = start.elapsed();
    let sent = stats.sent.load(Ordering::Relaxed);
    let oks = stats.oks.load(Ordering::Relaxed);
    let ok_true = stats.ok_true.load(Ordering::Relaxed);
    let ok_false = stats.ok_false.load(Ordering::Relaxed);
    let errors = stats.errors.load(Ordering::Relaxed);
    let queries = stats.queries.load(Ordering::Relaxed);
    let query_results = stats.query_results.load(Ordering::Relaxed);

    let send_rate = sent as f64 / elapsed.as_secs_f64();
    let ok_rate = oks as f64 / elapsed.as_secs_f64();

    eprintln!();
    eprintln!("=== results ({:.2?}) ===", elapsed);
    eprintln!("  sent:          {sent:>10}  ({send_rate:>8.0}/s)");
    eprintln!("  oks:           {oks:>10}  ({ok_rate:>8.0}/s)");
    eprintln!("  stored:        {ok_true:>10}");
    eprintln!("  rejected:      {ok_false:>10}");
    eprintln!("  errors:        {errors:>10}");
    eprintln!("  queries:       {queries:>10}");
    eprintln!("  query results: {query_results:>10}");
}
