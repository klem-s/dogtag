//! One session: just the total balance and the rank/level badge.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub player: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub balance_start: Option<i64>,
    pub balance_now: Option<i64>,
    /// the rank/level badge, glued to the balance on the end-of-round layout - only ever set near
    /// the end of a session, since that's the only screen it's visible on.
    #[serde(default)]
    pub rank: Option<i64>,
    /// rounds ending in VICTORY/DEFEAT this session, from the same end-of-round screen as `rank`.
    #[serde(default)]
    pub wins: u32,
    #[serde(default)]
    pub losses: u32,
}

impl Session {
    pub fn new(player: &str) -> Self {
        let now = Utc::now();
        Self {
            id: format!("{}-{}", now.format("%Y%m%d-%H%M%S"), player.replace(|c: char| !c.is_ascii_alphanumeric(), "")),
            player: player.to_string(),
            started_at: now,
            ended_at: None,
            balance_start: None,
            balance_now: None,
            rank: None,
            wins: 0,
            losses: 0,
        }
    }

    pub fn set_balance(&mut self, b: i64) {
        if self.balance_start.is_none() {
            self.balance_start = Some(b);
        }
        self.balance_now = Some(b);
    }

    /// A big change that could not be explained (it stayed on the HUD a minute): take it as the balance,
    /// but move the session start with it so the session result does not jump.
    pub fn rebase_balance(&mut self, b: i64) {
        if let (Some(start), Some(now)) = (self.balance_start, self.balance_now) {
            self.balance_start = Some(start + (b - now));
        }
        self.set_balance(b);
    }

    /// Balance change over the session.
    pub fn balance_delta(&self) -> i64 {
        match (self.balance_start, self.balance_now) {
            (Some(a), Some(b)) => b - a,
            _ => 0,
        }
    }

    pub fn minutes(&self) -> f64 {
        let end = self.ended_at.unwrap_or_else(Utc::now);
        (end - self.started_at).num_seconds() as f64 / 60.0
    }

    /// What the overlay and the push receive: the session plus the derived numbers.
    pub fn snapshot(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.insert("balance_delta".into(), serde_json::json!(self.balance_delta()));
            o.insert("minutes".into(), serde_json::json!(self.minutes().round()));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn balance_and_rank() {
        let mut s = Session::new("Test");
        s.set_balance(1000);
        s.set_balance(1600);
        assert_eq!(s.balance_delta(), 600);
        assert_eq!(s.rank, None);
        s.rank = Some(147);
        assert_eq!(s.rank, Some(147));
    }

    #[test]
    fn rebase_moves_the_start_with_it() {
        let mut s = Session::new("Test");
        s.set_balance(1000);
        s.rebase_balance(90_000);
        // the jump is absorbed: it never shows up as a session gain
        assert_eq!(s.balance_delta(), 0);
        assert_eq!(s.balance_now, Some(90_000));
    }
}
