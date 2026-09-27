//! dogtag - session stats for WARDOGS.
//!
//!   dogtag [run]                 read the game live (Windows), overlay + push at the end
//!   dogtag replay <folder>       same thing on a folder of screenshots (any OS, for testing)
//!   dogtag calibrate <image>     save what the reader sees in one screenshot and print what it reads
//!   dogtag serve-stats           receive everyone's sessions and serve a leaderboard
//!
//! Options: --config <file> (default config.toml), --mode full|money, --debug (print every OCR read),
//!          serve-stats: --port --token --data
mod capture;
mod collector;
mod config;
mod hud;
mod metrics;
mod parse;
mod push;
mod server;
mod session;
mod tracker;

use anyhow::{bail, Result};
use capture::FrameSource;
use config::Config;
use server::Shared;
use session::Session;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

type SourceMaker = Box<dyn FnOnce() -> Result<Box<dyn FrameSource>> + Send>;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

/// Options that take a value; everything else starting with "--" is a switch (--debug).
const VALUE_OPTS: &[&str] = &["--config", "--mode", "--port", "--token", "--data"];

/// The command and its arguments, without the options: `dogtag --debug --mode money` -> [].
fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
        } else if VALUE_OPTS.contains(&a.as_str()) {
            skip = true;
        } else if !a.starts_with("--") {
            out.push(a.clone());
        }
    }
    out
}

fn load_cfg(path: &Path, args: &[String]) -> Result<Config> {
    let mut cfg = Config::load(path)?;
    if let Some(m) = arg(args, "--mode") {
        cfg.mode = m;
    }
    cfg.mode = match cfg.mode.to_lowercase().as_str() {
        "money" | "argent" => "money".into(),
        "solde" | "total" => "solde".into(),
        "full" | "complet" | "" => "full".into(),
        other => bail!("mode {other:?} inconnu : \"full\", \"money\" ou \"solde\""),
    };
    Ok(cfg)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg_path = PathBuf::from(arg(&args, "--config").unwrap_or_else(|| "config.toml".into()));
    let pos = positional(&args);
    let cmd = pos.first().map(String::as_str).unwrap_or("run");
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", include_str!("main.rs").lines().take(9).map(|l| l.trim_start_matches("//!")).collect::<Vec<_>>().join("\n"));
        return Ok(());
    }

    match cmd {
        "serve-stats" => {
            let port = arg(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(8787);
            let token = arg(&args, "--token").unwrap_or_default();
            let data = PathBuf::from(arg(&args, "--data").unwrap_or_else(|| "stats.jsonl".into()));
            collector::run(port, token, data).await
        }
        "calibrate" => {
            let Some(img) = pos.get(1) else { bail!("usage: dogtag calibrate <screenshot.png>") };
            calibrate(&load_cfg(&cfg_path, &args)?, Path::new(img))
        }
        "replay" => {
            let Some(dir) = pos.get(1).cloned() else { bail!("usage: dogtag replay <folder>") };
            let cfg = load_cfg(&cfg_path, &args)?;
            let maker: SourceMaker =
                Box::new(move || Ok(Box::new(capture::FolderReplay::new(Path::new(&dir))?) as Box<dyn FrameSource>));
            session_main(cfg, maker, false).await
        }
        "run" => {
            let cfg = load_cfg(&cfg_path, &args)?;
            live(cfg).await
        }
        other => bail!("unknown command {other:?} (try --help)"),
    }
}

#[cfg(windows)]
async fn live(cfg: Config) -> Result<()> {
    let cap = cfg.capture.clone();
    let maker: SourceMaker =
        Box::new(move || Ok(Box::new(capture::LiveCapture::new(&cap)?) as Box<dyn FrameSource>));
    session_main(cfg, maker, true).await
}

#[cfg(not(windows))]
async fn live(_cfg: Config) -> Result<()> {
    bail!("live capture is Windows only; use `dogtag replay <folder>` to test on screenshots")
}

