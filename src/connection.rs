use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::task::Poll;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
use tungstenite::Message;
use tungstenite::protocol::WebSocketConfig;

use crate::admin;
use crate::event::{Event, is_ephemeral, is_protected};
use crate::relay::{BroadcastEvent, Relay};
use crate::subscription::{Filter, Subscription};
use crate::writer::WriteResult;

type WsUpgradeFut = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<
                    WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
                    tungstenite::Error,
                >,
            > + Send,
    >,
>;

const MAX_AUTH_WINDOW: u64 = 600;
const MAX_DRAIN: usize = 64;

struct ConnectionState {
    challenge: String,
    authed_pubkeys: HashSet<String>,
    subscriptions: HashMap<String, Subscription>,
    violation_count: u32,
    first_violation: Option<std::time::Instant>,
}

enum HandleResult {
    Immediate(Vec<String>),
    Disconnect(String),
    PendingWrite {
        event: Arc<Event>,
        raw: String,
        event_id: String,
        deletion_targets: Vec<String>,
    },
}

impl ConnectionState {
    fn record_violation(&mut self, relay: &Relay) -> bool {
        let window = std::time::Duration::from_secs(relay.config.abuse_strike_window_secs);
        let now = std::time::Instant::now();

        match self.first_violation {
            Some(first) if now.duration_since(first) > window => {
                self.violation_count = 1;
                self.first_violation = Some(now);
            }
            None => {
                self.violation_count = 1;
                self.first_violation = Some(now);
            }
            _ => {
                self.violation_count += 1;
            }
        }

        self.violation_count >= relay.config.abuse_strike_limit
    }
}

struct PendingEvent {
    event: Arc<Event>,
    raw: String,
    event_id: String,
}

pub async fn handle_request(
    req: Request<Incoming>,
    relay: Arc<Relay>,
) -> Result<Response<Full<Bytes>>, hyper::http::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();

    if (method == hyper::Method::POST || method == hyper::Method::DELETE) && path == "/admin/pubkey"
    {
        let admin_key = match &relay.config.admin_key {
            Some(key) => key,
            None => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from("not found")));
            }
        };
        let method_str = method.as_str().to_string();
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let limited = http_body_util::Limited::new(req.into_body(), 128);
        let body = match http_body_util::BodyExt::collect(limited).await {
            Ok(b) => b.to_bytes(),
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Full::new(Bytes::from(r#"{"error":"bad body"}"#)));
            }
        };
        return match admin::handle_admin(&relay.db, &method_str, &body, auth.as_deref(), admin_key)
        {
            Ok(json) => {
                eprintln!("{method_str} /admin/pubkey -> 200");
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(json)))
            }
            Err((code, json)) => {
                eprintln!("{method_str} /admin/pubkey -> {code}");
                Response::builder()
                    .status(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(json)))
            }
        };
    }

    if method == hyper::Method::GET && path == "/admin/snapshot" {
        let admin_key = match &relay.config.admin_key {
            Some(key) => key,
            None => {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Full::new(Bytes::from("not found")));
            }
        };
        let auth = req
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        return match admin::handle_snapshot(&relay, auth.as_deref(), admin_key) {
            Ok(json) => {
                eprintln!("GET /admin/snapshot -> 200");
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(json)))
            }
            Err((code, json)) => {
                eprintln!("GET /admin/snapshot -> {code}");
                Response::builder()
                    .status(StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
                    .header("content-type", "application/json")
                    .body(Full::new(Bytes::from(json)))
            }
        };
    }

    if method == hyper::Method::GET && path == "/public/logo" {
        return match tokio::fs::read("public/logo.png").await {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "image/png")
                .header("cache-control", "public, max-age=86400")
                .body(Full::new(Bytes::from(bytes))),
            Err(_) => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("logo not found"))),
        };
    }

    if method == hyper::Method::GET && path == "/public/og" {
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "image/png")
            .header("cache-control", "public, max-age=86400")
            .body(Full::new(Bytes::from(relay.og_image.clone())));
    }

    if method == hyper::Method::GET && path == "/public/font" {
        return match tokio::fs::read("public/GeistMono-Regular.ttf").await {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "font/ttf")
                .header("cache-control", "public, max-age=31536000, immutable")
                .body(Full::new(Bytes::from(bytes))),
            Err(_) => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::from("font not found"))),
        };
    }

    if method == hyper::Method::GET && path == "/" {
        if let Some(accept) = req.headers().get("accept").and_then(|v| v.to_str().ok())
            && accept.contains("application/nostr+json")
        {
            return Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/nostr+json")
                .header("access-control-allow-origin", "*")
                .body(Full::new(Bytes::from(relay.nip11.clone())));
        }

        if is_upgrade(&req) {
            return ws_upgrade(req, relay).await;
        }

        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/html; charset=utf-8")
            .body(Full::new(Bytes::from(relay.homepage.clone())));
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::from("not found")))
}

