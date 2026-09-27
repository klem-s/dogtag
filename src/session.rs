//! One streaming/playing session: counters, money, and the raw event log.
use crate::parse::Reward;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub kind: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub xp: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// "full" or "money"
    #[serde(default)]
    pub mode: String,
    pub player: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub kills: u32,
    pub downs: u32,
    pub assists: u32,
    pub headshots: u32,
    pub revives: u32,
    pub vehicles: u32,
    pub objectives: u32,
    pub xp: i64,
    pub money_earned: i64,
    pub money_spent: i64,
    pub balance_start: Option<i64>,
    pub balance_now: Option<i64>,
    /// the game's own match change box (-$13,393), as last read
    #[serde(default)]
    pub match_change: Option<i64>,
    /// the rank/level badge, glued to the balance on the end-of-round layout - only ever set near
    /// the end of a session, since that's the only screen it's visible on.
    #[serde(default)]
    pub rank: Option<i64>,
    /// every reason seen, with how many times
    pub reasons: BTreeMap<String, u32>,
    pub events: Vec<Event>,
    /// true while "DAMAGE LOG" is on screen
    pub downed: bool,
}

impl Session {
    pub fn new(player: &str) -> Self {
        let now = Utc::now();
        Self {
            mode: "full".into(),
            id: format!("{}-{}", now.format("%Y%m%d-%H%M%S"), player.replace(|c: char| !c.is_ascii_alphanumeric(), "")),
            player: player.to_string(),
            started_at: now,
            ended_at: None,
            kills: 0,
            downs: 0,
            assists: 0,
            headshots: 0,
            revives: 0,
            vehicles: 0,
            objectives: 0,
            xp: 0,
            money_earned: 0,
            money_spent: 0,
            balance_start: None,
            balance_now: None,
            match_change: None,
            rank: None,
            reasons: BTreeMap::new(),
            events: Vec::new(),
            downed: false,
        }
    }

    /// Which counter a reason goes to.
    pub fn kind_of(reason: &str) -> &'static str {
        match reason {
            "KILL" | "REVENGE KILL" => "kill",
            "HEADSHOT" => "headshot",
            "ASSIST" | "SUPPLIED PLAYER ASSIST" => "assist",
            "REVIVED TEAMMATE" => "revive",
            "VEHICLE DESTROYED" | "ROTORS DESTROYED" => "vehicle",
            r if r.contains("ZONE") => "objective",
            _ => "other",
        }
    }

    pub fn add_reward(&mut self, r: &Reward) {
        let kind = Self::kind_of(&r.reason);
        match kind {
            "kill" => self.kills += 1,
            "headshot" => self.headshots += 1,
            "assist" => self.assists += 1,
            "revive" => self.revives += 1,
            "vehicle" => self.vehicles += 1,
            "objective" => self.objectives += 1,
            _ => {}
        }
        match r.amount {
            Some(a) if a >= 0 => self.money_earned += a,
            Some(a) => self.money_spent += -a,
            None => {}
        }
        self.xp += r.xp.unwrap_or(0);
        *self.reasons.entry(r.reason.clone()).or_default() += 1;
        self.events.push(Event { at: Utc::now(), kind: kind.into(), reason: r.reason.clone(), amount: r.amount, xp: r.xp });
    }

    pub fn set_balance(&mut self, b: i64) {
        if self.balance_start.is_none() {
            self.balance_start = Some(b);
        }
        if self.balance_now != Some(b) {
            let amount = self.balance_now.map(|old| b - old);
            self.events.push(Event { at: Utc::now(), kind: "balance".into(), reason: b.to_string(), amount, xp: None });
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
        self.events.push(Event { at: Utc::now(), kind: "rebase".into(), reason: "BALANCE REBASE".into(), amount: Some(b), xp: None });
    }

    /// Returns true when this is a new down.
    /// `count`: false when this is the same down coming back (map closed), see DownTracker.
    #[cfg(test)]
    pub fn set_downed(&mut self, downed: bool) -> bool {
        self.set_downed_counting(downed, true)
    }

    pub fn set_downed_counting(&mut self, downed: bool, count: bool) -> bool {
        let new = downed && !self.downed && count;
        self.downed = downed;
        if new {
            self.downs += 1;
            self.events.push(Event { at: Utc::now(), kind: "down".into(), reason: "DOWNED".into(), ..Default::default() });
        }
        new
    }

    pub fn kd(&self) -> f64 {
        self.kills as f64 / (self.downs.max(1)) as f64
    }

    /// Balance change over the session (from the HUD balance when read, else earned - spent).
    pub fn balance_delta(&self) -> i64 {
        match (self.balance_start, self.balance_now) {
            (Some(a), Some(b)) => b - a,
            _ => self.money_earned - self.money_spent,
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
            o.insert("kd".into(), serde_json::json!((self.kd() * 100.0).round() / 100.0));
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
    fn counters() {
        let mut s = Session::new("Test");
        s.add_reward(&Reward { reason: "KILL".into(), known: true, amount: None, xp: Some(250) });
        s.add_reward(&Reward { reason: "CONTROL ZONE PRESENCE".into(), known: true, amount: Some(150), xp: None });
        s.add_reward(&Reward { reason: "PURCHASE REFUNDED".into(), known: true, amount: Some(-500), xp: None });
        assert!(s.set_downed(true));
        assert!(!s.set_downed(true));
        s.set_downed(false);
        assert_eq!((s.kills, s.objectives, s.downs, s.xp), (1, 1, 1, 250));
        assert_eq!((s.money_earned, s.money_spent, s.balance_delta()), (150, 500, -350));
        s.set_balance(1000);
        s.set_balance(1600);
        assert_eq!(s.balance_delta(), 600);
    }
}
