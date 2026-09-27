//! Local web server for OBS: the overlay page, a WebSocket with live stats, and a tiny control API.
use crate::session::Session;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use std::sync::{Arc, Mutex};
use tokio::sync::{watch, Notify};

pub struct Shared {
    pub session: Mutex<Session>,
    pub tx: watch::Sender<serde_json::Value>,
    /// fired by POST /api/end (or Ctrl+C in main)
    pub end: Notify,
}

impl Shared {
    pub fn new(session: Session) -> Arc<Self> {
        let (tx, _) = watch::channel(session.snapshot());
        Arc::new(Self { session: Mutex::new(session), tx, end: Notify::new() })
    }

    /// Change the session and tell every overlay.
    pub fn with<R>(&self, f: impl FnOnce(&mut Session) -> R) -> R {
        let mut s = self.session.lock().unwrap();
        let r = f(&mut s);
        let _ = self.tx.send(s.snapshot());
        r
    }
}

const OVERLAY: &str = include_str!("../overlay/overlay.html");

pub async fn serve(shared: Arc<Shared>, bind: String, port: u16) -> anyhow::Result<()> {
    let app = Router::new()
        .route("/", get(|| async { Html(OVERLAY) }))
        .route("/ws", get(ws))
        .route("/metrics", get(metrics))
        .route("/api/session", get(|State(s): State<Arc<Shared>>| async move { Json(s.tx.borrow().clone()) }))
        .route(
            "/api/end",
            post(|State(s): State<Arc<Shared>>| async move {
                s.end.notify_one();
                "ending session"
            }),
        )
        .with_state(shared);
    let listener = tokio::net::TcpListener::bind((bind.as_str(), port)).await?;
    eprintln!("[overlay] OBS browser source: http://127.0.0.1:{port}/  (Prometheus: /metrics)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn ws(up: WebSocketUpgrade, State(s): State<Arc<Shared>>) -> impl IntoResponse {
    up.on_upgrade(move |sock| feed(sock, s))
}

async fn feed(mut sock: WebSocket, s: Arc<Shared>) {
    let mut rx = s.tx.subscribe();
    loop {
        let text = rx.borrow_and_update().to_string();
        if sock.send(Message::Text(text.into())).await.is_err() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Prometheus text format, for Grafana. One series per player; counters restart with each session.
async fn metrics(State(s): State<Arc<Shared>>) -> impl IntoResponse {
    let v = s.tx.borrow().clone();
    let player = v["player"].as_str().unwrap_or("?").replace(['"', '\\', '\n'], "");
    let mut out = String::new();
    let mut g = |name: &str, help: &str, val: Option<f64>| {
        if let Some(x) = val {
            out.push_str(&format!("# HELP dogtag_{name} {help}\n# TYPE dogtag_{name} gauge\ndogtag_{name}{{player=\"{player}\"}} {x}\n"));
        }
    };
    let n = |k: &str| v[k].as_f64();
    g("up", "1 while dogtag is running", Some(1.0));
    g("balance_dollars", "In-game balance as read on the HUD", n("balance_now"));
    g("session_balance_delta_dollars", "Balance change since the session started", n("balance_delta"));
    g("rank", "Rank/level badge, as last read on the end-of-round screen", n("rank"));
    g("session_minutes", "Minutes since the session started", n("minutes"));
    ([("content-type", "text/plain; version=0.0.4")], out)
}
