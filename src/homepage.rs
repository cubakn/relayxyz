use bech32::{Bech32, Hrp};

use crate::config::Config;

const NIP_BASE: &str = "https://github.com/nostr-protocol/nips/blob/master/";

pub fn homepage_html(config: &Config) -> String {
    let name = html_escape(&config.name);
    let description = html_escape(&config.description);
    let version = env!("CARGO_PKG_VERSION");

    let auth_text = if config.require_auth { "yes" } else { "no" };

    let mut contact_html = String::new();
    if !config.contact.is_empty() {
        let contact = &config.contact;
        let display = if let Some(npub) = resolve_nip05(contact) {
            format!(
                r#"<a href="https://damus.io/{npub}">{}</a>"#,
                html_escape(contact),
            )
        } else {
            html_escape(contact)
        };
        contact_html.push_str(&format!(
            r#"<hr><div class="section"><div class="label">Contact</div><p class="meta">{display}</p></div>"#,
        ));
    }
    if !config.pubkey.is_empty() {
        let display = match hex_to_npub(&config.pubkey) {
            Some(npub) => format!(r#"<a href="https://damus.io/{npub}">{npub}</a>"#),
            None => html_escape(&config.pubkey),
        };
        contact_html.push_str(&format!(
            r#"<div class="section"><div class="label">Operator</div><p class="pubkey">{display}</p></div>"#,
        ));
    }

    let og_image_url = config
        .relay_url
        .as_deref()
        .map(|u| {
            u.replacen("wss://", "https://", 1)
                .replacen("ws://", "http://", 1)
        })
        .map(|base| format!("{base}/public/og"))
        .unwrap_or_else(|| "/public/og".into());

    let og_tags = format!(
        concat!(
            r#"<meta property="og:type" content="website">"#,
            r#"<meta property="og:title" content="{name}">"#,
            r#"<meta property="og:description" content="{desc}">"#,
            r#"<meta property="og:image" content="{img}">"#,
            r#"<meta property="og:image:width" content="1200">"#,
            r#"<meta property="og:image:height" content="630">"#,
            r#"<meta name="twitter:card" content="summary_large_image">"#,
            r#"<meta name="twitter:title" content="{name}">"#,
            r#"<meta name="twitter:description" content="{desc}">"#,
            r#"<meta name="twitter:image" content="{img}">"#,
        ),
        name = name,
        desc = description,
        img = og_image_url,
    );

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{name}</title>
{og_tags}
<style>
@font-face{{font-family:'Geist Mono';src:url('/public/font')format('truetype');font-display:swap}}
*{{margin:0;padding:0;box-sizing:border-box}}
body{{font-family:'Geist Mono',monospace;background:#0a0a0a;color:#fafafa;min-height:100vh;display:flex;align-items:center;justify-content:center}}
main{{max-width:480px;width:100%;padding:2rem}}
h1{{font-size:2rem;font-weight:700;margin-bottom:.25rem}}
.desc{{color:rgba(250,250,250,.5);margin-bottom:2rem}}
.section{{margin-bottom:1.5rem}}
.label{{font-size:.75rem;text-transform:uppercase;letter-spacing:.1em;color:rgba(250,250,250,.35);margin-bottom:.5rem}}
.nips{{display:flex;flex-wrap:wrap;gap:.375rem}}
a.nip{{font-size:.75rem;padding:.2rem .5rem;border:1px solid rgba(250,250,250,.12);text-decoration:none;color:#fafafa}}
a.nip:hover{{border-color:#f7931a}}
a.nip b{{color:#f7931a;font-weight:400}}
.limits{{font-size:.8rem;color:rgba(250,250,250,.5);line-height:1.8}}
.limits b{{color:rgba(250,250,250,.8);font-weight:400}}
hr{{border:none;border-top:1px solid rgba(250,250,250,.08);margin:1.5rem 0}}
.meta{{font-size:.8rem;color:rgba(250,250,250,.5)}}
.meta a{{color:rgba(250,250,250,.5);text-decoration:none}}
.meta a:hover{{color:#f7931a}}
.pubkey{{font-size:.75rem;word-break:break-all;color:rgba(250,250,250,.5)}}
.pubkey a{{color:rgba(250,250,250,.5);text-decoration:none}}
.pubkey a:hover{{color:#f7931a}}
.footer{{font-size:.7rem;color:rgba(250,250,250,.25)}}
.footer a{{color:#f7931a;text-decoration:none}}
</style>
</head>
<body>
<main>
<h1>{name}</h1>
<p class="desc">{description}</p>
<div class="section">
<div class="label">Supported NIPs</div>
<div class="nips">
<a class="nip" href="{nip}01.md"><b>01</b> protocol</a>
<a class="nip" href="{nip}02.md"><b>02</b> follows</a>
<a class="nip" href="{nip}04.md"><b>04</b> DMs</a>
<a class="nip" href="{nip}09.md"><b>09</b> delete</a>
<a class="nip" href="{nip}11.md"><b>11</b> info</a>
<a class="nip" href="{nip}22.md"><b>22</b> comment</a>
<a class="nip" href="{nip}25.md"><b>25</b> react</a>
<a class="nip" href="{nip}40.md"><b>40</b> expiry</a>
<a class="nip" href="{nip}42.md"><b>42</b> auth</a>
<a class="nip" href="{nip}70.md"><b>70</b> protect</a>
</div>
</div>
<hr>
<div class="section">
<div class="label">Limits</div>
<div class="limits">
max message <b>{max_msg}</b> bytes<br>
max subscriptions <b>{max_subs}</b><br>
default query limit <b>{query_limit}</b><br>
auth required <b>{auth_text}</b>
</div>
</div>
{contact_html}
<hr>
<div class="footer">powered by <a href="https://github.com/cubakn/relayxyz">relayxyz</a> v{version}</div>
</main>
</body>
</html>"#,
        name = name,
        og_tags = og_tags,
        description = description,
        nip = NIP_BASE,
        max_msg = config.max_message_length,
        max_subs = config.max_subscriptions,
        query_limit = config.default_query_limit,
        auth_text = auth_text,
        contact_html = contact_html,
        version = version,
    )
}

fn resolve_nip05(contact: &str) -> Option<String> {
    let (local, domain) = contact.split_once('@')?;
    if local.is_empty() || !domain.contains('.') {
        return None;
    }

    let url = format!("https://{}/.well-known/nostr.json?name={}", domain, local);

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build()
        .new_agent();

    let body: serde_json::Value = agent.get(&url).call().ok()?.body_mut().read_json().ok()?;

    let hex_pubkey = body.get("names")?.get(local)?.as_str()?;

    hex_to_npub(hex_pubkey)
}

fn hex_to_npub(hex_pubkey: &str) -> Option<String> {
    let bytes = hex::decode(hex_pubkey).ok()?;
    let hrp = Hrp::parse("npub").ok()?;
    bech32::encode::<Bech32>(hrp, &bytes).ok()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