fn is_upgrade(req: &Request<Incoming>) -> bool {
    req.headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

async fn ws_upgrade(
    req: Request<Incoming>,
    relay: Arc<Relay>,
) -> Result<Response<Full<Bytes>>, hyper::http::Error> {
    let max_message_length = relay.config.max_message_length;
    let (response, ws_fut) = match hyper_tungstenite_upgrade(req, max_message_length) {
        Ok(pair) => pair,
        Err(_) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Full::new(Bytes::from("websocket upgrade failed")));
        }
    };

    tokio::spawn(async move {
        match ws_fut.await {
            Ok(ws) => {
                let count = relay.connect();
                eprintln!("ws connected (active: {count})");
                if let Err(e) = handle_ws(ws, &relay).await {
                    eprintln!("ws error: {e}");
                }
                let count = relay.disconnect();
                eprintln!("ws disconnected (active: {count})");
            }
            Err(e) => eprintln!("ws upgrade error: {e}"),
        }
    });

    Ok(response)
}

fn hyper_tungstenite_upgrade(
    req: Request<Incoming>,
    max_message_length: usize,
) -> Result<(Response<Full<Bytes>>, WsUpgradeFut), ()> {
    use hyper::header::{
        CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_VERSION, UPGRADE,
    };
    use tungstenite::handshake::derive_accept_key;

    let key = req.headers().get(SEC_WEBSOCKET_KEY).ok_or(())?.clone();
    let accept = derive_accept_key(key.as_bytes());

    let response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(CONNECTION, "Upgrade")
        .header(UPGRADE, "websocket")
        .header(SEC_WEBSOCKET_ACCEPT, accept)
        .header(SEC_WEBSOCKET_VERSION, "13")
        .body(Full::new(Bytes::new()))
        .map_err(|_| ())?;

    let on_upgrade = hyper::upgrade::on(req);

    let mut ws_config = WebSocketConfig::default();
    ws_config.max_message_size = Some(max_message_length);
    ws_config.max_frame_size = Some(max_message_length);

    let ws_fut = async move {
        let upgraded = on_upgrade.await.map_err(|_| {
            tungstenite::Error::Protocol(tungstenite::error::ProtocolError::HandshakeIncomplete)
        })?;
        let io = TokioIo::new(upgraded);
        let ws = WebSocketStream::from_raw_socket(
            io,
            tungstenite::protocol::Role::Server,
            Some(ws_config),
        )
        .await;
        Ok(ws)
    };

    Ok((response, Box::pin(ws_fut)))
}

fn generate_challenge() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
    hex::encode(bytes)
}

