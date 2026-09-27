//! Reward lines stay on screen for a few seconds: each one must count once, and only once it has been
//! read the same way on consecutive frames.
use crate::parse::Reward;
use std::collections::HashMap;
use std::time::{Duration, Instant};

struct Seen {
    /// how many identical lines were on the last frame
    last: usize,
    /// how many frames in a row showed at least `pending` lines
    streak: u32,
    /// highest count already reported while the lines stayed visible
    counted: usize,
    /// frames in a row with fewer lines than `counted` (one scrolled away)
    low: u32,
    last_seen: Instant,
}

pub struct Tracker {
    seen: HashMap<Reward, Seen>,
    confirm_frames: u32,
    forget_after: Duration,
}

impl Tracker {
    pub fn new(confirm_frames: u32, forget_after: Duration) -> Self {
        Self { seen: HashMap::new(), confirm_frames, forget_after }
    }

    /// Feeds one frame's rewards; returns the ones that are new.
    pub fn update(&mut self, rewards: &[Reward], now: Instant) -> Vec<Reward> {
        let mut counts: HashMap<&Reward, usize> = HashMap::new();
        for r in rewards {
            *counts.entry(r).or_default() += 1;
        }
        let mut fresh = Vec::new();
        for (r, n) in counts {
            let s = self.seen.entry(r.clone()).or_insert(Seen { last: 0, streak: 0, counted: 0, low: 0, last_seen: now });
            if now.duration_since(s.last_seen) >= self.forget_after {
                // it left the screen long enough ago: this is a new line
                *s = Seen { last: 0, streak: 0, counted: 0, low: 0, last_seen: now };
            }
            // a new count must itself be seen on `confirm_frames` frames before it counts
            s.streak = if n == s.last { s.streak + 1 } else { 1 };
            if n < s.counted {
                s.low += 1;
                if s.low >= self.confirm_frames {
                    s.counted = n;
                }
            } else {
                s.low = 0;
            }
            s.last = n;
            s.last_seen = now;
            if s.streak >= self.confirm_frames && n > s.counted {
                for _ in s.counted..n {
                    fresh.push(r.clone());
                }
                s.counted = n;
            }
        }
        let forget = self.forget_after;
        self.seen.retain(|_, s| now.duration_since(s.last_seen) < forget);
        fresh
    }
}

/// What the balance filter did with a read.
#[derive(Debug, PartialEq)]
pub enum BalanceVerdict {
    /// a new balance is accepted
    Accepted(i64),
    /// a big change that stayed on the normal HUD for `big_hold`: accepted as the balance, but the session
    /// should re-anchor on it rather than count it as won or lost (see Session::rebase_balance)
    AcceptedJump(i64),
    /// a big jump is waiting (the death / deploy screen shows another amount, e.g. the account total)
    Held(i64),
    Nothing,
}

/// Balance reads, with the screens that show ANOTHER amount in mind: while downed nothing is read, and a
/// jump bigger than `max_jump` must stay on screen for `big_hold` before it is believed. The normal HUD
/// comes back within seconds after a death, so the account total shown meanwhile never gets in.
pub struct BalanceFilter {
    pub balance: Option<i64>,
    cand: Option<(i64, Instant, u32)>,
    confirm_frames: u32,
    first_hold: Duration,
    /// any change must be read the same for this long (the HUD rolls through in-between values)
    min_stable: Duration,
    max_jump: i64,
    big_hold: Duration,
}

impl BalanceFilter {
    pub fn new(confirm_frames: u32, max_jump: i64, big_hold: Duration) -> Self {
        Self {
            balance: None,
            cand: None,
            confirm_frames,
            first_hold: Duration::from_secs(3),
            min_stable: Duration::from_millis(900),
            max_jump,
            big_hold,
        }
    }

    #[cfg(test)]
    pub fn update(&mut self, read: Option<i64>, now: Instant, frozen: bool) -> BalanceVerdict {
        self.update_read(read, now, frozen, true)
    }

