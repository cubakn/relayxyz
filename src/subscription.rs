use std::collections::HashMap;

use serde::Deserialize;
use serde::de::{self, Deserializer, MapAccess, Visitor};

use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{Event, is_expired};

const MAX_FILTER_LIST: usize = 50;

#[derive(Debug, Clone)]
pub struct Filter {
    pub ids: Option<Vec<String>>,
    pub authors: Option<Vec<String>>,
    pub kinds: Option<Vec<u32>>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<u32>,
    pub generic_tags: HashMap<String, Vec<String>>,
}

impl<'de> Deserialize<'de> for Filter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(FilterVisitor)
    }
}

struct FilterVisitor;

impl<'de> Visitor<'de> for FilterVisitor {
    type Value = Filter;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a filter object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut ids = None;
        let mut authors = None;
        let mut kinds = None;
        let mut since = None;
        let mut until = None;
        let mut limit = None;
        let mut generic_tags = HashMap::new();

        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "ids" => ids = Some(map.next_value()?),
                "authors" => authors = Some(map.next_value()?),
                "kinds" => kinds = Some(map.next_value()?),
                "since" => since = Some(map.next_value()?),
                "until" => until = Some(map.next_value()?),
                "limit" => limit = Some(map.next_value()?),
                k if k.starts_with('#')
                    && k.len() == 2
                    && k.as_bytes()[1].is_ascii_alphabetic() =>
                {
                    let tag_name = k[1..].to_string();
                    let values: Vec<String> = map.next_value()?;
                    generic_tags.insert(tag_name, values);
                }
                _ => {
                    let _ = map.next_value::<de::IgnoredAny>()?;
                }
            }
        }

        Ok(Filter {
            ids,
            authors,
            kinds,
            since,
            until,
            limit,
            generic_tags,
        })
    }
}

impl Filter {
    pub fn is_oversized(&self) -> bool {
        fn too_long<T>(opt: &Option<Vec<T>>) -> bool {
            opt.as_ref().is_some_and(|v| v.len() > MAX_FILTER_LIST)
        }
        too_long(&self.ids)
            || too_long(&self.authors)
            || too_long(&self.kinds)
            || self
                .generic_tags
                .values()
                .any(|v| v.len() > MAX_FILTER_LIST)
    }

    pub fn matches_event(&self, event: &Event) -> bool {
        if let Some(ref ids) = self.ids
            && !ids
                .iter()
                .any(|prefix| event.id.starts_with(prefix.as_str()))
        {
            return false;
        }
        if let Some(ref authors) = self.authors
            && !authors
                .iter()
                .any(|prefix| event.pubkey.starts_with(prefix.as_str()))
        {
            return false;
        }
        if let Some(ref kinds) = self.kinds
            && !kinds.contains(&event.kind)
        {
            return false;
        }
        if let Some(since) = self.since
            && event.created_at < since
        {
            return false;
        }
        if let Some(until) = self.until
            && event.created_at > until
        {
            return false;
        }
        for (tag_name, values) in &self.generic_tags {
            if !event_has_tag(event, tag_name, values) {
                return false;
            }
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if is_expired(&event.tags, now) {
            return false;
        }
        true
    }
}

fn event_has_tag(event: &Event, tag_name: &str, values: &[String]) -> bool {
    event
        .tags
        .iter()
        .any(|tag| tag.len() >= 2 && tag[0] == tag_name && values.contains(&tag[1]))
}

pub struct Subscription {
    pub filters: Vec<Filter>,
}

impl Subscription {
    pub fn matches(&self, event: &Event) -> bool {
        self.filters.iter().any(|f| f.matches_event(event))
    }
}
