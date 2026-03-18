use std::collections::HashSet;
use std::sync::Arc;

use redb::{Database, ReadableTable, TableDefinition};

use crate::error::RelayError;
use crate::event::{Event, is_replaceable};
use crate::subscription::Filter;

const EVENTS: TableDefinition<&str, &[u8]> = TableDefinition::new("events");
const IDX_KIND: TableDefinition<(u32, u64, &str), ()> = TableDefinition::new("idx_kind");
const IDX_AUTHOR: TableDefinition<(&str, u32, u64, &str), ()> = TableDefinition::new("idx_author");
const IDX_TAG: TableDefinition<(&str, &str, u64, &str), ()> = TableDefinition::new("idx_tag");
const ALLOWED_PUBKEYS: TableDefinition<&str, ()> = TableDefinition::new("allowed_pubkeys");

const RANGE_END: &str = concat!(
    "\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f",
    "\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f",
    "\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f",
    "\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f\x7f",
);

pub struct Db {
    inner: Database,
}

impl Db {
    pub fn open(path: &str) -> Result<Self, RelayError> {
        let db = Database::create(path)?;
        let tx = db.begin_write()?;
        {
            let _ = tx.open_table(EVENTS)?;
            let _ = tx.open_table(IDX_KIND)?;
            let _ = tx.open_table(IDX_AUTHOR)?;
            let _ = tx.open_table(IDX_TAG)?;
            let _ = tx.open_table(ALLOWED_PUBKEYS)?;
        }
        tx.commit()?;

        // Migration: populate IDX_TAG from existing events if needed
        let needs_migration = {
            let read_tx = db.begin_read()?;
            let events_table = read_tx.open_table(EVENTS)?;
            let tag_table = read_tx.open_table(IDX_TAG)?;
            let has_events = events_table.iter()?.next().is_some();
            let has_tags = tag_table.iter()?.next().is_some();
            has_events && !has_tags
        };

        if needs_migration {
            let all_events: Vec<Vec<u8>> = {
                let read_tx = db.begin_read()?;
                let events_table = read_tx.open_table(EVENTS)?;
                events_table
                    .iter()?
                    .filter_map(|e| e.ok().map(|e| e.1.value().to_vec()))
                    .collect()
            };

            let tx = db.begin_write()?;
            {
                let mut idx_tag = tx.open_table(IDX_TAG)?;
                for data in &all_events {
                    if let Ok(event) = serde_json::from_slice::<Event>(data) {
                        insert_tag_entries(&mut idx_tag, &event)?;
                    }
                }
            }
            tx.commit()?;
            eprintln!("migrated tag index ({} events)", all_events.len());
        }

        Ok(Self { inner: db })
    }

    pub fn is_whitelisted(&self, pubkey: &str) -> Result<bool, RelayError> {
        let tx = self.inner.begin_read()?;
        let table = tx.open_table(ALLOWED_PUBKEYS)?;
        Ok(table.get(pubkey)?.is_some())
    }