    /// `hud`: the read came from the normal in-game layout (match change + balance at the right margin
    /// on one line). The first balance and any big change are only believed from that layout.
    pub fn update_read(&mut self, read: Option<i64>, now: Instant, frozen: bool, hud: bool) -> BalanceVerdict {
        if frozen {
            self.cand = None;
            return BalanceVerdict::Nothing;
        }
        let Some(b) = read else { return BalanceVerdict::Nothing };
        // a balance that gains or loses 2+ digits at once (879,244 -> 896,749,872) is two numbers read as
        // one or a stray amount: never believed, however long it stays on screen
        if let Some(cur) = self.balance {
            let digits = |v: i64| v.unsigned_abs().max(1).ilog10() as i32;
            if (digits(b) - digits(cur)).abs() >= 2 {
                return BalanceVerdict::Held(b);
            }
        }
        if self.balance == Some(b) {
            self.cand = None; // back to the known balance: a pending jump was a passing screen
            return BalanceVerdict::Nothing;
        }
        let (v, since, frames) = match self.cand {
            Some((v, since, f)) if v == b => (v, since, f + 1),
            _ => (b, now, 1),
        };
        self.cand = Some((v, since, frames));
        let big = matches!(self.balance, Some(cur) if (v - cur).abs() > self.max_jump);
        if (big || self.balance.is_none()) && !hud {
            // another screen (end of game, inventory, map): its amount never starts the clock
            self.cand = None;
            return if big { BalanceVerdict::Held(v) } else { BalanceVerdict::Nothing };
        }
        let need = match self.balance {
            None => self.first_hold,
            Some(_) if big => self.big_hold,
            Some(_) => self.min_stable,
        };
        if frames >= self.confirm_frames && now.duration_since(since) >= need {
            self.balance = Some(v);
            self.cand = None;
            return if big { BalanceVerdict::AcceptedJump(v) } else { BalanceVerdict::Accepted(v) };
        }
        if big && frames == self.confirm_frames {
            return BalanceVerdict::Held(v);
        }
        BalanceVerdict::Nothing
    }
}

/// Downs, with the map in mind: opening the map (or any full-screen menu) while down hides
/// "DAMAGE LOG". When it comes back within `same_down_within`, it is the SAME down, not a new one.
/// `quiet()` says when the money boxes should not be trusted (down, or just after): reward lines are
/// still counted meanwhile (an assist can land while you are down).
pub struct DownTracker {
    votes: (bool, u32),
    last_seen: Option<Instant>,
    fresh: bool,
    pub down: bool,
    same_down_within: Duration,
}

#[derive(Debug, PartialEq)]
pub enum DownEvent {
    /// a new down: count it
    NewDown,
    /// DAMAGE LOG is back after the map was open: same down, not counted
    SameDown,
    /// back up (revived, or dead and gone to the deploy screen)
    Up,
}

impl DownTracker {
    pub fn new(same_down_within: Duration) -> Self {
        Self { votes: (false, 0), last_seen: None, fresh: false, down: false, same_down_within }
    }

    /// One read of the downed area (about once a second).
    pub fn read(&mut self, seen: bool, now: Instant) -> Option<DownEvent> {
        if seen {
            if self.last_seen.map_or(true, |t| now.duration_since(t) > self.same_down_within) {
                self.fresh = true;
            }
            self.last_seen = Some(now);
        }
        self.votes = if seen == self.votes.0 { (seen, self.votes.1 + 1) } else { (seen, 1) };
        if self.votes.1 < 2 || seen == self.down {
            return None;
        }
        self.down = seen;
        if !seen {
            return Some(DownEvent::Up);
        }
        let ev = if self.fresh { DownEvent::NewDown } else { DownEvent::SameDown };
        self.fresh = false;
        Some(ev)
    }

