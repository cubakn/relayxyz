use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use secp256k1::{Keypair, SECP256K1, SecretKey};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tungstenite::Message;

use relayxyz::config::Config;
use relayxyz::connection;
use relayxyz::db::Db;
use relayxyz::relay::Relay;

pub const TEST_RELAY_URL: &str = "ws://test.relay/";

pub struct TestRelay {
    pub addr: SocketAddr,
    pub admin_key: String,
    require_auth: bool,
    _db_file: NamedTempFile,
    _shutdown_tx: tokio::sync::watch::Sender<bool>,
}

impl TestRelay {
    pub async fn start_open() -> Self {
        Self::start_with_auth(false).await
    }

    pub async fn start() -> Self {
        Self::start_with_auth(true).await
    }

    pub async fn start_with_overrides(
        require_auth: bool,
        configure: impl FnOnce(&mut Config),
    ) -> Self {
        let db_file = NamedTempFile::new().expect("failed to create temp db file");
        let db_path = db_file.path().to_str().unwrap().to_string();
        let admin_key = "test-admin-key".to_string();

        let mut config = Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            db_path,
            admin_key: Some(admin_key.clone()),
            require_auth,
            relay_url: Some(TEST_RELAY_URL.to_string()),
            name: "test-relay".into(),
            description: "test relay".into(),
            pubkey: String::new(),
            contact: String::new(),
            allowed_kinds: vec![0, 1, 2, 3, 4, 5, 6, 7, 16, 1111, 9735, 10000, 10001, 10002],
            max_content_graphemes: 180,
            max_subscriptions: 20,
            max_message_length: 65536,
            default_query_limit: 500,
            icon_url: "http://localhost:7777/public/logo".into(),
            min_event_interval_ms: 0,
            abuse_strike_limit: 10,
            abuse_strike_window_secs: 60,
            abuse_suspend_secs: 300,
            payments_url: None,
            admission_fee_msats: None,
        };
        configure(&mut config);

        let db = Db::open(config.db_path.as_str()).expect("failed to open db");
        let relay = Arc::new(Relay::new(config, db));
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind");
        let addr = listener.local_addr().unwrap();

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        tokio::spawn({
            let relay = relay.clone();
            let mut shutdown_rx = shutdown_rx.clone();
            async move {
                loop {
                    tokio::select! {
                        result = listener.accept() => {
                            let (stream, _addr) = match result {
                                Ok(v) => v,
                                Err(_) => continue,
                            };
                            let relay = relay.clone();
                            let mut shutdown_watch = shutdown_rx.clone();
                            tokio::spawn(async move {
                                let io = TokioIo::new(stream);
                                let service = service_fn(move |req| {
                                    let relay = relay.clone();
                                    async move { connection::handle_request(req, relay).await }
                                });
                                let mut conn = std::pin::pin!(
                                    http1::Builder::new()
                                        .serve_connection(io, service)
                                        .with_upgrades()
                                );
                                loop {
                                    tokio::select! {
                                        result = conn.as_mut() => {
                                            if let Err(e) = result {
                                                let s = e.to_string();
                                                if !s.contains("early eof")
                                                    && !s.contains("connection reset")
                                                    && !s.contains("broken pipe") {
                                                    eprintln!("test conn error: {e}");
                                                }
                                            }
                                            break;
                                        }
                                        _ = shutdown_watch.changed() => {
                                            conn.as_mut().graceful_shutdown();
                                        }
                                    }
                                }
                            });
                        }
                        _ = shutdown_rx.changed() => {
                            break;
                        }
                    }
                }
            }
        });

