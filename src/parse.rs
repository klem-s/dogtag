//! Turns OCR'd lines of the money corner into readings: the balance, and (near the end of a round)
//! the rank/level badge and the team scores.
use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    /// the balance box at the HUD's right margin ($877,511) - never negative
    pub balance: Option<i64>,
    /// the match-change box left of it (-$13,393) - only used internally to tell the balance box
    /// apart from it; nothing downstream tracks the match change itself anymore.
    pub match_change: Option<i64>,
    /// both boxes read on one line in the normal in-game layout: the only reads trusted to start a
    /// balance or to move it a lot
    pub hud_layout: bool,
}

fn money_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // "$1,000" "+$150" "- $3.009"; groups only by comma/dot, so a box next to it ("143") never joins;
    // digits may come back as O/S/I/l
    R.get_or_init(|| Regex::new(r"([+\-–—~])?\s*\$\s*([0-9OoSsIl]{1,3}(?:[,.][0-9OoSsIl]{3})+|[0-9OoSsIl]+)").unwrap())
}

/// Fixes the letters OCR likes to put inside numbers and drops separators.
fn digits(s: &str) -> Option<i64> {
    let cleaned: String = s
        .chars()
        .filter_map(|c| match c {
            '0'..='9' => Some(c),
            'O' | 'o' => Some('0'),
            'S' | 's' => Some('5'),
            'I' | 'l' => Some('1'),
            _ => None,
        })
        .collect();
    if cleaned.is_empty() || cleaned.len() > 9 {
        return None;
    }
    cleaned.parse().ok()
}

/// End-of-round panel: one score per team, top to bottom (each sits in its own small box next to a
/// colored bar, so one OCR'd line = one team - no need to disambiguate by position/color here).
pub fn team_scores(lines: &[String]) -> Vec<i64> {
    lines.iter().filter_map(|l| digits(l)).collect()
}

/// One OCR'd line -> a money line (the balance and/or match change) or nothing.
pub enum Line {
    Money,
    Noise,
}

pub fn parse_line(text: &str) -> Line {
    let t = text.trim();
    if t.len() < 2 || money_re().find(t).is_none() {
        return Line::Noise;
    }
    Line::Money
}

/// The balance from a region that only shows the balance (maybe with a label or a currency icon):
/// the last amount read, negative with a leading minus.
pub fn balance_in<S: AsRef<str>>(lines: &[S]) -> Option<i64> {
    let text = lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>().join(" ");
    let m = money_re().captures_iter(&text).last()?;
    let v = digits(m.get(2)?.as_str())?;
    let neg = matches!(m.get(1).map(|s| s.as_str()), Some("-" | "–" | "—" | "~"));
    Some(if neg { -v } else { v })
}

/// Right edge (fraction of the region width) past which an amount is the balance box. Measured on a
/// 2560x1440 frame: balance box ends at ~0.94, match change at ~0.69.
pub const BALANCE_X: f32 = 0.82;

/// The amounts of a money-only line: (balance, match change).
/// With positions: the amount ending near the right margin is the balance, one left of it the change.
/// Without: two amounts = change then balance; one signed amount = change; one unsigned = balance.
/// The third value is true when the balance was found against the right margin (normal HUD).
pub fn money_boxes(text: &str, xs: Option<&[f32]>) -> (Option<i64>, Option<i64>, bool) {
    struct Tok {
        v: i64,
        signed: bool,
        x: Option<f32>,
    }
    let byte_to_char: Vec<usize> = {
        let mut m = vec![0; text.len() + 1];
        for (ci, (bi, _)) in text.char_indices().enumerate() {
            m[bi] = ci;
        }
        m[text.len()] = text.chars().count();
        m
    };
    let toks: Vec<Tok> = money_re()
        .captures_iter(text)
        .filter_map(|c| {
            let v = digits(c.get(2)?.as_str().trim_end_matches([',', '.', ' ']))?;
            let sign = c.get(1).map(|m| m.as_str());
            let neg = matches!(sign, Some("-" | "–" | "—" | "~"));
            let end_char = byte_to_char[c.get(0)?.as_str().trim_end().len() + c.get(0)?.start()];
            let x = xs.and_then(|xs| xs.get(end_char.saturating_sub(1)).copied());
            Some(Tok { v: if neg { -v } else { v }, signed: sign.is_some(), x })
        })
        .collect();
    if toks.is_empty() {
        return (None, None, false);
    }
    if toks.iter().all(|t| t.x.is_some()) {
        let mut bal = toks.iter().rev().find(|t| t.x.unwrap() >= BALANCE_X && !t.signed).map(|t| t.v);
        let at_margin = bal.is_some();
        let chg = toks.iter().find(|t| t.x.unwrap() < BALANCE_X).map(|t| t.v);
        // inventory / scoreboard screens move the boxes left (a third box, the "143" without $, sits at the
        // margin): a signed amount followed by an unsigned one is still change + balance
        if bal.is_none() && toks.len() >= 2 && toks[0].signed {
            bal = toks.iter().skip(1).rev().find(|t| !t.signed).map(|t| t.v);
        }
        return (bal, chg, at_margin);
    }
    if toks.len() >= 2 {
        let last = toks.last().unwrap();
        return ((!last.signed).then_some(last.v), Some(toks[0].v), true);
    }
    let t = &toks[0];
    if t.signed { (None, Some(t.v), false) } else { (Some(t.v), None, true) }
}