    /// Down, or DAMAGE LOG seen recently (the map may be hiding it).
    pub fn quiet(&self, now: Instant) -> bool {
        self.down || self.last_seen.map_or(false, |t| now.duration_since(t) <= self.same_down_within)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kill() -> Reward {
        Reward { reason: "KILL".into(), known: true, amount: None, xp: Some(250) }
    }
    fn zone() -> Reward {
        Reward { reason: "CONTROL ZONE PRESENCE".into(), known: true, amount: Some(150), xp: None }
    }

    #[test]
    fn a_line_on_screen_counts_once() {
        let mut t = Tracker::new(2, Duration::from_millis(1500));
        let t0 = Instant::now();
        let ms = |m| t0 + Duration::from_millis(m);
        assert!(t.update(&[kill()], ms(0)).is_empty()); // not confirmed yet
        assert_eq!(t.update(&[kill()], ms(300)).len(), 1);
        for i in 2..10 {
            assert!(t.update(&[kill()], ms(300 * i)).is_empty());
        }
    }

    #[test]
    fn a_second_identical_line_counts() {
        let mut t = Tracker::new(2, Duration::from_millis(1500));
        let t0 = Instant::now();
        let ms = |m| t0 + Duration::from_millis(m);
        t.update(&[kill()], ms(0));
        assert_eq!(t.update(&[kill()], ms(300)).len(), 1);
        t.update(&[kill(), kill(), zone()], ms(600));
        let fresh = t.update(&[kill(), kill(), zone()], ms(900));
        assert_eq!(fresh.len(), 2); // one more kill + the zone line
    }

    #[test]
    fn a_single_misread_frame_does_not_count() {
        let mut t = Tracker::new(2, Duration::from_millis(1500));
        let t0 = Instant::now();
        assert!(t.update(&[zone()], t0).is_empty());
        assert!(t.update(&[], t0 + Duration::from_millis(300)).is_empty());
        // gone for longer than forget_after: a later line is a new reward
        let later = t0 + Duration::from_secs(5);
        t.update(&[zone()], later);
        assert_eq!(t.update(&[zone()], later + Duration::from_millis(300)).len(), 1);
    }

    #[test]
    fn same_line_again_after_it_left_counts_again() {
        let mut t = Tracker::new(2, Duration::from_millis(1500));
        let t0 = Instant::now();
        let ms = |m| t0 + Duration::from_millis(m);
        t.update(&[kill()], ms(0));
        assert_eq!(t.update(&[kill()], ms(300)).len(), 1);
        t.update(&[], ms(600));
        t.update(&[kill()], ms(4000));
        assert_eq!(t.update(&[kill()], ms(4300)).len(), 1);
    }

    #[test]
    fn map_during_a_down_is_the_same_down() {
        let mut d = DownTracker::new(Duration::from_secs(45));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert_eq!(d.read(true, at(0)), None);
        assert_eq!(d.read(true, at(1)), Some(DownEvent::NewDown));
        d.read(true, at(2));
        // map open for 20 s
        assert_eq!(d.read(false, at(3)), None);
        assert_eq!(d.read(false, at(4)), Some(DownEvent::Up));
        assert!(d.quiet(at(20))); // money boxes not trusted meanwhile
        // map closed: same down
        d.read(true, at(23));
        assert_eq!(d.read(true, at(24)), Some(DownEvent::SameDown));
        // revived, playing for a minute, downed again: new down
        d.read(false, at(25));
        assert_eq!(d.read(false, at(26)), Some(DownEvent::Up));
        assert!(!d.quiet(at(80)));
        d.read(true, at(90));
        assert_eq!(d.read(true, at(91)), Some(DownEvent::NewDown));
    }

    #[test]
    fn death_screen_amount_is_ignored() {
        let mut f = BalanceFilter::new(2, 20_000, Duration::from_secs(60));
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_millis(s * 300);
        // first balance: needs 2 s of the same reads
        for i in 0..12 {
            f.update(Some(-8000), at(i), false);
        }
        assert_eq!(f.balance, Some(-8000));
        // death screen shows the account total for 20 s: never accepted
        for i in 12..75 {
            assert!(!matches!(f.update(Some(152_300), at(i), false), BalanceVerdict::Accepted(_)));
        }
        // back in game, a normal change is accepted once stable for ~1 s
        let mut got = None;
        for i in 75..82 {
            if let BalanceVerdict::Accepted(v) = f.update(Some(-7850), at(i), false) {
                got = Some((v, i));
                break;
            }
        }
        assert_eq!(got.map(|g| g.0), Some(-7850));
        assert!(got.unwrap().1 <= 79);
    }

    /// Two hours of play at 3 reads/s with every kind of bad read seen so far, including screens that
    /// show a STEADY wrong amount for up to 5 minutes (end of game) and the counter rolling through an
    /// in-between value for 3 frames. Only values that really were the balance may ever be accepted.
    #[test]
    fn no_strange_jump_under_noise() {
        let mut rng: u64 = 0x9E3779B97F4A7C15;
        let mut rand = move |n: u64| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng % n
        };
        let mut f = BalanceFilter::new(3, 20_000, Duration::from_secs(60));
        let t0 = Instant::now();
        let mut truth: i64 = 877_511;
        let mut rolling: Option<(i64, u64)> = None; // in-between value shown until frame
        let mut screen: Option<(Option<i64>, u64)> = None; // steady wrong screen until frame
        let mut truths = std::collections::HashSet::from([truth]);
        let mut accepted: Vec<i64> = Vec::new();
        let mut max_true_step = 0i64;
        for i in 0..(2 * 3600 * 3) {
            let now = t0 + Duration::from_millis(i * 333);
            if rand(60) == 0 && screen.is_none() {
                let before = truth;
                truth += rand(12_001) as i64 - 6_000;
                max_true_step = max_true_step.max((truth - before).abs());
                truths.insert(truth);
                rolling = Some((before + (truth - before) / 2, i + 3));
            }
            if screen.is_none() && rand(1200) == 0 {
                let v = match rand(4) {
                    0 => Some(truth * 1000 + 143), // merged with the "143" box
                    1 => Some(1_764_020),          // another total (end of game)
                    2 => Some(200),                // stray "$200" from the map
                    _ => None,                     // HUD hidden
                };
                screen = Some((v, i + 90 + rand(810))); // 30 s to 5 min
            }
            let mut hud = true;
            let read = if let Some((v, _)) = screen.filter(|(_, u)| i < *u) {
                hud = false; // another layout
                v
            } else if let Some((v, _)) = rolling.filter(|(_, u)| i < *u) {
                Some(v)
            } else {
                screen = None;
                match rand(20) {
                    0 => Some(truth / 10),                                               // dropped digit
                    1 => Some(truth + 10i64.pow(rand(6) as u32) * (1 + rand(8) as i64)), // one misread frame
                    2 => Some(truth * 1000 + 143),                                       // merged "143"
                    3 => None,
                    _ => Some(truth),
                }
            };
            if let BalanceVerdict::Accepted(v) | BalanceVerdict::AcceptedJump(v) = f.update_read(read, now, false, hud) {
                assert!(truths.contains(&v), "accepted {v}, never the balance");
                accepted.push(v);
            }
        }
        assert!(accepted.len() > 100, "the real changes still get through ({})", accepted.len());
        for w in accepted.windows(2) {
            assert!((w[1] - w[0]).abs() <= 3 * max_true_step, "jump {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn another_screen_never_starts_the_clock() {
        let mut f = BalanceFilter::new(3, 20_000, Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..12 {
            f.update_read(Some(879_244), t0 + Duration::from_millis(i * 300), false, true);
        }
        // 10 minutes of an end-of-game screen showing another total: never accepted
        for i in 0..2000 {
            let v = f.update_read(Some(1_764_020), t0 + Duration::from_secs(4) + Duration::from_millis(i * 300), false, false);
            assert!(!matches!(v, BalanceVerdict::Accepted(_) | BalanceVerdict::AcceptedJump(_)));
        }
        // and the first balance is never taken from another screen either
        let mut g = BalanceFilter::new(3, 20_000, Duration::from_secs(60));
        for i in 0..100 {
            g.update_read(Some(1_764_020), t0 + Duration::from_millis(i * 300), false, false);
        }
        assert_eq!(g.balance, None);
    }

    #[test]
    fn end_of_game_screen_never_gets_in() {
        let mut f = BalanceFilter::new(2, 20_000, Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..12 {
            f.update(Some(879_244), t0 + Duration::from_millis(i * 300), false);
        }
        // stays for 5 minutes: still refused
        for i in 0..1000 {
            let v = f.update(Some(896_749_872), t0 + Duration::from_secs(3) + Duration::from_millis(i * 300), false);
            assert!(!matches!(v, BalanceVerdict::Accepted(_)));
        }
        assert_eq!(f.balance, Some(879_244));
    }

    #[test]
    fn frozen_while_downed_and_big_jump_after_a_minute() {
        let mut f = BalanceFilter::new(2, 20_000, Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..12 {
            f.update(Some(1000), t0 + Duration::from_millis(i * 300), false);
        }
        assert_eq!(f.update(Some(90_000), t0 + Duration::from_secs(5), true), BalanceVerdict::Nothing);
        // a real big change that stays: accepted after big_hold
        let s = t0 + Duration::from_secs(10);
        f.update(Some(90_000), s, false);
        assert_eq!(f.update(Some(90_000), s + Duration::from_secs(1), false), BalanceVerdict::Held(90_000));
        assert_eq!(f.update(Some(90_000), s + Duration::from_secs(61), false), BalanceVerdict::AcceptedJump(90_000));
    }

}