        Self {
            addr,
            admin_key,
            require_auth,
            _db_file: db_file,
            _shutdown_tx: shutdown_tx,
        }
    }

    async fn start_with_auth(require_auth: bool) -> Self {
        Self::start_with_overrides(require_auth, |_| {}).await
    }

    pub async fn connect(&self) -> TestClient {
        let url = format!("ws://{}/", self.addr);
        let (ws, _) = tokio_tungstenite::connect_async(&url)
            .await
            .expect("ws connect failed");
        let (sink, mut stream) = ws.split();

        let challenge = if self.require_auth {
            match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    let val: Value = serde_json::from_str(&text).expect("AUTH parse failed");
                    assert_eq!(val[0], "AUTH", "first message should be AUTH challenge");
                    val[1].as_str().unwrap().to_string()
                }
                other => panic!("expected AUTH challenge, got: {other:?}"),
            }
        } else {
            String::new()
        };

        TestClient {
            sink,
            stream,
            challenge,
        }
    }

    pub async fn authed_client(&self, secret_hex: &str) -> TestClient {
        let (pubkey, _) = keypair_from_hex(secret_hex);
        self.admin_add(&pubkey).await;
        let mut client = self.connect().await;
        let replies = client.authenticate(secret_hex).await;
        assert_eq!(replies[0][2], true, "auth should succeed");
        client
    }

    pub async fn admin_add(&self, pubkey: &str) -> u16 {
        admin_request(self.addr, "POST", &self.admin_key, pubkey).await
    }

    pub async fn admin_request(&self, method: &str, key: &str, pubkey: &str) -> u16 {
        admin_request(self.addr, method, key, pubkey).await
    }

    pub async fn admin_request_no_auth(&self) -> u16 {
        admin_request_no_auth(self.addr).await
    }

    pub async fn admin_snapshot(&self) -> (u16, String) {
        admin_snapshot_request(self.addr, &self.admin_key).await
    }
}

type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WsStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

pub struct OkReply {
    pub id: String,
    pub accepted: bool,
    pub message: String,
}

pub struct TestClient {
    sink: WsSink,
    stream: WsStream,
    pub challenge: String,
}

impl TestClient {
    pub async fn authenticate(&mut self, secret_hex: &str) -> Vec<Value> {
        let auth_event = sign_auth_event(secret_hex, &self.challenge, TEST_RELAY_URL);
        let msg = serde_json::to_string(&("AUTH", &auth_event)).unwrap();
        self.send(&msg).await
    }

    /// Publish event, assert accepted, return event id.
    pub async fn publish(&mut self, event: &Value) -> String {
        let ok = self.publish_ok(event).await;
        assert!(ok.accepted, "event should be accepted: {}", ok.message);
        ok.id
    }

    /// Publish event, return the OK reply for inspection.
    pub async fn publish_ok(&mut self, event: &Value) -> OkReply {
        let msg = serde_json::to_string(&("EVENT", event)).unwrap();
        self.sink.send(Message::Text(msg.into())).await.unwrap();
        let replies = self
            .recv_until(|v| v[0] == "OK", Duration::from_secs(2))
            .await;
        let ok = replies
            .iter()
            .find(|r| r[0] == "OK")
            .expect("should get OK reply");
        OkReply {
            id: ok[1].as_str().unwrap().to_string(),
            accepted: ok[2].as_bool().unwrap(),
            message: ok[3].as_str().unwrap_or("").to_string(),
        }
    }

    /// Query with filter, return just the event objects (no envelope).
    pub async fn query(&mut self, sub_id: &str, filter: Value) -> Vec<Value> {
        let req = serde_json::to_string(&("REQ", sub_id, filter)).unwrap();
        self.sink.send(Message::Text(req.into())).await.unwrap();
        let replies = self
            .recv_until(|v| v[0] == "EOSE" && v[1] == sub_id, Duration::from_secs(2))
            .await;
        assert!(
            replies.iter().any(|r| r[0] == "EOSE" && r[1] == sub_id),
            "should get EOSE for {sub_id}, got: {replies:?}"
        );
        replies
            .into_iter()
            .filter(|r| r[0] == "EVENT" && r[1] == sub_id)
            .map(|r| r[2].clone())
            .collect()
    }

