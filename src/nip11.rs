use crate::config::Config;

pub fn nip11_json(config: &Config) -> String {
    let mut doc = serde_json::json!({
        "name": config.name,
        "description": config.description,
        "icon": config.icon_url,
        "pubkey": config.pubkey,
        "contact": config.contact,
        "supported_nips": [1, 2, 4, 9, 11, 18, 22, 25, 40, 42, 51, 65, 70],
        "software": "git+https://github.com/cubakn/relayxyz.git",
        "version": env!("CARGO_PKG_VERSION"),
        "limitation": {
            "max_message_length": config.max_message_length,
            "max_subscriptions": config.max_subscriptions,
            "max_limit": config.default_query_limit,
            "auth_required": config.require_auth,
            "payment_required": config.require_auth,
            "restricted_writes": config.require_auth,
            "created_at_upper_limit": 600
        }
    });
    if let Some(url) = &config.payments_url {
        doc["payments_url"] = serde_json::json!(url);
    }
    if let Some(msats) = config.admission_fee_msats {
        doc["fees"] = serde_json::json!({
            "admission": [{ "amount": msats, "unit": "msats" }]
        });
    }
    doc.to_string()
}