    pub fn add_pubkey(&self, pubkey: &str) -> Result<(), RelayError> {
        let tx = self.inner.begin_write()?;
        {
            let mut table = tx.open_table(ALLOWED_PUBKEYS)?;
            table.insert(pubkey, ())?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_pubkey(&self, pubkey: &str) -> Result<(), RelayError> {
        let tx = self.inner.begin_write()?;
        {
            let mut table = tx.open_table(ALLOWED_PUBKEYS)?;
            table.remove(pubkey)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn write_event(&self, event: &Event, raw_json: &[u8]) -> Result<bool, RelayError> {
        let tx = self.inner.begin_write()?;
        {
            let mut events = tx.open_table(EVENTS)?;

            if events.get(event.id.as_str())?.is_some() {
                return Ok(false);
            }

            let mut idx_kind = tx.open_table(IDX_KIND)?;
            let mut idx_author = tx.open_table(IDX_AUTHOR)?;
            let mut idx_tag = tx.open_table(IDX_TAG)?;

            if is_replaceable(event.kind) {
                let start = (event.pubkey.as_str(), event.kind, 0u64, "");
                let end = (event.pubkey.as_str(), event.kind, u64::MAX, RANGE_END);
                let range = idx_author.range(start..=end)?;
                for entry in range {
                    let entry = entry?;
                    let (_, _, ts, eid) = entry.0.value();
                    if ts > event.created_at {
                        return Ok(false);
                    }
                    if ts == event.created_at && eid <= event.id.as_str() {
                        return Ok(false);
                    }
                }

                delete_replaceable(
                    &mut events,
                    &mut idx_kind,
                    &mut idx_author,
                    &mut idx_tag,
                    &event.pubkey,
                    event.kind,
                )?;
            }

            events.insert(event.id.as_str(), raw_json)?;
            idx_kind.insert((event.kind, event.created_at, event.id.as_str()), ())?;
            idx_author.insert(
                (
                    event.pubkey.as_str(),
                    event.kind,
                    event.created_at,
                    event.id.as_str(),
                ),
                (),
            )?;
            insert_tag_entries(&mut idx_tag, event)?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn write_event_batch(
        &self,
        batch: &[(Arc<Event>, Vec<u8>)],
    ) -> Result<Vec<bool>, RelayError> {
        let tx = self.inner.begin_write()?;
        let mut results = Vec::with_capacity(batch.len());
        {
            let mut events = tx.open_table(EVENTS)?;
            let mut idx_kind = tx.open_table(IDX_KIND)?;
            let mut idx_author = tx.open_table(IDX_AUTHOR)?;
            let mut idx_tag = tx.open_table(IDX_TAG)?;

            for (event, raw_json) in batch {
                if events.get(event.id.as_str())?.is_some() {
                    results.push(false);
                    continue;
                }

                if is_replaceable(event.kind) {
                    let start = (event.pubkey.as_str(), event.kind, 0u64, "");
                    let end = (event.pubkey.as_str(), event.kind, u64::MAX, RANGE_END);
                    let range = idx_author.range(start..=end)?;
                    let mut dominated = false;
                    for entry in range {
                        let entry = entry?;
                        let (_, _, ts, eid) = entry.0.value();
                        if ts > event.created_at
                            || (ts == event.created_at && eid <= event.id.as_str())
                        {
                            dominated = true;
                            break;
                        }
                    }
                    if dominated {
                        results.push(false);
                        continue;
                    }
                    delete_replaceable(
                        &mut events,
                        &mut idx_kind,
                        &mut idx_author,
                        &mut idx_tag,
                        &event.pubkey,
                        event.kind,
                    )?;
                }

                events.insert(event.id.as_str(), raw_json.as_slice())?;
                idx_kind.insert((event.kind, event.created_at, event.id.as_str()), ())?;
                idx_author.insert(
                    (
                        event.pubkey.as_str(),
                        event.kind,
                        event.created_at,
                        event.id.as_str(),
                    ),
                    (),
                )?;
                insert_tag_entries(&mut idx_tag, event)?;
                results.push(true);
            }
        }
        tx.commit()?;
        Ok(results)
    }

    pub fn delete_events_by_ids(
        &self,
        pubkey: &str,
        target_ids: &[String],
    ) -> Result<Vec<String>, RelayError> {
        let tx = self.inner.begin_write()?;
        let mut deleted = Vec::new();
        {
            let mut events = tx.open_table(EVENTS)?;
            let mut idx_kind = tx.open_table(IDX_KIND)?;
            let mut idx_author = tx.open_table(IDX_AUTHOR)?;
            let mut idx_tag = tx.open_table(IDX_TAG)?;

            for target_id in target_ids {
                let raw = match events.get(target_id.as_str())? {
                    Some(v) => v.value().to_vec(),
                    None => continue,
                };
                let event: Event = match serde_json::from_slice(&raw) {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if event.pubkey != pubkey {
                    continue;
                }
                if event.kind == 5 {
                    continue;
                }
                events.remove(target_id.as_str())?;
                idx_kind.remove((event.kind, event.created_at, event.id.as_str()))?;
                idx_author.remove((
                    event.pubkey.as_str(),
                    event.kind,
                    event.created_at,
                    event.id.as_str(),
                ))?;
                remove_tag_entries(&mut idx_tag, &event)?;
                deleted.push(target_id.clone());
            }
        }
        tx.commit()?;
        Ok(deleted)
    }

    pub fn list_allowed_pubkeys(&self) -> Result<Vec<String>, RelayError> {
        let tx = self.inner.begin_read()?;
        let table = tx.open_table(ALLOWED_PUBKEYS)?;
        let mut pubkeys = Vec::new();
        for entry in table.iter()? {
            let entry = entry?;
            pubkeys.push(entry.0.value().to_string());
        }
        Ok(pubkeys)
    }

    pub fn list_unique_authors(&self) -> Result<Vec<String>, RelayError> {
        let tx = self.inner.begin_read()?;
        let table = tx.open_table(IDX_AUTHOR)?;
        let mut authors = Vec::new();
        let mut last: Option<String> = None;
        for entry in table.iter()? {
            let entry = entry?;
            let (pubkey, _, _, _) = entry.0.value();
            match &last {
                Some(prev) if prev == pubkey => {}
                _ => {
                    let owned = pubkey.to_string();
                    last = Some(owned.clone());
                    authors.push(owned);
                }
            }
        }
        Ok(authors)
    }

    pub fn count_events_by_kind(&self) -> Result<Vec<(u32, u64)>, RelayError> {
        let tx = self.inner.begin_read()?;
        let table = tx.open_table(IDX_KIND)?;
        let mut counts: Vec<(u32, u64)> = Vec::new();
        for entry in table.iter()? {
            let entry = entry?;
            let (kind, _, _) = entry.0.value();
            match counts.last_mut() {
                Some((k, c)) if *k == kind => *c += 1,
                _ => counts.push((kind, 1)),
            }
        }
        Ok(counts)
    }

    pub fn get_events_by_kind(&self, kind: u32) -> Result<Vec<Vec<u8>>, RelayError> {
        let tx = self.inner.begin_read()?;
        let idx = tx.open_table(IDX_KIND)?;
        let events_table = tx.open_table(EVENTS)?;
        let start = (kind, 0u64, "");
        let end = (kind, u64::MAX, RANGE_END);
        let mut results = Vec::new();
        for entry in idx.range(start..=end)? {
            let entry = entry?;
            let (_, _, eid) = entry.0.value();
            if let Some(val) = events_table.get(eid)? {
                results.push(val.value().to_vec());
            }
        }
        Ok(results)
    }

    pub fn query(
        &self,
        filter: &Filter,
        allowed_kinds: &[u32],
        default_limit: u32,
    ) -> Result<Vec<Vec<u8>>, RelayError> {
        let tx = self.inner.begin_read()?;
        let events_table = tx.open_table(EVENTS)?;
        let mut results = Vec::new();
        let mut seen = HashSet::new();
        let limit = filter.limit.unwrap_or(default_limit) as usize;

        if let Some(ref ids) = filter.ids {
            for id in ids {
                if results.len() >= limit {
                    break;
                }
                if id.len() == 64 {
                    if let Some(val) = events_table.get(id.as_str())? {
                        let data = val.value().to_vec();
                        match serde_json::from_slice::<Event>(&data) {
                            Ok(event) => {
                                if filter.matches_event(&event) && seen.insert(event.id.clone()) {
                                    results.push(data);
                                }
                            }
                            Err(e) => {
                                eprintln!("corrupt event in db (id={id}): {e}");
                            }
                        }
                    }
                } else {
                    let upper = format!("{id}\x7f");
                    let range = events_table.range(id.as_str()..upper.as_str())?;
                    for entry in range {
                        if results.len() >= limit {
                            break;
                        }
                        let entry = entry?;
                        let data = entry.1.value().to_vec();
                        match serde_json::from_slice::<Event>(&data) {
                            Ok(event) => {
                                if filter.matches_event(&event) && seen.insert(event.id.clone()) {
                                    results.push(data);
                                }
                            }
                            Err(e) => {
                                let eid = entry.0.value();
                                eprintln!("corrupt event in db (id={eid}): {e}");
                            }
                        }
                    }
                }
            }
            return Ok(results);
        }

        let since = filter.since.unwrap_or(0);
        let until = filter.until.unwrap_or(u64::MAX);

        // Tag-first query strategy: when filtering by tags without specific authors
        if !filter.generic_tags.is_empty() && filter.authors.is_none() {
            let idx_tag = tx.open_table(IDX_TAG)?;
            if let Some((tag_name, tag_values)) = filter.generic_tags.iter().next() {
                for tag_value in tag_values {
                    if results.len() >= limit {
                        break;
                    }
                    let start = (tag_name.as_str(), tag_value.as_str(), since, "");
                    let end = (tag_name.as_str(), tag_value.as_str(), until, RANGE_END);
                    let range = idx_tag.range(start..=end)?;
                    for entry in range.rev() {
                        if results.len() >= limit {
                            break;
                        }
                        let entry = entry?;
                        let (_, _, _, eid) = entry.0.value();
                        if seen.insert(eid.to_string())
                            && let Some(val) = events_table.get(eid)?
                        {
                            let data = val.value().to_vec();
                            match serde_json::from_slice::<Event>(&data) {
                                Ok(event) => {
                                    if filter.matches_event(&event) {
                                        results.push(data);
                                    }
                                }
                                Err(e) => {
                                    eprintln!("corrupt event in db (id={eid}): {e}");
                                }
                            }
                        }
                    }
                }
                return Ok(results);
            }
        }

        if let Some(ref authors) = filter.authors
            && authors.iter().all(|a| a.len() == 64)
        {
            let idx_author = tx.open_table(IDX_AUTHOR)?;
            let kinds = filter.kinds.as_deref().unwrap_or(allowed_kinds);
            for author in authors {
                for kind in kinds {
                    if results.len() >= limit {
                        break;
                    }
                    let start = (author.as_str(), *kind, since, "");
                    let end = (author.as_str(), *kind, until, RANGE_END);
                    let range = idx_author.range(start..=end)?;
                    for entry in range.rev() {
                        if results.len() >= limit {
                            break;
                        }
                        let entry = entry?;
                        let (_, _, _, eid) = entry.0.value();
                        if seen.insert(eid.to_string())
                            && let Some(val) = events_table.get(eid)?
                        {
                            let data = val.value().to_vec();
                            if !filter.generic_tags.is_empty() {
                                match serde_json::from_slice::<Event>(&data) {
                                    Ok(event) => {
                                        if filter.matches_event(&event) {
                                            results.push(data);
                                        }
                                    }
                                    Err(e) => {
                                        eprintln!("corrupt event in db (id={eid}): {e}");
                                    }
                                }
                            } else {
                                results.push(data);
                            }
                        }
                    }
                }
            }
            return Ok(results);
        }

        let needs_filter = !filter.generic_tags.is_empty() || filter.authors.is_some();

        let kinds = filter.kinds.as_deref().unwrap_or(allowed_kinds);
        let idx_kind_table = tx.open_table(IDX_KIND)?;
        for kind in kinds {
            if results.len() >= limit {
                break;
            }
            let start = (*kind, since, "");
            let end = (*kind, until, RANGE_END);
            let range = idx_kind_table.range(start..=end)?;
            for entry in range.rev() {
                if results.len() >= limit {
                    break;
                }
                let entry = entry?;
                let (_, _, eid) = entry.0.value();
                if seen.insert(eid.to_string())
                    && let Some(val) = events_table.get(eid)?
                {
                    let data = val.value().to_vec();
                    if needs_filter {
                        match serde_json::from_slice::<Event>(&data) {
                            Ok(event) => {
                                if filter.matches_event(&event) {
                                    results.push(data);
                                }
                            }
                            Err(e) => {
                                eprintln!("corrupt event in db (id={eid}): {e}");
                            }
                        }
                    } else {
                        results.push(data);
                    }
                }
            }
        }
        Ok(results)
    }
}

fn is_indexable_tag(tag: &[String]) -> bool {
    tag.len() >= 2 && tag[0].len() == 1 && tag[0].as_bytes()[0].is_ascii_alphabetic()
}

fn insert_tag_entries(
    idx_tag: &mut redb::Table<(&str, &str, u64, &str), ()>,
    event: &Event,
) -> Result<(), RelayError> {
    for tag in &event.tags {
        if is_indexable_tag(tag) {
            idx_tag.insert(
                (
                    tag[0].as_str(),
                    tag[1].as_str(),
                    event.created_at,
                    event.id.as_str(),
                ),
                (),
            )?;
        }
    }
    Ok(())
}

fn remove_tag_entries(
    idx_tag: &mut redb::Table<(&str, &str, u64, &str), ()>,
    event: &Event,
) -> Result<(), RelayError> {
    for tag in &event.tags {
        if is_indexable_tag(tag) {
            idx_tag.remove((
                tag[0].as_str(),
                tag[1].as_str(),
                event.created_at,
                event.id.as_str(),
            ))?;
        }
    }
    Ok(())
}

fn delete_replaceable(
    events: &mut redb::Table<&str, &[u8]>,
    idx_kind: &mut redb::Table<(u32, u64, &str), ()>,
    idx_author: &mut redb::Table<(&str, u32, u64, &str), ()>,
    idx_tag: &mut redb::Table<(&str, &str, u64, &str), ()>,
    pubkey: &str,
    kind: u32,
) -> Result<(), RelayError> {
    let start = (pubkey, kind, 0u64, "");
    let end = (pubkey, kind, u64::MAX, RANGE_END);
    let mut to_delete = Vec::new();

    let range = idx_author.range(start..=end)?;
    for entry in range {
        let entry = entry?;
        let (pk, k, ts, eid) = entry.0.value();
        to_delete.push((pk.to_string(), k, ts, eid.to_string()));
    }

    for (pk, k, ts, eid) in &to_delete {
        if let Some(val) = events.get(eid.as_str())? {
            let data = val.value().to_vec();
            if let Ok(old_event) = serde_json::from_slice::<Event>(&data) {
                remove_tag_entries(idx_tag, &old_event)?;
            }
        }
        events.remove(eid.as_str())?;
        idx_kind.remove((*k, *ts, eid.as_str()))?;
        idx_author.remove((pk.as_str(), *k, *ts, eid.as_str()))?;
    }
    Ok(())
}