/// Runs one session: reader thread + overlay server, until Ctrl+C, POST /api/end, or the replay ends.
async fn session_main(cfg: Config, maker: SourceMaker, realtime: bool) -> Result<()> {
    push::retry_pending(&cfg.push).await;
    let mut sess = Session::new(&cfg.player);
    sess.mode = cfg.mode.clone();
    let shared = Shared::new(sess);
    eprintln!("[mode] {}", match cfg.mode.as_str() {
        "money" => "money : seulement ton argent total et sa variation",
        "solde" => "solde : seulement ton argent total, rien d'autre",
        _ => "full : récompenses, downs et argent",
    });
    let stop = Arc::new(AtomicBool::new(false));

    let srv = tokio::spawn(server::serve(shared.clone(), cfg.overlay.bind.clone(), cfg.overlay.port));
    let metrics_stop = Arc::new(tokio::sync::Notify::new());
    let metrics = tokio::spawn(metrics::run(cfg.metrics.clone(), cfg.push.data_dir.clone(), shared.tx.subscribe(), metrics_stop.clone()));

    let (c, sh, st) = (cfg.clone(), shared.clone(), stop.clone());
    let reader = std::thread::spawn(move || {
        if let Err(e) = read_loop(&c, maker, &sh, &st, realtime) {
            eprintln!("[reader] stopped: {e:#}");
        }
        sh.end.notify_one();
    });

    eprintln!("[session] started - Ctrl+C (or POST /api/end) ends it and pushes the stats");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = shared.end.notified() => {},
    }
    stop.store(true, Ordering::SeqCst);
    let _ = tokio::task::spawn_blocking(move || reader.join()).await;

    let session = shared.with(|s| {
        s.ended_at = Some(chrono::Utc::now());
        s.clone()
    });
    if session.mode == "solde" {
        eprintln!("[session] {} min - solde {:?}", session.minutes().round(), session.balance_now);
    } else if session.mode == "money" {
        eprintln!(
            "[session] {} min - solde {:?} -> {:?}, balance {:+}",
            session.minutes().round(),
            session.balance_start,
            session.balance_now,
            session.balance_delta()
        );
    } else {
    eprintln!(
        "[session] {} min - {} kills, {} downs, K/D {:.2}, balance {:+}",
        session.minutes().round(),
        session.kills,
        session.downs,
        session.kd(),
        session.balance_delta()
    );
    }
    metrics_stop.notify_one();
    let _ = metrics.await;
    push::finish(&cfg.push, &session).await?;
    srv.abort();
    Ok(())
}