    /// Wait for a broadcast EVENT on the given subscription.
    pub async fn expect_event(&mut self, sub_id: &str) -> Value {
        let replies = self
            .recv_until(
                |v| v[0] == "EVENT" && v[1] == sub_id,
                Duration::from_secs(2),
            )
            .await;
        let event_msg = replies
            .iter()
            .find(|r| r[0] == "EVENT" && r[1] == sub_id)
            .expect("should receive broadcast EVENT");
        event_msg[2].clone()
    }

    /// Collect messages until predicate matches, with a hard deadline.
    pub async fn recv_until(
        &mut self,
        predicate: impl Fn(&Value) -> bool,
        timeout: Duration,
    ) -> Vec<Value> {
        let mut replies = Vec::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        let done = predicate(&val);
                        replies.push(val);
                        if done {
                            break;
                        }
                    }
                }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        replies
    }

    /// Send raw message, collect replies.
    pub async fn send(&mut self, msg: &str) -> Vec<Value> {
        self.sink.send(Message::Text(msg.into())).await.unwrap();
        self.collect(Duration::from_millis(300)).await
    }

    /// Collect pending messages from the stream.
    pub async fn collect(&mut self, timeout: Duration) -> Vec<Value> {
        let mut replies = Vec::new();
        loop {
            match tokio::time::timeout(timeout, self.stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => {
                    if let Ok(val) = serde_json::from_str::<Value>(&text) {
                        replies.push(val);
                    }
                }
                Ok(Some(Ok(_))) => continue,
                _ => break,
            }
        }
        replies
    }
}

pub fn keypair_from_hex(secret_hex: &str) -> (String, Keypair) {
    let secret_bytes = hex::decode(secret_hex).unwrap();
    let sk = SecretKey::from_slice(&secret_bytes).unwrap();
    let kp = Keypair::from_secret_key(SECP256K1, &sk);
    let (xonly, _parity) = kp.x_only_public_key();
    (hex::encode(xonly.serialize()), kp)
}

pub fn sign_event(secret_hex: &str, kind: u32, content: &str, tags: Vec<Vec<String>>) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    sign_event_at(secret_hex, kind, content, tags, now)
}

pub fn sign_event_at(
    secret_hex: &str,
    kind: u32,
    content: &str,
    tags: Vec<Vec<String>>,
    created_at: u64,
) -> Value {
    let (pubkey_hex, kp) = keypair_from_hex(secret_hex);
    let canonical =
        serde_json::to_string(&(0u8, &pubkey_hex, created_at, kind, &tags, content)).unwrap();
    let hash = Sha256::digest(canonical.as_bytes());
    let id_hex = hex::encode(hash);
    let sig = SECP256K1.sign_schnorr_no_aux_rand(hash.as_slice(), &kp);

    serde_json::json!({
        "id": id_hex,
        "pubkey": pubkey_hex,
        "created_at": created_at,
        "kind": kind,
        "tags": tags,
        "content": content,
        "sig": hex::encode(sig.to_byte_array()),
    })
}

pub fn sign_auth_event(secret_hex: &str, challenge: &str, relay_url: &str) -> Value {
    sign_event(
        secret_hex,
        22242,
        "",
        vec![
            vec!["relay".to_string(), relay_url.to_string()],
            vec!["challenge".to_string(), challenge.to_string()],
        ],
    )
}

async fn admin_request(addr: SocketAddr, method: &str, key: &str, pubkey: &str) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let body = format!(r#"{{"pubkey":"{pubkey}"}}"#);
    let request = format!(
        "{method} /admin/pubkey HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {key}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    resp.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

pub async fn admin_snapshot_request(addr: SocketAddr, key: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request = format!(
        "GET /admin/snapshot HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {key}\r\n\
         Connection: close\r\n\
         \r\n"
    );

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf);

    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let body = resp.split("\r\n\r\n").nth(1).unwrap_or("").to_string();

    (status, body)
}

async fn admin_request_no_auth(addr: SocketAddr) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let body = r#"{"pubkey":"0000000000000000000000000000000000000000000000000000000000000000"}"#;
    let request = format!(
        "POST /admin/pubkey HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap();
    let resp = String::from_utf8_lossy(&buf[..n]);

    resp.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
