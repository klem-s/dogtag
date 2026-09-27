//! `dogtag serve-stats`: a minimal server that receives sessions from any number of players and serves
//! a leaderboard. Run it on a VPS / Raspberry Pi and point everyone's push.endpoint at it.
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Store {
    path: PathBuf,
    token: String,
    sessions: Mutex<Vec<Value>>,
}

pub async fn run(port: u16, token: String, path: PathBuf) -> anyhow::Result<()> {
    let sessions: Vec<Value> = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    eprintln!("[stats] {} sessions loaded from {}", sessions.len(), path.display());
    let store = Arc::new(Store { path, token, sessions: Mutex::new(sessions) });
    let app = Router::new()
        .route("/api/sessions", post(receive).get(list))
        .route("/api/leaderboard", get(leaderboard))
        .with_state(store);
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!("[stats] listening on :{port}  (POST /api/sessions, GET /api/leaderboard)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn receive(State(st): State<Arc<Store>>, headers: HeaderMap, Json(body): Json<Value>) -> StatusCode {
    if !st.token.is_empty() {
        let ok = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .map(|h| h == format!("Bearer {}", st.token))
            .unwrap_or(false);
        if !ok {
            return StatusCode::UNAUTHORIZED;
        }
    }
    let Some(sess) = body.get("session") else { return StatusCode::BAD_REQUEST };
    let id = sess.get("id").cloned().unwrap_or(Value::Null);
    let mut all = st.sessions.lock().unwrap();
    if all.iter().any(|v| v["session"]["id"] == id) {
        return StatusCode::OK; // already have it (a retried push)
    }
    // keep the stored copy small: the event log stays on the player's PC
    let mut stored = body.clone();
    if let Some(s) = stored.get_mut("session").and_then(|s| s.as_object_mut()) {
        s.remove("events");
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&st.path) {
        let _ = writeln!(f, "{stored}");
    }
    all.push(stored);
    StatusCode::CREATED
}

#[derive(Deserialize)]
struct Filter {
    player: Option<String>,
}

async fn list(State(st): State<Arc<Store>>, Query(f): Query<Filter>) -> Json<Vec<Value>> {
    let all = st.sessions.lock().unwrap();
    Json(
        all.iter()
            .filter(|v| f.player.as_deref().map_or(true, |p| v["session"]["player"] == p))
            .cloned()
            .collect(),
    )
}

async fn leaderboard(State(st): State<Arc<Store>>) -> Json<Value> {
    #[derive(Default)]
    struct Agg {
        sessions: u64,
        kills: i64,
        downs: i64,
        assists: i64,
        revives: i64,
        vehicles: i64,
        balance: i64,
        minutes: f64,
    }
    let all = st.sessions.lock().unwrap();
    let mut by: HashMap<String, Agg> = HashMap::new();
    for v in all.iter() {
        let s = &v["session"];
        let n = |k: &str| s[k].as_i64().unwrap_or(0);
        let a = by.entry(s["player"].as_str().unwrap_or("?").to_string()).or_default();
        a.sessions += 1;
        a.kills += n("kills");
        a.downs += n("downs");
        a.assists += n("assists");
        a.revives += n("revives");
        a.vehicles += n("vehicles");
        a.balance += n("balance_delta");
        a.minutes += s["minutes"].as_f64().unwrap_or(0.0);
    }
    let mut rows: Vec<Value> = by
        .into_iter()
        .map(|(p, a)| {
            json!({
                "player": p, "sessions": a.sessions, "kills": a.kills, "downs": a.downs,
                "kd": ((a.kills as f64 / a.downs.max(1) as f64) * 100.0).round() / 100.0,
                "assists": a.assists, "revives": a.revives, "vehicles": a.vehicles,
                "balance": a.balance, "hours": (a.minutes / 6.0).round() / 10.0,
                "kills_per_hour": if a.minutes > 0.0 { ((a.kills as f64 / (a.minutes / 60.0)) * 10.0).round() / 10.0 } else { 0.0 },
            })
        })
        .collect();
    rows.sort_by(|a, b| b["kills"].as_i64().cmp(&a["kills"].as_i64()));
    Json(json!({ "players": rows }))
}
