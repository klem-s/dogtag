//! Saves every session locally and pushes it: to any HTTP endpoint and/or a Discord webhook.
//! Pushes that fail are queued in data/pending.jsonl and retried on the next start.
use crate::config::PushCfg;
use crate::session::Session;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::PathBuf;

pub fn payload(s: &Session) -> Value {
    json!({
        "app": "dogtag",
        "version": env!("CARGO_PKG_VERSION"),
        "game": "WARDOGS",
        "session": s.snapshot(),
    })
}

fn dir(cfg: &PushCfg) -> PathBuf {
    let d = PathBuf::from(&cfg.data_dir);
    let _ = std::fs::create_dir_all(d.join("sessions"));
    d
}

fn append(path: PathBuf, v: &Value) -> Result<()> {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{v}")?;
    Ok(())
}

async fn post_endpoint(client: &reqwest::Client, cfg: &PushCfg, body: &Value) -> Result<()> {
    let mut req = client.post(&cfg.endpoint).json(body);
    if !cfg.token.is_empty() {
        req = req.bearer_auth(&cfg.token);
    }
    let res = req.send().await?;
    if !res.status().is_success() {
        bail!("{} answered {}", cfg.endpoint, res.status());
    }
    Ok(())
}

fn money(v: i64) -> String {
    let s = v.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{}${out}", if v < 0 { "-" } else { "" })
}

async fn post_discord(client: &reqwest::Client, url: &str, s: &Session) -> Result<()> {
    let delta = s.balance_delta();
    let body = json!({
        "username": "dogtag",
        "embeds": [{
            "title": format!("{} - session WARDOGS", s.player),
            "description": format!("{} min de jeu", s.minutes().round()),
            "color": if delta >= 0 { 0x3fa34d } else { 0xc0392b },
            "fields": if s.mode == "solde" {
                json!([
                    {"name": "Solde", "value": s.balance_now.map(money).unwrap_or("?".into()), "inline": true},
                ])
            } else if s.mode == "money" {
                json!([
                    {"name": "Balance session", "value": format!("{}{}", if delta >= 0 {"+"} else {""}, money(delta)), "inline": true},
                    {"name": "Solde début", "value": s.balance_start.map(money).unwrap_or("?".into()), "inline": true},
                    {"name": "Solde fin", "value": s.balance_now.map(money).unwrap_or("?".into()), "inline": true},
                ])
            } else {
                json!([
                {"name": "Kills", "value": s.kills.to_string(), "inline": true},
                {"name": "Downs", "value": s.downs.to_string(), "inline": true},
                {"name": "K/D", "value": format!("{:.2}", s.kd()), "inline": true},
                {"name": "Assists", "value": s.assists.to_string(), "inline": true},
                {"name": "Revives", "value": s.revives.to_string(), "inline": true},
                {"name": "Véhicules", "value": s.vehicles.to_string(), "inline": true},
                {"name": "Balance", "value": format!("{}{}", if delta >= 0 {"+"} else {""}, money(delta)), "inline": true},
                {"name": "Gagné / dépensé", "value": format!("{} / {}", money(s.money_earned), money(s.money_spent)), "inline": true},
                ])
            },
            "timestamp": s.started_at.to_rfc3339(),
        }]
    });
    let res = client.post(url).json(&body).send().await?;
    if !res.status().is_success() {
        bail!("Discord answered {}", res.status());
    }
    Ok(())
}

/// Called once at the end of a session.
pub async fn finish(cfg: &PushCfg, s: &Session) -> Result<()> {
    let d = dir(cfg);
    let body = payload(s);
    std::fs::write(d.join("sessions").join(format!("{}.json", s.id)), serde_json::to_string_pretty(&body)?)?;
    append(d.join("history.jsonl"), &body)?;
    eprintln!("[push] saved {}", d.join("sessions").join(format!("{}.json", s.id)).display());

    if s.minutes() < cfg.min_minutes as f64 {
        eprintln!("[push] session under {} min: kept locally, not pushed", cfg.min_minutes);
        return Ok(());
    }
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build()?;
    if !cfg.endpoint.is_empty() {
        match post_endpoint(&client, cfg, &body).await {
            Ok(()) => eprintln!("[push] sent to {}", cfg.endpoint),
            Err(e) => {
                eprintln!("[push] {e} - queued, will retry next start");
                append(d.join("pending.jsonl"), &body)?;
            }
        }
    }
    if !cfg.discord_webhook.is_empty() {
        match post_discord(&client, &cfg.discord_webhook, s).await {
            Ok(()) => eprintln!("[push] posted to Discord"),
            Err(e) => eprintln!("[push] Discord: {e}"),
        }
    }
    Ok(())
}

/// Resends queued sessions; keeps the ones that still fail.
pub async fn retry_pending(cfg: &PushCfg) {
    if cfg.endpoint.is_empty() {
        return;
    }
    let path = dir(cfg).join("pending.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build() else { return };
    let mut left = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if post_endpoint(&client, cfg, &v).await.is_err() {
            left.push(line.to_string());
        }
    }
    let sent = text.lines().filter(|l| !l.trim().is_empty()).count() - left.len();
    if sent > 0 {
        eprintln!("[push] resent {sent} queued session(s)");
    }
    let _ = std::fs::write(&path, left.join("\n") + if left.is_empty() { "" } else { "\n" });
}

#[cfg(test)]
mod tests {
    #[test]
    fn money_format() {
        assert_eq!(super::money(48250), "$48,250");
        assert_eq!(super::money(-3009), "-$3,009");
        assert_eq!(super::money(150), "$150");
    }
}
