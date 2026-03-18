use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::db::Db;
use crate::event::{Event, is_replaceable};

pub enum WriteResult {
    Stored,
    Duplicate,
    Error(String),
}

struct WriteRequest {
    event: Arc<Event>,
    raw_json: Vec<u8>,
    response_tx: oneshot::Sender<WriteResult>,
}

struct BatchWriteRequest {
    events: Vec<(Arc<Event>, Vec<u8>)>,
    response_tx: oneshot::Sender<Vec<WriteResult>>,
}

enum WriterMessage {
    Single(WriteRequest),
    Batch(BatchWriteRequest),
}

pub struct BatchWriter {
    tx: mpsc::Sender<WriterMessage>,
}

impl BatchWriter {
    pub fn new(db: Arc<Db>, batch_size: usize, batch_timeout: Duration) -> Self {
        let (tx, rx) = mpsc::channel(4096);
        tokio::spawn(writer_loop(db, rx, batch_size, batch_timeout));
        Self { tx }
    }

    pub async fn submit(&self, event: Arc<Event>, raw_json: Vec<u8>) -> WriteResult {
        let (response_tx, response_rx) = oneshot::channel();
        let request = WriteRequest {
            event,
            raw_json,
            response_tx,
        };
        if self.tx.send(WriterMessage::Single(request)).await.is_err() {
            return WriteResult::Error("writer shutdown".into());
        }
        match response_rx.await {
            Ok(result) => result,
            Err(_) => WriteResult::Error("writer dropped".into()),
        }
    }

    pub async fn submit_batch(&self, events: Vec<(Arc<Event>, Vec<u8>)>) -> Vec<WriteResult> {
        if events.is_empty() {
            return Vec::new();
        }
        let (response_tx, response_rx) = oneshot::channel();
        let request = BatchWriteRequest {
            events,
            response_tx,
        };
        let count = request.events.len();
        if self.tx.send(WriterMessage::Batch(request)).await.is_err() {
            return (0..count)
                .map(|_| WriteResult::Error("writer shutdown".into()))
                .collect();
        }
        match response_rx.await {
            Ok(results) => results,
            Err(_) => (0..count)
                .map(|_| WriteResult::Error("writer dropped".into()))
                .collect(),
        }
    }
}

// Tracks where each entry came from so we can route results back
enum EntryOrigin {
    Single(oneshot::Sender<WriteResult>),
    BatchItem { batch_idx: usize },
}

struct InternalEntry {
    event: Arc<Event>,
    raw_json: Vec<u8>,
    origin: EntryOrigin,
}

// Tracks batch response channels and how many items each batch contributed
struct BatchResponseSlot {
    tx: oneshot::Sender<Vec<WriteResult>>,
    count: usize,
}

async fn writer_loop(
    db: Arc<Db>,
    mut rx: mpsc::Receiver<WriterMessage>,
    batch_size: usize,
    batch_timeout: Duration,
) {
    loop {
        let first = match rx.recv().await {
            Some(msg) => msg,
            None => break,
        };

        let mut entries: Vec<InternalEntry> = Vec::new();
        let mut batch_slots: Vec<BatchResponseSlot> = Vec::new();

        // Flatten the first message
        flatten_message(first, &mut entries, &mut batch_slots);

        // Collect more messages up to batch_size or timeout
        let deadline = tokio::time::Instant::now() + batch_timeout;
        while entries.len() < batch_size {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(msg)) => {
                    flatten_message(msg, &mut entries, &mut batch_slots);
                    if entries.len() >= batch_size {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Pre-process: deduplicate by event ID within batch
        let mut seen_ids: HashMap<String, usize> = HashMap::new();
        let mut skip = vec![false; entries.len()];

        for i in 0..entries.len() {
            match seen_ids.entry(entries[i].event.id.clone()) {
                std::collections::hash_map::Entry::Occupied(_) => {
                    skip[i] = true;
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(i);
                }
            }
        }

        // Pre-process: resolve intra-batch replaceable conflicts
        let mut best_replaceable: HashMap<(String, u32), usize> = HashMap::new();
        for i in 0..entries.len() {
            if skip[i] {
                continue;
            }
            let ev = &entries[i].event;
            if is_replaceable(ev.kind) {
                let key = (ev.pubkey.clone(), ev.kind);
                match best_replaceable.entry(key) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        let prev_i = *e.get();
                        let prev = &entries[prev_i].event;
                        if ev.created_at > prev.created_at
                            || (ev.created_at == prev.created_at && ev.id < prev.id)
                        {
                            skip[prev_i] = true;
                            e.insert(i);
                        } else {
                            skip[i] = true;
                        }
                    }
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(i);
                    }
                }
            }
        }

        // Collect non-skipped events for batch write (take raw_json to avoid cloning)
        let write_batch: Vec<(Arc<Event>, Vec<u8>)> = entries
            .iter_mut()
            .enumerate()
            .filter(|(i, _)| !skip[*i])
            .map(|(_, entry)| {
                (
                    Arc::clone(&entry.event),
                    std::mem::take(&mut entry.raw_json),
                )
            })
            .collect();

        // Build per-entry results
        let mut results: Vec<Option<WriteResult>> = Vec::with_capacity(entries.len());

        match db.write_event_batch(&write_batch) {
            Ok(outcomes) => {
                let mut outcome_iter = outcomes.into_iter();
                for skipped in skip.iter().take(entries.len()) {
                    let result = if *skipped {
                        WriteResult::Duplicate
                    } else {
                        let stored = outcome_iter.next().unwrap_or(false);
                        if stored {
                            WriteResult::Stored
                        } else {
                            WriteResult::Duplicate
                        }
                    };
                    results.push(Some(result));
                }
            }
            Err(e) => {
                let msg = e.to_string();
                for _ in 0..entries.len() {
                    results.push(Some(WriteResult::Error(msg.clone())));
                }
            }
        }

        // Distribute results back through response channels
        let mut batch_results: Vec<Vec<WriteResult>> = batch_slots
            .iter()
            .map(|slot| Vec::with_capacity(slot.count))
            .collect();

        for (i, entry) in entries.into_iter().enumerate() {
            let result = results[i].take().unwrap();
            match entry.origin {
                EntryOrigin::Single(tx) => {
                    let _ = tx.send(result);
                }
                EntryOrigin::BatchItem { batch_idx, .. } => {
                    batch_results[batch_idx].push(result);
                }
            }
        }

        for (slot, results) in batch_slots.into_iter().zip(batch_results) {
            let _ = slot.tx.send(results);
        }
    }
}

fn flatten_message(
    msg: WriterMessage,
    entries: &mut Vec<InternalEntry>,
    batch_slots: &mut Vec<BatchResponseSlot>,
) {
    match msg {
        WriterMessage::Single(req) => {
            entries.push(InternalEntry {
                event: req.event,
                raw_json: req.raw_json,
                origin: EntryOrigin::Single(req.response_tx),
            });
        }
        WriterMessage::Batch(req) => {
            let batch_idx = batch_slots.len();
            let count = req.events.len();
            batch_slots.push(BatchResponseSlot {
                tx: req.response_tx,
                count,
            });
            for (event, raw_json) in req.events {
                entries.push(InternalEntry {
                    event,
                    raw_json,
                    origin: EntryOrigin::BatchItem { batch_idx },
                });
            }
        }
    }
}
