//! config.toml: what to capture, where the HUD sits, where sessions go.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Your in-game name, sent with every session.
    pub player: String,
    /// "full": everything (rewards, downs, money). "money": only your total money and its change.
    pub mode: String,
    pub capture: CaptureCfg,
    pub ocr: OcrCfg,
    pub regions: Regions,
    pub overlay: OverlayCfg,
    pub push: PushCfg,
    pub metrics: MetricsCfg,
    pub balance: BalanceCfg,
}

/// Guards against screens that show another amount than your match balance (death / deploy screen).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BalanceCfg {
    /// A change bigger than this ($) is suspicious...
    pub max_jump: i64,
    /// ...and must stay on screen this many seconds before it is believed.
    pub big_jump_hold_s: f32,
    /// Do not read the balance / match change while down (and just after: the map may hide it).
    pub freeze_when_downed: bool,
    /// "DAMAGE LOG" coming back within this many seconds is the same down (you looked at the map).
    pub same_down_within_s: f32,
}

impl Default for BalanceCfg {
    fn default() -> Self {
        Self { max_jump: 20_000, big_jump_hold_s: 60.0, freeze_when_downed: true, same_down_within_s: 45.0 }
    }
}

/// Live time series for Grafana: InfluxDB line protocol, understood by InfluxDB 1/2/3 and VictoriaMetrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsCfg {
    /// e.g. "http://localhost:8428/write" (VictoriaMetrics, see grafana/) or
    /// "http://localhost:8086/api/v2/write?org=me&bucket=wardogs&precision=ns" (InfluxDB 2). Empty = off.
    pub url: String,
    /// InfluxDB 2/3 token (sent as "Authorization: Token <token>"). Empty for VictoriaMetrics.
    pub token: String,
    /// Also append every change to data/balance.csv (opens in Excel / Grafana CSV plugin).
    pub csv: bool,
}

impl Default for MetricsCfg {
    fn default() -> Self {
        Self { url: String::new(), token: String::new(), csv: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureCfg {
    /// Part of the game window title. Empty = capture a monitor instead.
    pub window_title: String,
    /// Monitor index when window_title is empty (0 = primary).
    pub monitor: usize,
    /// Reads per second of the money/reward corner.
    pub fps: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OcrCfg {
    pub detection_model: String,
    pub recognition_model: String,
    /// Upscale factor applied to each crop before OCR (HUD text is small).
    pub scale: f32,
    /// Turn light HUD text into dark text on white before OCR.
    pub invert: bool,
    /// Pixels brighter than this (0-255) count as HUD text when binarizing. 0 = keep greyscale.
    pub threshold: u8,
}

/// A region as fractions of the frame. Like the game HUD, x is anchored to the RIGHT edge and sized from
/// the frame HEIGHT, so the same numbers work at 16:9, 21:9 and any resolution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Region {
    /// Distance of the region's LEFT edge from the frame's left edge, in frame heights. When set, the
    /// region is anchored to the left instead of the right (and `right` is ignored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<f32>,
    /// Distance of the region's right edge from the frame's right edge, in frame heights.
    #[serde(default)]
    pub right: f32,
    /// Width in frame heights.
    pub width: f32,
    /// Top in frame heights.
    pub top: f32,
    /// Height in frame heights.
    pub height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Regions {
    /// Balance + reward lines ("KILL 250XP", "CONTROL ZONE PRESENCE +$150").
    pub cash: Region,
    /// Where "DAMAGE LOG" shows up while you are downed. Read once a second.
    pub downed: Region,
    /// Your balance on its own (e.g. top left of the screen). When set, the balance is read here
    /// instead of in `cash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<Region>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlayCfg {
    pub port: u16,
    /// "127.0.0.1" = this PC only. "0.0.0.0" lets Prometheus in Docker read /metrics.
    pub bind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PushCfg {
    /// Any HTTP endpoint that accepts the session JSON (your own server, `dogtag serve-stats`, ...).
    pub endpoint: String,
    /// Sent as "Authorization: Bearer <token>".
    pub token: String,
    /// Discord webhook URL: posts a summary card at the end of each session.
    pub discord_webhook: String,
    /// Folder for the local copy of every session and the retry queue.
    pub data_dir: String,
    /// Sessions shorter than this (minutes) are kept locally but not pushed.
    pub min_minutes: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            player: "Player".into(),
            mode: "full".into(),
            capture: CaptureCfg::default(),
            ocr: OcrCfg::default(),
            regions: Regions::default(),
            overlay: OverlayCfg::default(),
            push: PushCfg::default(),
            metrics: MetricsCfg::default(),
            balance: BalanceCfg::default(),
        }
    }
}
impl Default for CaptureCfg {
    fn default() -> Self {
        Self { window_title: "WARDOGS".into(), monitor: 0, fps: 3.0 }
    }
}
impl Default for OcrCfg {
    fn default() -> Self {
        Self {
            detection_model: "models/text-detection.rten".into(),
            recognition_model: "models/text-recognition.rten".into(),
            scale: 2.0,
            invert: true,
            threshold: 0,
        }
    }
}
impl Default for Regions {
    fn default() -> Self {
        Self {
            // Kennel.gg's measured cash corner: 0.42 H wide at the right edge, 0.22 H tall at the top.
            cash: Region { left: None, right: 0.0, width: 0.42, top: 0.0, height: 0.22 },
            downed: Region { left: None, right: 0.0, width: 0.60, top: 0.25, height: 0.60 },
            balance: None,
        }
    }
}
impl Default for OverlayCfg {
    fn default() -> Self {
        Self { port: 47900, bind: "127.0.0.1".into() }
    }
}
impl Default for PushCfg {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            token: String::new(),
            discord_webhook: String::new(),
            data_dir: "data".into(),
            min_minutes: 5.0,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            let cfg = Config::default();
            std::fs::write(path, toml::to_string_pretty(&cfg)?)
                .with_context(|| format!("writing default {}", path.display()))?;
            eprintln!("[config] wrote a default {} - edit it and run again if needed", path.display());
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

impl Region {
    /// Pixel rectangle (x, y, w, h) of this region inside a w x h frame, clamped to the frame.
    pub fn rect(&self, fw: u32, fh: u32) -> (u32, u32, u32, u32) {
        let h = fh as f32;
        let (x0, x1) = match self.left {
            Some(l) => {
                let x0 = (l * h).clamp(0.0, fw as f32);
                (x0, (x0 + self.width * h).clamp(x0, fw as f32))
            }
            None => {
                let x1 = (fw as f32 - self.right * h).clamp(0.0, fw as f32);
                ((x1 - self.width * h).clamp(0.0, x1), x1)
            }
        };
        let y0 = (self.top * h).clamp(0.0, h);
        let y1 = ((self.top + self.height) * h).clamp(y0, h);
        (x0 as u32, y0 as u32, ((x1 - x0) as u32).max(1), ((y1 - y0) as u32).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cash_region_1080p_and_ultrawide() {
        let r = Regions::default().cash;
        assert_eq!(r.rect(1920, 1080), (1466, 0, 453, 237));
        // 21:9 keeps the same size, still hugging the right edge
        assert_eq!(r.rect(2560, 1080), (2106, 0, 453, 237));
    }
    #[test]
    fn left_anchored() {
        let r = Region { left: Some(0.02), right: 0.0, width: 0.30, top: 0.01, height: 0.06 };
        assert_eq!(r.rect(1920, 1080), (21, 10, 324, 64));
    }
}
