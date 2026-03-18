use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::config::Config;
use crate::db::Db;
use crate::event::Event;
use crate::writer::BatchWriter;

pub struct BroadcastEvent {
    pub event: Arc<Event>,
    pub raw: Arc<str>,
}

struct AbuseRecord {
    violations: u32,
    first_violation: Instant,
    suspended_until: Option<Instant>,
}

pub struct AbuseSnapshot {
    pub pubkey: String,
    pub violations: u32,
    pub suspended: bool,
    pub suspend_remaining_secs: u64,
}

pub struct Relay {
    pub db: Arc<Db>,
    pub writer: BatchWriter,
    pub broadcast_tx: broadcast::Sender<Arc<BroadcastEvent>>,
    pub config: Config,
    pub nip11: String,
    pub homepage: String,
    pub og_image: Vec<u8>,
    pub ws_connections: AtomicUsize,
    rate_limits: Mutex<HashMap<String, Instant>>,
    abuse_records: Mutex<HashMap<String, AbuseRecord>>,
}

impl Relay {
    pub fn new(config: Config, db: Db) -> Self {
        let db = Arc::new(db);
        let writer = BatchWriter::new(Arc::clone(&db), 256, Duration::from_millis(5));
        let (broadcast_tx, _) = broadcast::channel(4096);
        let nip11 = crate::nip11::nip11_json(&config);
        let og_image = crate::og::generate(&config);
        let homepage = crate::homepage::homepage_html(&config);
        Self {
            db,
            writer,
            broadcast_tx,
            config,
            nip11,
            homepage,
            og_image,
            ws_connections: AtomicUsize::new(0),
            rate_limits: Mutex::new(HashMap::new()),
            abuse_records: Mutex::new(HashMap::new()),
        }
    }

    pub fn connect(&self) -> usize {
        self.ws_connections.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn disconnect(&self) -> usize {
        self.ws_connections
            .fetch_sub(1, Ordering::Relaxed)
            .saturating_sub(1)
    }

    pub fn check_rate_limit(&self, pubkey: &str) -> bool {
        let interval = Duration::from_millis(self.config.min_event_interval_ms);
        if interval.is_zero() {
            return true;
        }
        let now = Instant::now();
        let mut limits = self.rate_limits.lock().unwrap();
        if let Some(last) = limits.get(pubkey)
            && now.duration_since(*last) < interval
        {
            return false;
        }
        limits.insert(pubkey.to_string(), now);
        true
    }

    pub fn record_abuse(&self, pubkey: &str) {
        let window = Duration::from_secs(self.config.abuse_strike_window_secs);
        let now = Instant::now();
        let mut records = self.abuse_records.lock().unwrap();

        let record = records.entry(pubkey.to_string()).or_insert(AbuseRecord {
            violations: 0,
            first_violation: now,
            suspended_until: None,
        });

        if now.duration_since(record.first_violation) > window {
            record.violations = 0;
            record.first_violation = now;
        }

        record.violations += 1;

        if record.violations >= self.config.abuse_strike_limit {
            let suspend = Duration::from_secs(self.config.abuse_suspend_secs);
            record.suspended_until = Some(now + suspend);
            eprintln!(
                "abuse: suspended pubkey={} for {}s ({} violations)",
                &pubkey[..pubkey.len().min(16)],
                self.config.abuse_suspend_secs,
                record.violations
            );
        }

        if records.len() > 1000 {
            records.retain(|_, r| {
                r.suspended_until.is_some_and(|t| t > now)
                    || now.duration_since(r.first_violation) <= window
            });
        }
    }

    pub fn is_suspended(&self, pubkey: &str) -> bool {
        let records = self.abuse_records.lock().unwrap();
        match records.get(pubkey) {
            Some(r) => r.suspended_until.is_some_and(|t| t > Instant::now()),
            None => false,
        }
    }

    pub fn abuse_snapshot(&self) -> Vec<AbuseSnapshot> {
        let now = Instant::now();
        let records = self.abuse_records.lock().unwrap();
        records
            .iter()
            .filter(|(_, r)| r.violations > 0)
            .map(|(pubkey, r)| {
                let suspended = r.suspended_until.is_some_and(|t| t > now);
                let remaining = r
                    .suspended_until
                    .filter(|t| *t > now)
                    .map(|t| t.duration_since(now).as_secs())
                    .unwrap_or(0);
                AbuseSnapshot {
                    pubkey: pubkey.clone(),
                    violations: r.violations,
                    suspended,
                    suspend_remaining_secs: remaining,
                }
            })
            .collect()
    }
}