fn read_loop(cfg: &Config, maker: SourceMaker, shared: &Shared, stop: &AtomicBool, realtime: bool) -> Result<()> {
    let reader = hud::Reader::new(&cfg.ocr)?;
    let mut src = maker()?;
    let mut tracker = tracker::Tracker::new(2, Duration::from_millis(1500));
    let period = Duration::from_secs_f32(1.0 / cfg.capture.fps.clamp(0.5, 10.0));
    // replays run as fast as the OCR goes, on a simulated clock so de-duplication behaves like live
    let t0 = Instant::now();
    let mut sim = t0;
    let mut last_downed_read = t0 - Duration::from_secs(10);
    let mut last_victory_read = t0 - Duration::from_secs(10);
    let mut last_victory_scores: Vec<i64> = Vec::new();
    let mut last_rank: Option<i64> = None;
    let mut downs = tracker::DownTracker::new(Duration::from_secs_f32(cfg.balance.same_down_within_s.max(0.0)));
    let debug = std::env::args().any(|a| a == "--debug");
    let mut last_lines: Vec<String> = Vec::new();
    let money_only = cfg.mode == "money" || cfg.mode == "solde";
    let solde_only = cfg.mode == "solde";
    let mut bal_filter = tracker::BalanceFilter::new(
        3,
        cfg.balance.max_jump,
        Duration::from_secs_f32(cfg.balance.big_jump_hold_s.max(0.0)),
    );
    let mut last_change: Option<(i64, u32)> = None;
    let mut shown_change: Option<i64> = None;
    let mut last_held: Option<i64> = None;

    while !stop.load(Ordering::SeqCst) {
        let tick = Instant::now();
        let Some(frame) = src.next_frame()? else { break };
        let now = if realtime { tick } else { sim };
        sim += period;
        if frame.width() < 100 {
            std::thread::sleep(Duration::from_secs(1)); // game not found yet
            continue;
        }

        let plines = reader.read_positioned(&frame, &cfg.regions.cash).unwrap_or_default();
        let lines: Vec<String> = plines.iter().map(|(t, _)| t.clone()).collect();
        if debug && lines != last_lines {
            eprintln!("[ocr] {}", if lines.is_empty() { "(rien lu)".to_string() } else { lines.join(" | ") });
            last_lines = lines.clone();
        }
        let reading = parse::parse_positioned(&plines);
        // rank/level badge, glued to the balance on the end-of-round layout: diagnostic only for now,
        // like [regions.victory] - not wired into the session/metrics yet.
        if debug {
            if let Some(rank) = lines.iter().find_map(|l| parse::rank_in(l)) {
                if Some(rank) != last_rank {
                    eprintln!("[rank] {rank}");
                    last_rank = Some(rank);
                }
            }
        }
        // the match change: accepted when read the same on 3 frames in a row
        last_change = match (reading.match_change, last_change) {
            (Some(c), Some((p, n))) if c == p => Some((c, n + 1)),
            (Some(c), _) => Some((c, 1)),
            (None, _) => None,
        };
        let match_change = last_change.filter(|(_, n)| *n >= 3).map(|(c, _)| c);
        let fresh = if money_only { Vec::new() } else { tracker.update(&reading.rewards, now) };
        let balance_read = match &cfg.regions.balance {
            Some(region) => {
                let bl = reader.read_lines(&frame, region).unwrap_or_default();
                if debug && !bl.is_empty() {
                    eprintln!("[ocr solde] {}", bl.join(" | "));
                }
                parse::balance_in(&bl)
            }
            None => reading.balance,
        };
        // down (or just after, map open): the money boxes are not trusted, reward lines still count
        let frozen = cfg.balance.freeze_when_downed && downs.quiet(now);
        let hud_ok = reading.hud_layout || cfg.regions.balance.is_some();
        let mut rebase = None;
        let balance = match bal_filter.update_read(balance_read, now, frozen, hud_ok) {
            tracker::BalanceVerdict::Accepted(b) => Some(b),
            tracker::BalanceVerdict::AcceptedJump(b) => {
                eprintln!("[solde] {b}$ : gros changement resté affiché, la session se recale dessus (pas de saut)");
                rebase = Some(b);
                None
            }
            tracker::BalanceVerdict::Held(b) => {
                if last_held != Some(b) {
                    eprintln!("[solde ignoré] {b}$ : saut trop grand (écran de mort / fin de partie ?)");
                    last_held = Some(b);
                }
                None
            }
            tracker::BalanceVerdict::Nothing => None,
        };

        // downed check about once a second (it is a bigger area)
        let mut downed = None;
        if now.duration_since(last_downed_read) >= Duration::from_millis(900) {
            last_downed_read = now;
            let text = reader.read_lines(&frame, &cfg.regions.downed).unwrap_or_default().join(" ").to_uppercase();
            let seen = text.contains("DAMAGE LOG") || (text.contains("DAMAGE") && text.contains("LOG"));
            downed = downs.read(seen, now);
        }

        // end-of-round panel (3 team scores): diagnostic only for now, not wired into the session yet -
        // `[regions.victory]` is unset by default, and even calibrated it only prints in --debug so we can
        // confirm the region/parsing are right on real footage before it feeds anything.
        if let Some(region) = &cfg.regions.victory {
            if now.duration_since(last_victory_read) >= Duration::from_millis(900) {
                last_victory_read = now;
                let lines = reader.read_lines(&frame, region).unwrap_or_default();
                let scores = parse::team_scores(&lines);
                if debug && scores != last_victory_scores && !scores.is_empty() {
                    eprintln!("[victory] {:?} => scores {:?}", lines, scores);
                    last_victory_scores = scores;
                }
            }
        }

        // the match change only counts when the balance box is on screen with it (a stray "$200" from the
        // map is not a match change) and not while down
        let match_change = if solde_only || frozen || reading.balance.is_none() { None } else { match_change };
        let match_change = match_change.filter(|c| shown_change != Some(*c));
        if let Some(c) = match_change {
            shown_change = Some(c);
        }
        let downed = if money_only { None } else { downed };
        if !fresh.is_empty() || balance.is_some() || rebase.is_some() || downed.is_some() || match_change.is_some() {
            shared.with(|s| {
                for r in &fresh {
                    eprintln!("[+] {}{}{}", r.reason,
                        r.amount.map(|a| format!(" {a:+}$")).unwrap_or_default(),
                        r.xp.map(|x| format!(" {x}XP")).unwrap_or_default());
                    s.add_reward(r);
                }
                if let Some(c) = match_change {
                    eprintln!("[partie] {c:+}$");
                    s.match_change = Some(c);
                }
                if let Some(b) = balance {
                    eprintln!("[solde] {b}$");
                    s.set_balance(b);
                }
                if let Some(b) = rebase {
                    s.rebase_balance(b);
                }
                match &downed {
                    // money mode: downs only pause the money reading, they are not counted
                    _ if money_only => {}
                    Some(tracker::DownEvent::NewDown) => {
                        s.set_downed_counting(true, true);
                        eprintln!("[+] DOWNED");
                    }
                    Some(tracker::DownEvent::SameDown) => {
                        s.set_downed_counting(true, false);
                        eprintln!("[=] même down (carte fermée)");
                    }
                    Some(tracker::DownEvent::Up) => {
                        s.set_downed_counting(false, false);
                    }
                    None => {}
                }
            });
        }

        if realtime {
            if let Some(rest) = period.checked_sub(tick.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    }
    Ok(())
}

fn calibrate(cfg: &Config, img: &Path) -> Result<()> {
    let frame = image::open(img)?.into_rgba8();
    std::fs::create_dir_all("calibrate")?;
    eprintln!("frame {}x{}", frame.width(), frame.height());
    let mut regions = vec![("cash", cfg.regions.cash), ("downed", cfg.regions.downed)];
    if let Some(b) = cfg.regions.balance {
        regions.push(("balance", b));
    }
    if let Some(v) = cfg.regions.victory {
        regions.push(("victory", v));
    }
    for (name, region) in &regions {
        let (x, y, w, h) = region.rect(frame.width(), frame.height());
        let out = format!("calibrate/{name}.png");
        hud::prepare(&frame, region, &cfg.ocr).save(&out)?;
        eprintln!("\n== {name}: x={x} y={y} {w}x{h} -> {out}");
    }
    let reader = match hud::Reader::new(&cfg.ocr) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("\n(no OCR: {e:#})\nCheck the crops in calibrate/ - the regions are the part you can fix without the models.");
            return Ok(());
        }
    };
    for (name, region) in &regions {
        let lines = reader.read_lines(&frame, region)?;
        println!("\n== {name} reads:");
        if *name == "balance" {
            println!("  {:<40} => balance {:?}", lines.join(" | "), parse::balance_in(&lines));
            continue;
        }
        if *name == "victory" {
            println!("  {:<40} => team scores {:?}", lines.join(" | "), parse::team_scores(&lines));
            continue;
        }
        for l in &lines {
            let what = match parse::parse_line(l) {
                parse::Line::Money => {
                    let (b, c, _) = parse::money_boxes(l, None);
                    format!("solde {b:?}, variation de partie {c:?}")
                }
                parse::Line::Reward(r) => format!("reward {:?} amount {:?} xp {:?}{}", r.reason, r.amount, r.xp,
                    if r.known { "" } else { " (unknown reason)" }),
                parse::Line::Noise => "-".into(),
            };
            println!("  {l:<40} => {what}");
        }
    }
    Ok(())
}