/// The rank/level badge next to the balance on the end-of-round layout ("$967,270147" - no separator,
/// tight kerning glues it to the balance): purely additive, doesn't touch the balance itself (money_re
/// already stops at the last full group of 3 digits, so the balance amount is never affected by this).
/// Only the LAST money match is checked, and only 2-4 trailing digits directly glued to it (no space,
/// no separator) count - anything else looks nothing like this specific layout.
pub fn rank_in(text: &str) -> Option<i64> {
    let m = money_re().find_iter(text).last()?;
    let rest = &text[m.end()..];
    let n = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if !(2..=4).contains(&n) {
        return None;
    }
    // must not continue into more digits/letters that OCR could still consider part of the same token
    if rest.chars().nth(n).is_some_and(|c| c.is_alphanumeric()) {
        return None;
    }
    digits(&rest[..n])
}

#[cfg(test)]
pub fn parse_lines<S: AsRef<str>>(lines: &[S]) -> Reading {
    let pl: Vec<(String, Vec<f32>)> = lines.iter().map(|l| (l.as_ref().to_string(), Vec::new())).collect();
    parse_positioned(&pl)
}

/// Lines with the right edge of each character (empty = no positions).
pub fn parse_positioned(lines: &[(String, Vec<f32>)]) -> Reading {
    let mut r = Reading::default();
    for (text, xs) in lines {
        if matches!(parse_line(text), Line::Noise) {
            continue;
        }
        let xs = (xs.len() == text.chars().count()).then_some(xs.as_slice());
        let (b, c, at_margin) = money_boxes(text, xs);
        // normal HUD: change + balance on one line, balance against the right margin
        if b.is_some() && c.is_some() && at_margin && r.balance.is_none() {
            r.hud_layout = true;
        }
        if r.balance.is_none() {
            r.balance = b;
        }
        if r.match_change.is_none() {
            r.match_change = c;
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_line() {
        // the real HUD, 2560x1440: "-$13,393" (match change) then "$877,511" (balance)
        let r = parse_lines(&["-$13,393 $877,511"]);
        assert_eq!((r.balance, r.match_change), (Some(877_511), Some(-13_393)));
        assert!(r.hud_layout);
        assert!(!parse_lines(&["$48,250"]).hud_layout);
        assert_eq!(parse_lines(&["$48,250"]).balance, Some(48250));
        // a lone signed amount is the match change, never the balance
        let r = parse_lines(&["-$8,000"]);
        assert_eq!((r.balance, r.match_change), (None, Some(-8000)));
        let r = parse_lines(&["+$1,000"]);
        assert_eq!((r.balance, r.match_change), (None, Some(1000)));
    }

    #[test]
    fn rank_glued_to_balance() {
        // end-of-round layout: no separator between the balance and the rank badge - money_re still
        // stops at the last full group of 3 digits, so the balance itself is unaffected.
        assert_eq!(rank_in("$967,270147"), Some(147));
        assert_eq!(money_boxes("$967,270147", None).0, Some(967_270));
        // normal HUD line: nothing glued on, no false positive
        assert_eq!(rank_in("-$13,393 $877,511"), None);
        assert_eq!(rank_in("$48,250"), None);
        // only 2-4 digits count; a longer run looks like OCR noise, not a rank badge
        assert_eq!(rank_in("$1,015,55612345"), None);
        assert_eq!(rank_in("$1,0155"), None); // only 1 digit past the group - not a badge either
    }

    #[test]
    fn positions_decide_which_box() {
        // only the change box read, unsigned (sign lost): its right edge is at 0.69, so not the balance
        let text = "$13,393";
        let n = text.chars().count();
        let xs: Vec<f32> = (0..n).map(|i| 0.50 + 0.19 * (i + 1) as f32 / n as f32).collect();
        let r = parse_positioned(&[(text.to_string(), xs)]);
        assert_eq!((r.balance, r.match_change), (None, Some(13393)));
        // the balance alone, ending at 0.94
        let text = "$877,511";
        let n = text.chars().count();
        let xs: Vec<f32> = (0..n).map(|i| 0.76 + 0.18 * (i + 1) as f32 / n as f32).collect();
        let r = parse_positioned(&[(text.to_string(), xs)]);
        assert_eq!((r.balance, r.match_change), (Some(877_511), None));
    }

    #[test]
    fn inventory_screen_layout() {
        // 2560x1440 inventory: "-$19,040" | "$879,244" | "143" boxes, all shifted left
        let text = "-$19,040 $879,244 143";
        let xs: Vec<f32> = text
            .chars()
            .enumerate()
            .map(|(i, _)| match i {
                0..=8 => 0.21 + 0.19 * i as f32 / 8.0,
                9..=17 => 0.53 + 0.14 * (i - 9) as f32 / 8.0,
                _ => 0.85 + 0.04 * (i - 18) as f32 / 2.0,
            })
            .collect();
        let r = parse_positioned(&[(text.to_string(), xs)]);
        assert_eq!((r.balance, r.match_change), (Some(879_244), Some(-19_040)));
        assert!(!r.hud_layout, "the inventory is not the in-game HUD");
    }

    #[test]
    fn balance_region() {
        assert_eq!(balance_in(&["BALANCE -$8,000"]), Some(-8000));
        assert_eq!(balance_in(&["WALLET", "$ 12.450"]), Some(12450));
        assert_eq!(balance_in(&["nothing"]), None);
    }

    #[test]
    fn noise() {
        assert!(matches!(parse_line("x"), Line::Noise));
        assert!(matches!(parse_line("SOMETHING ELSE"), Line::Noise));
    }
}
