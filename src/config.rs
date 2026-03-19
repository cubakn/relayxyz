use std::net::SocketAddr;

pub struct Config {
    pub bind: SocketAddr,
    pub db_path: String,
    pub admin_key: Option<String>,
    pub require_auth: bool,
    pub relay_url: Option<String>,
    pub name: String,
    pub description: String,
    pub pubkey: String,
    pub contact: String,
    pub allowed_kinds: Vec<u32>,
    pub max_content_graphemes: usize,
    pub max_subscriptions: usize,
    pub max_message_length: usize,
    pub default_query_limit: u32,
    pub icon_url: String,
    pub min_event_interval_ms: u64,
    pub abuse_strike_limit: u32,
    pub abuse_strike_window_secs: u64,
    pub abuse_suspend_secs: u64,
    pub payments_url: Option<String>,
    pub admission_fee_msats: Option<u64>,
}

impl Config {
    pub fn from_env() -> Self {
        let bind = std::env::var("RELAY_BIND")
            .unwrap_or_else(|_| "0.0.0.0:7777".into())
            .parse()
            .expect("invalid RELAY_BIND address");
        let db_path = std::env::var("RELAY_DB_PATH").unwrap_or_else(|_| "relay.redb".into());
        let admin_key = std::env::var("RELAY_ADMIN_KEY").ok();
        let require_auth = std::env::var("RELAY_REQUIRE_AUTH")
            .unwrap_or_else(|_| "false".into())
            .parse()
            .expect("RELAY_REQUIRE_AUTH must be true or false");
        let relay_url = std::env::var("RELAY_URL").ok();
        if require_auth && admin_key.is_none() {
            panic!("RELAY_ADMIN_KEY is required when RELAY_REQUIRE_AUTH=true");
        }
        if require_auth && relay_url.is_none() {
            panic!("RELAY_URL is required when RELAY_REQUIRE_AUTH=true");
        }
        let name = std::env::var("RELAY_NAME").unwrap_or_else(|_| "relayxyz".into());
        let description =
            std::env::var("RELAY_DESCRIPTION").unwrap_or_else(|_| "A private Nostr relay".into());

        let pubkey = std::env::var("RELAY_PUBKEY").unwrap_or_default();
        if !pubkey.is_empty()
            && (pubkey.len() != 64 || !pubkey.chars().all(|c| c.is_ascii_hexdigit()))
        {
            panic!("RELAY_PUBKEY must be a 64-character hex string");
        }
        let contact = std::env::var("RELAY_CONTACT").unwrap_or_default();

        let allowed_kinds: Vec<u32> = std::env::var("RELAY_ALLOWED_KINDS")
            .unwrap_or_else(|_| "0,1,2,3,4,5,6,7,16,1111,9735,10000,10001,10002".into())
            .split(',')
            .map(|s| {
                s.trim()
                    .parse()
                    .expect("RELAY_ALLOWED_KINDS must be comma-separated u32 values")
            })
            .collect();
        assert!(
            !allowed_kinds.is_empty(),
            "RELAY_ALLOWED_KINDS must not be empty"
        );

        let max_content_graphemes: usize = std::env::var("RELAY_MAX_CONTENT_GRAPHEMES")
            .unwrap_or_else(|_| "180".into())
            .parse()
            .expect("invalid RELAY_MAX_CONTENT_GRAPHEMES");
        let max_subscriptions: usize = std::env::var("RELAY_MAX_SUBSCRIPTIONS")
            .unwrap_or_else(|_| "20".into())
            .parse()
            .expect("invalid RELAY_MAX_SUBSCRIPTIONS");
        let max_message_length: usize = std::env::var("RELAY_MAX_MESSAGE_LENGTH")
            .unwrap_or_else(|_| "65536".into())
            .parse()
            .expect("invalid RELAY_MAX_MESSAGE_LENGTH");
        let default_query_limit: u32 = std::env::var("RELAY_DEFAULT_QUERY_LIMIT")
            .unwrap_or_else(|_| "500".into())
            .parse()
            .expect("invalid RELAY_DEFAULT_QUERY_LIMIT");

        let icon_url = std::env::var("RELAY_ICON_URL")
            .unwrap_or_else(|_| "http://localhost:7777/public/logo".into());

        let min_event_interval_ms: u64 = std::env::var("RELAY_MIN_EVENT_INTERVAL_MS")
            .unwrap_or_else(|_| "1000".into())
            .parse()
            .expect("invalid RELAY_MIN_EVENT_INTERVAL_MS");

        let abuse_strike_limit: u32 = std::env::var("RELAY_ABUSE_STRIKE_LIMIT")
            .unwrap_or_else(|_| "10".into())
            .parse()
            .expect("invalid RELAY_ABUSE_STRIKE_LIMIT");
        let abuse_strike_window_secs: u64 = std::env::var("RELAY_ABUSE_STRIKE_WINDOW_SECS")
            .unwrap_or_else(|_| "60".into())
            .parse()
            .expect("invalid RELAY_ABUSE_STRIKE_WINDOW_SECS");
        let abuse_suspend_secs: u64 = std::env::var("RELAY_ABUSE_SUSPEND_SECS")
            .unwrap_or_else(|_| "300".into())
            .parse()
            .expect("invalid RELAY_ABUSE_SUSPEND_SECS");

        let payments_url = std::env::var("RELAY_PAYMENTS_URL").ok();
        let admission_fee_msats: Option<u64> = std::env::var("RELAY_ADMISSION_FEE_MSATS")
            .ok()
            .map(|s| s.parse().expect("invalid RELAY_ADMISSION_FEE_MSATS"));

        Self {
            bind,
            db_path,
            admin_key,
            require_auth,
            relay_url,
            name,
            description,
            pubkey,
            contact,
            allowed_kinds,
            max_content_graphemes,
            max_subscriptions,
            max_message_length,
            default_query_limit,
            icon_url,
            min_event_interval_ms,
            abuse_strike_limit,
            abuse_strike_window_secs,
            abuse_suspend_secs,
            payments_url,
            admission_fee_msats,
        }
    }
}
