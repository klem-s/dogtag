//! Time series for Grafana. Every change of the session (and a heartbeat every 30 s so graphs stay
//! continuous) becomes one InfluxDB line-protocol point, sent in batches every 5 s. The same points go
//! to data/balance.csv. If the database is unreachable, points wait in memory and are sent later.
use crate::config::MetricsCfg;
use serde_json::Value;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Notify};

const FIELDS: &[&str] = &[
    "balance_now", "balance_delta", "kills", "downs", "assists", "headshots", "revives", "vehicles",
    "objectives", "xp", "money_earned", "money_spent", "match_change", "rank",
];

fn esc_tag(v: &str) -> String {
    v.replace('\\', "\\\\").replace(',', "\\,").replace('=', "\\=").replace(' ', "\\ ")
}

/// `wardogs,player=Name balance=-8000i,balance_delta=0i,kills=3i,... <ns>`
pub fn line(snap: &Value, ts_ns: i64) -> Option<String> {
    let player = snap["player"].as_str().unwrap_or("?");
    let mut fields = Vec::new();
    for f in FIELDS {
        if let Some(v) = snap[*f].as_i64() {
            let name = if *f == "balance_now" { "balance" } else { f };
            fields.push(format!("{name}={v}i"));
        }
    }
    if let Some(kd) = snap["kd"].as_f64() {
        fields.push(format!("kd={kd}"));
    }
    fields.push(format!("downed={}i", snap["downed"].as_bool().unwrap_or(false) as i64));
    Some(format!("wardogs,player={} {} {ts_ns}", esc_tag(player), fields.join(",")))
}

fn csv_row(snap: &Value, now: chrono::DateTime<chrono::Utc>) -> String {
    let n = |k: &str| snap[k].as_i64().map(|v| v.to_string()).unwrap_or_default();
    format!(
        "{},{},{},{},{},{},{}\n",
        now.to_rfc3339(),
        snap["player"].as_str().unwrap_or("?").replace(',', " "),
        n("balance_now"),
        n("balance_delta"),
        n("kills"),
        n("downs"),
        n("assists")
    )
}

/// The part of the snapshot that matters for graphs (the event log and timers change on every event).
fn key(snap: &Value) -> String {
    FIELDS.iter().map(|f| snap[*f].to_string()).collect::<Vec<_>>().join(",") + &snap["downed"].to_string()
}

async fn flush(client: &reqwest::Client, cfg: &MetricsCfg, buf: &mut Vec<String>) {
    if cfg.url.is_empty() || buf.is_empty() {
        buf.clear();
        return;
    }
    let mut req = client.post(&cfg.url).body(buf.join("\n"));
    if !cfg.token.is_empty() {
        req = req.header("Authorization", format!("Token {}", cfg.token));
    }
    match req.send().await {
        Ok(r) if r.status().is_success() => buf.clear(),
        Ok(r) => eprintln!("[metrics] {} answered {} - will retry", cfg.url, r.status()),
        Err(e) => eprintln!("[metrics] {e} - will retry"),
    }
    if buf.len() > 20_000 {
        buf.drain(..buf.len() - 20_000); // keep the newest points if the database stays down
    }
}

pub async fn run(cfg: MetricsCfg, data_dir: String, mut rx: watch::Receiver<Value>, stop: Arc<Notify>) {
    if cfg.url.is_empty() && !cfg.csv {
        stop.notified().await;
        return;
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().unwrap_or_default();
    let csv_path = std::path::Path::new(&data_dir).join("balance.csv");
    let _ = std::fs::create_dir_all(&data_dir);
    if cfg.csv && !csv_path.exists() {
        let _ = std::fs::write(&csv_path, "time,player,balance,session_delta,kills,downs,assists\n");
    }
    if !cfg.url.is_empty() {
        eprintln!("[metrics] sending to {}", cfg.url);
    }
    let mut buf: Vec<String> = Vec::new();
    let mut last_key = String::new();
    let mut flush_tick = tokio::time::interval(Duration::from_secs(5));
    let mut beat = tokio::time::interval(Duration::from_secs(30));

    let mut record = |snap: &Value, force: bool, buf: &mut Vec<String>| {
        let k = key(snap);
        if !force && k == last_key {
            return;
        }
        let changed = k != last_key;
        last_key = k;
        let now = chrono::Utc::now();
        if let Some(l) = line(snap, now.timestamp_nanos_opt().unwrap_or(0)) {
            buf.push(l);
        }
        if cfg.csv && changed {
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&csv_path) {
                let _ = f.write_all(csv_row(snap, now).as_bytes());
            }
        }
    };

    loop {
        tokio::select! {
            r = rx.changed() => {
                if r.is_err() { break; }
                let snap = rx.borrow_and_update().clone();
                record(&snap, false, &mut buf);
            }
            _ = beat.tick() => {
                let snap = rx.borrow().clone();
                record(&snap, true, &mut buf);
            }
            _ = flush_tick.tick() => flush(&client, &cfg, &mut buf).await,
            _ = stop.notified() => {
                let snap = rx.borrow().clone();
                record(&snap, true, &mut buf);
                flush(&client, &cfg, &mut buf).await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_protocol() {
        let snap = serde_json::json!({"player": "Big Dog, Jr", "balance_now": -8000, "balance_delta": 0,
            "kills": 3, "downs": 1, "kd": 3.0, "downed": false, "xp": 750});
        let l = line(&snap, 1_700_000_000_000_000_000).unwrap();
        assert_eq!(
            l,
            "wardogs,player=Big\\ Dog\\,\\ Jr balance=-8000i,balance_delta=0i,kills=3i,downs=1i,xp=750i,kd=3,downed=0i 1700000000000000000"
        );
    }
    #[test]
    fn no_balance_yet() {
        let snap = serde_json::json!({"player": "A", "balance_now": null, "kills": 0});
        assert!(!line(&snap, 1).unwrap().contains("balance="));
    }
    #[test]
    fn rank_field() {
        let snap = serde_json::json!({"player": "A", "balance_now": 100, "rank": 147, "downed": false});
        assert!(line(&snap, 1).unwrap().contains("rank=147i"));
        // absent (mode != solde, or not seen yet this session): no field at all, not rank=0i
        let snap = serde_json::json!({"player": "A", "balance_now": 100, "downed": false});
        assert!(!line(&snap, 1).unwrap().contains("rank="));
    }
}