async fn handle_ws(
    ws: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
    relay: &Relay,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut sink, mut stream) = ws.split();
    let mut broadcast_rx = relay.broadcast_tx.subscribe();

    let mut state = ConnectionState {
        challenge: generate_challenge(),
        authed_pubkeys: HashSet::new(),
        subscriptions: HashMap::new(),
        violation_count: 0,
        first_violation: None,
    };

    if relay.config.require_auth {
        let auth_msg = serde_json::to_string(&("AUTH", &state.challenge))?;
        sink.send(Message::Text(auth_msg.into())).await?;
    }

    loop {
        tokio::select! {
            msg = stream.next() => {
                let msg = match msg {
                    Some(Ok(msg)) => msg,
                    Some(Err(e)) => {
                        if !is_normal_close(&e) {
                            eprintln!("ws recv error: {e}");
                        }
                        break;
                    }
                    None => break,
                };

                match msg {
                    Message::Text(text) => {
                        let text_str: &str = &text;
                        match handle_client_message(text_str, relay, &mut state) {
                            HandleResult::Immediate(replies) => {
                                for reply in replies {
                                    sink.feed(Message::Text(reply.into())).await?;
                                }
                                sink.flush().await?;
                            }
                            HandleResult::Disconnect(msg) => {
                                sink.send(Message::Text(msg.into())).await?;
                                sink.send(Message::Close(None)).await?;
                                break;
                            }
                            HandleResult::PendingWrite { event, raw, event_id, deletion_targets } => {
                                // Pipeline: drain additional ready messages before awaiting the writer
                                let mut pending_writes: Vec<PendingEvent> = Vec::new();
                                let mut pending_deletions: Vec<(String, Vec<String>)> = Vec::new();
                                if !deletion_targets.is_empty() {
                                    pending_deletions.push((event.pubkey.clone(), deletion_targets));
                                }

                                pending_writes.push(PendingEvent {
                                    event,
                                    raw,
                                    event_id,
                                });

                                // Drain up to MAX_DRAIN more ready messages without blocking.
                                // Uses poll_fn to preserve the real task waker (unlike now_or_never
                                // which uses a no-op waker and could cause latency stalls).
                                let mut stream_ended = false;
                                for _ in 0..MAX_DRAIN {
                                    let next = std::future::poll_fn(|cx| {
                                        Poll::Ready(stream.poll_next_unpin(cx))
                                    }).await;
                                    match next {
                                        Poll::Ready(Some(Ok(Message::Text(more_text)))) => {
                                            let more_str: &str = &more_text;
                                            match handle_client_message(more_str, relay, &mut state) {
                                                HandleResult::Immediate(replies) => {
                                                    for reply in replies {
                                                        sink.feed(Message::Text(reply.into())).await?;
                                                    }
                                                }
                                                HandleResult::Disconnect(msg) => {
                                                    sink.feed(Message::Text(msg.into())).await?;
                                                    stream_ended = true;
                                                    break;
                                                }
                                                HandleResult::PendingWrite { event, raw, event_id, deletion_targets } => {
                                                    if !deletion_targets.is_empty() {
                                                        pending_deletions.push((event.pubkey.clone(), deletion_targets));
                                                    }
                                                    pending_writes.push(PendingEvent {
                                                        event,
                                                        raw,
                                                        event_id,
                                                    });
                                                }
                                            }
                                        }
                                        Poll::Ready(Some(Ok(Message::Close(_)))) => {
                                            stream_ended = true;
                                            break;
                                        }
                                        Poll::Ready(Some(Ok(Message::Ping(data)))) => {
                                            sink.feed(Message::Pong(data)).await?;
                                        }
                                        Poll::Ready(Some(Ok(_))) => {} // binary, pong — ignore
                                        Poll::Ready(Some(Err(e))) => {
                                            if !is_normal_close(&e) {
                                                eprintln!("ws recv error: {e}");
                                            }
                                            stream_ended = true;
                                            break;
                                        }
                                        Poll::Ready(None) => {
                                            stream_ended = true;
                                            break;
                                        }
                                        Poll::Pending => break, // no more ready messages — stop draining
                                    }
                                }

                                // Run NIP-09 deletions per-pubkey
                                for (pubkey, targets) in &pending_deletions {
                                    match relay.db.delete_events_by_ids(pubkey, targets) {
                                        Ok(deleted) => {
                                            if !deleted.is_empty() {
                                                eprintln!(
                                                    "nip09 deleted {} events for pubkey={}",
                                                    deleted.len(),
                                                    &pubkey[..16]
                                                );
                                            }
                                        }
                                        Err(e) => eprintln!("nip09 delete error: {e}"),
                                    }
                                }

                                if pending_writes.len() == 1 {
                                    // Single event — use original submit path
                                    let pe = pending_writes.pop().unwrap();
                                    let raw_bytes = pe.raw.as_bytes().to_vec();
                                    match relay.writer.submit(pe.event.clone(), raw_bytes).await {
                                        WriteResult::Stored => {
                                            eprintln!("event stored kind={} id={}", pe.event.kind, pe.event_id);
                                            let _ = relay.broadcast_tx.send(Arc::new(BroadcastEvent {
                                                event: pe.event,
                                                raw: Arc::from(pe.raw),
                                            }));
                                            sink.send(Message::Text(ok_msg(&pe.event_id, true, "").into())).await?;
                                        }
                                        WriteResult::Duplicate => {
                                            sink.send(Message::Text(ok_msg(&pe.event_id, true, "duplicate:").into())).await?;
                                        }
                                        WriteResult::Error(e) => {
                                            sink.send(Message::Text(ok_msg(&pe.event_id, false, &format!("error: {e}")).into())).await?;
                                        }
                                    }
                                } else {
                                    // Batch submit
                                    let batch: Vec<(Arc<Event>, Vec<u8>)> = pending_writes
                                        .iter()
                                        .map(|pe| (Arc::clone(&pe.event), pe.raw.as_bytes().to_vec()))
                                        .collect();

                                    let results = relay.writer.submit_batch(batch).await;

                                    for (pe, result) in pending_writes.into_iter().zip(results) {
                                        match result {
                                            WriteResult::Stored => {
                                                eprintln!("event stored kind={} id={}", pe.event.kind, pe.event_id);
                                                let _ = relay.broadcast_tx.send(Arc::new(BroadcastEvent {
                                                    event: pe.event,
                                                    raw: Arc::from(pe.raw),
                                                }));
                                                sink.feed(Message::Text(ok_msg(&pe.event_id, true, "").into())).await?;
                                            }
                                            WriteResult::Duplicate => {
                                                sink.feed(Message::Text(ok_msg(&pe.event_id, true, "duplicate:").into())).await?;
                                            }
                                            WriteResult::Error(e) => {
                                                sink.feed(Message::Text(ok_msg(&pe.event_id, false, &format!("error: {e}")).into())).await?;
                                            }
                                        }
                                    }
                                    sink.flush().await?;
                                }

                                if stream_ended {
                                    break;
                                }
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(data) => {
                        sink.send(Message::Pong(data)).await?;
                    }
                    _ => {}
                }
            }
            event = broadcast_rx.recv() => {
                match event {
                    Ok(broadcast) => {
                        let mut sent = false;
                        for (sub_id, sub) in &state.subscriptions {
                            if sub.matches(&broadcast.event) {
                                let raw_val: Box<serde_json::value::RawValue> =
                                    serde_json::from_str(&broadcast.raw)?;
                                let msg = serde_json::to_string(&("EVENT", sub_id, &raw_val))?;
                                sink.feed(Message::Text(msg.into())).await?;
                                sent = true;
                            }
                        }
                        if sent {
                            sink.flush().await?;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        eprintln!("broadcast lagged, skipped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    Ok(())
}

fn is_normal_close(e: &tungstenite::Error) -> bool {
    matches!(
        e,
        tungstenite::Error::ConnectionClosed
            | tungstenite::Error::Protocol(
                tungstenite::error::ProtocolError::ResetWithoutClosingHandshake
            )
    )
}

fn handle_client_message(text: &str, relay: &Relay, state: &mut ConnectionState) -> HandleResult {
    let parts: Vec<&serde_json::value::RawValue> = match serde_json::from_str(text) {
        Ok(p) => p,
        Err(_) => return HandleResult::Immediate(vec![notice("invalid JSON")]),
    };

    if parts.is_empty() {
        return HandleResult::Immediate(vec![notice("expected JSON array")]);
    }

    let msg_type: String = match serde_json::from_str(parts[0].get()) {
        Ok(s) => s,
        Err(_) => return HandleResult::Immediate(vec![notice("first element must be string")]),
    };

    match msg_type.as_str() {
        "EVENT" => handle_event(&parts, relay, state),
        "REQ" => HandleResult::Immediate(handle_req(&parts, relay, &mut state.subscriptions)),
        "CLOSE" => HandleResult::Immediate(handle_close(&parts, &mut state.subscriptions)),
        "AUTH" => HandleResult::Immediate(handle_auth(&parts, relay, state)),
        _ => HandleResult::Immediate(vec![notice(&format!("unknown message type: {msg_type}"))]),
    }
}

fn handle_event(
    parts: &[&serde_json::value::RawValue],
    relay: &Relay,
    state: &mut ConnectionState,
) -> HandleResult {
    if parts.len() < 2 {
        return HandleResult::Immediate(vec![notice("EVENT requires event object")]);
    }

    let raw = parts[1].get().to_string();
    let event: Event = match serde_json::from_str(&raw) {
        Ok(e) => e,
        Err(e) => return HandleResult::Immediate(vec![notice(&format!("bad event: {e}"))]),
    };

    let event_id = event.id.clone();

    if event.kind == 22242 {
        return HandleResult::Immediate(vec![ok_msg(
            &event_id,
            false,
            "blocked: kind 22242 is ephemeral, use AUTH message",
        )]);
    }

    // Check suspension before expensive sig verify (read-only, safe with unverified pubkey)
    if relay.is_suspended(&event.pubkey) {
        return HandleResult::Immediate(vec![ok_msg(
            &event_id,
            false,
            "rate-limited: pubkey suspended",
        )]);
    }

    if let Err(e) = event.validate(
        &relay.config.allowed_kinds,
        relay.config.max_content_graphemes,
    ) {
        return HandleResult::Immediate(vec![ok_msg(&event_id, false, &e.to_string())]);
    }

    if is_protected(&event.tags) && !state.authed_pubkeys.contains(&event.pubkey) {
        return HandleResult::Immediate(vec![ok_msg(
            &event_id,
            false,
            "auth-required: protected event requires authentication",
        )]);
    }

    if relay.config.require_auth {
        if !state.authed_pubkeys.contains(&event.pubkey) {
            return HandleResult::Immediate(vec![ok_msg(
                &event_id,
                false,
                "auth-required: authenticate to publish",
            )]);
        }
        match relay.db.is_whitelisted(&event.pubkey) {
            Ok(true) => {}
            Ok(false) => {
                return HandleResult::Immediate(vec![ok_msg(
                    &event_id,
                    false,
                    "restricted: pubkey not whitelisted",
                )]);
            }
            Err(e) => {
                return HandleResult::Immediate(vec![ok_msg(
                    &event_id,
                    false,
                    &format!("error: {e}"),
                )]);
            }
        }
    }

    if !relay.check_rate_limit(&event.pubkey) {
        relay.record_abuse(&event.pubkey);
        if state.record_violation(relay) {
            return HandleResult::Disconnect(ok_msg(
                &event_id,
                false,
                "rate-limited: too many violations, disconnecting",
            ));
        }
        return HandleResult::Immediate(vec![ok_msg(
            &event_id,
            false,
            "rate-limited: try again later",
        )]);
    }

    if is_ephemeral(event.kind) {
        let _ = relay.broadcast_tx.send(Arc::new(BroadcastEvent {
            event: Arc::new(event),
            raw: Arc::from(raw),
        }));
        return HandleResult::Immediate(vec![ok_msg(&event_id, true, "")]);
    }

    let deletion_targets = if event.kind == 5 {
        event
            .tags
            .iter()
            .filter(|t| t.len() >= 2 && t[0] == "e")
            .map(|t| t[1].clone())
            .collect()
    } else {
        Vec::new()
    };

    HandleResult::PendingWrite {
        event: Arc::new(event),
        raw,
        event_id,
        deletion_targets,
    }
}

fn handle_auth(
    parts: &[&serde_json::value::RawValue],
    relay: &Relay,
    state: &mut ConnectionState,
) -> Vec<String> {
    if parts.len() < 2 {
        return vec![notice("AUTH requires event object")];
    }

    let event: Event = match serde_json::from_str(parts[1].get()) {
        Ok(e) => e,
        Err(e) => return vec![ok_msg("", false, &format!("bad auth event: {e}"))],
    };

    let event_id = event.id.clone();

    if let Err(msg) = validate_auth_event(&event, &state.challenge, relay) {
        return vec![ok_msg(&event_id, false, &msg)];
    }

    state.authed_pubkeys.insert(event.pubkey.clone());
    eprintln!("auth success pubkey={}", &event.pubkey[..16]);

    vec![ok_msg(&event_id, true, "")]
}

fn validate_auth_event(event: &Event, challenge: &str, relay: &Relay) -> Result<(), String> {
    if event.kind != 22242 {
        return Err("auth event must be kind 22242".into());
    }

    if let Err(e) = event.verify_identity() {
        return Err(format!("invalid: {e}"));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let diff = event.created_at.abs_diff(now);
    if diff > MAX_AUTH_WINDOW {
        return Err("auth event timestamp too far from current time".into());
    }

    let challenge_ok = event
        .tags
        .iter()
        .any(|t| t.len() >= 2 && t[0] == "challenge" && t[1] == challenge);
    if !challenge_ok {
        return Err("challenge tag missing or mismatch".into());
    }

    let relay_url = match &relay.config.relay_url {
        Some(url) => url,
        None => return Err("relay not configured for auth".into()),
    };
    let relay_ok = event
        .tags
        .iter()
        .any(|t| t.len() >= 2 && t[0] == "relay" && t[1] == *relay_url);
    if !relay_ok {
        return Err("relay tag missing or mismatch".into());
    }

    Ok(())
}

fn handle_req(
    parts: &[&serde_json::value::RawValue],
    relay: &Relay,
    subscriptions: &mut HashMap<String, Subscription>,
) -> Vec<String> {
    if parts.len() < 3 {
        return vec![notice("REQ requires sub_id and at least one filter")];
    }

    let sub_id: String = match serde_json::from_str(parts[1].get()) {
        Ok(s) => s,
        Err(_) => return vec![notice("sub_id must be string")],
    };

    if sub_id.is_empty() || sub_id.chars().count() > 64 {
        return vec![closed_msg(
            &sub_id,
            "invalid: subscription id must be 1-64 chars",
        )];
    }

    if subscriptions.len() >= relay.config.max_subscriptions && !subscriptions.contains_key(&sub_id)
    {
        return vec![closed_msg(&sub_id, "too many subscriptions")];
    }

    let mut filters = Vec::new();
    for part in &parts[2..] {
        match serde_json::from_str::<Filter>(part.get()) {
            Ok(f) => {
                if f.is_oversized() {
                    return vec![closed_msg(&sub_id, "filter too broad")];
                }
                filters.push(f);
            }
            Err(e) => return vec![notice(&format!("bad filter: {e}"))],
        }
    }

    let mut replies = Vec::new();
    for filter in &filters {
        match relay.db.query(
            filter,
            &relay.config.allowed_kinds,
            relay.config.default_query_limit,
        ) {
            Ok(events) => {
                for raw in events {
                    match std::str::from_utf8(&raw) {
                        Ok(raw_str) => {
                            match serde_json::from_str::<Box<serde_json::value::RawValue>>(raw_str)
                            {
                                Ok(raw_val) => {
                                    match serde_json::to_string(&("EVENT", &sub_id, &raw_val)) {
                                        Ok(msg) => replies.push(msg),
                                        Err(e) => eprintln!("event serialize error: {e}"),
                                    }
                                }
                                Err(e) => eprintln!("corrupt event json in db: {e}"),
                            }
                        }
                        Err(e) => {
                            eprintln!("invalid utf-8 in stored event: {e}");
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("query error: {e}");
            }
        }
    }

    replies.push(serde_json::to_string(&("EOSE", &sub_id)).unwrap());
    subscriptions.insert(sub_id, Subscription { filters });
    replies
}

fn handle_close(
    parts: &[&serde_json::value::RawValue],
    subscriptions: &mut HashMap<String, Subscription>,
) -> Vec<String> {
    if parts.len() < 2 {
        return vec![notice("CLOSE requires sub_id")];
    }
    let sub_id: String = match serde_json::from_str(parts[1].get()) {
        Ok(s) => s,
        Err(_) => return vec![notice("sub_id must be string")],
    };
    if sub_id.is_empty() || sub_id.chars().count() > 64 {
        return vec![closed_msg(
            &sub_id,
            "invalid: subscription id must be 1-64 chars",
        )];
    }
    subscriptions.remove(&sub_id);
    vec![closed_msg(&sub_id, "")]
}

fn notice(msg: &str) -> String {
    serde_json::to_string(&("NOTICE", msg)).unwrap()
}

fn ok_msg(id: &str, success: bool, msg: &str) -> String {
    serde_json::to_string(&("OK", id, success, msg)).unwrap()
}

fn closed_msg(sub_id: &str, msg: &str) -> String {
    serde_json::to_string(&("CLOSED", sub_id, msg)).unwrap()
}
