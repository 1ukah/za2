//! Effects (buffs and debuffs) over time: the spans between an effect going on an entity and coming off.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::catalog::{buffs, skills};
use crate::decoder::{BuffChange, BuffEvent};
use crate::segment::{Micros, SECOND};

/// An effect whose server end passed this long ago without a removal record is taken as gone.
const OVERDUE: Micros = 2 * SECOND;
/// The timeline keeps at most this many spans; the oldest closed ones go first.
const MAX_SPANS: usize = 50_000;

#[derive(Clone, Debug, PartialEq)]
pub struct BuffSpan {
    pub target: i32,
    pub caster: i32,
    pub abnormal: u32,
    pub skill: Option<u32>,
    pub start: Micros,
    /// None while the effect is on.
    pub end: Option<Micros>,
    /// Endless (a passive or a toggle).
    pub endless: bool,
    /// When the server said it ends (unix ms).
    server_end_ms: i64,
}

#[derive(Default)]
pub struct BuffTimeline {
    spans: Vec<BuffSpan>,
    open: HashMap<(i32, i64), usize>,
    last_sweep: Micros,
}

/// How long an effect was on a target in a time window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuffUptime {
    pub abnormal: u32,
    pub skill: Option<u32>,
    pub name: String,
    /// The effect was on its target for this share of the window (0..1).
    pub uptime: f64,
    /// How often it was put on in the window.
    pub applications: u32,
    /// The spans it was on, in seconds from the window start, overlapping ones merged.
    pub spans: Vec<(f64, f64)>,
    /// Cast by the target itself (else by someone else).
    pub own: bool,
}

/// The name of an effect: its own, else the name of the skill that put it on.
pub fn name_of(abnormal: u32, skill: Option<u32>) -> String {
    if let Some(name) = buffs::name_of(abnormal).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    let skill = skill.map_or(abnormal as i32 / 10, |s| s as i32);
    skills::english_name(skill).map(str::to_string).unwrap_or_else(|| format!("Effect {abnormal}"))
}

impl BuffTimeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn note(&mut self, e: &BuffEvent) {
        let key = (e.target, e.instance);
        match e.change {
            BuffChange::On => {
                self.close(key, e.timestamp);
                self.open_span(key, e, e.timestamp);
            }
            BuffChange::Changed { .. } => {
                let same = self.open.get(&key).map(|&i| &self.spans[i]).is_some_and(|s| s.abnormal == e.abnormal && s.caster == e.caster);
                if same {
                    let i = self.open[&key];
                    self.spans[i].server_end_ms = e.end_ms;
                } else {
                    self.close(key, e.timestamp);
                    // first seen changing: it went on when its duration started, if that was before
                    let since = if e.length_ms > 0 { ((e.end_ms - e.length_ms) * 1000).clamp(e.timestamp - e.length_ms * 1000, e.timestamp) } else { e.timestamp };
                    self.open_span(key, e, since);
                }
            }
            BuffChange::Off { .. } => self.close(key, e.timestamp),
        }
    }

    fn open_span(&mut self, key: (i32, i64), e: &BuffEvent, start: Micros) {
        if self.spans.len() >= MAX_SPANS {
            self.compact();
        }
        self.spans.push(BuffSpan {
            target: e.target,
            caster: e.caster,
            abnormal: e.abnormal,
            skill: e.skill,
            start,
            end: None,
            endless: e.length_ms < 0,
            server_end_ms: e.end_ms,
        });
        self.open.insert(key, self.spans.len() - 1);
    }

    fn close(&mut self, key: (i32, i64), at: Micros) {
        if let Some(i) = self.open.remove(&key) {
            let span = &mut self.spans[i];
            span.end = Some(at.max(span.start));
        }
    }

    fn close_where(&mut self, at: Micros, mut done: impl FnMut(&BuffSpan) -> bool) {
        let keys: Vec<(i32, i64)> = self.open.iter().filter(|(_, &i)| done(&self.spans[i])).map(|(k, _)| *k).collect();
        for key in keys {
            self.close(key, at);
        }
    }

    /// The entity died or left: its effects end.
    pub fn depart(&mut self, entity: i32, at: Micros) {
        self.close_where(at, |s| s.target == entity);
    }

    /// A new map: everything around is gone, the own character's effects carry on.
    pub fn zone_change(&mut self, local: Option<i32>, at: Micros) {
        self.close_where(at, |s| Some(s.target) != local);
    }

    /// Ends effects whose time ran out without a removal record; at most once a second.
    pub fn sweep(&mut self, now: Micros) {
        if now - self.last_sweep < SECOND {
            return;
        }
        self.last_sweep = now;
        self.close_where(now, |s| !s.endless && s.server_end_ms > 0 && s.server_end_ms * 1000 + OVERDUE < now);
        // closed at the sweep, but they ended when the server said
        for span in &mut self.spans {
            if let Some(end) = span.end {
                if end == now && !span.endless && span.server_end_ms > 0 {
                    span.end = Some((span.server_end_ms * 1000).clamp(span.start, now));
                }
            }
        }
    }

    /// The entity's combat id changed (a new map): its spans follow it.
    pub fn merge_ids(&mut self, from: i32, to: i32) {
        for span in &mut self.spans {
            if span.target == from {
                span.target = to;
            }
            if span.caster == from {
                span.caster = to;
            }
        }
        let keys: Vec<(i32, i64)> = self.open.keys().filter(|k| k.0 == from).copied().collect();
        for key in keys {
            let i = self.open.remove(&key).unwrap();
            self.open.insert((to, key.1), i);
        }
    }

    /// Drops the closed spans that ended before `before`.
    pub fn forget_before(&mut self, before: Micros) {
        if self.spans.iter().any(|s| s.end.is_some_and(|e| e < before)) {
            self.retain(|s| s.end.is_none_or(|e| e >= before));
        }
    }

    fn compact(&mut self) {
        let mut ends: Vec<Micros> = self.spans.iter().filter_map(|s| s.end).collect();
        ends.sort_unstable();
        if let Some(&cut) = ends.get(ends.len() / 2) {
            self.retain(|s| s.end.is_none_or(|e| e > cut));
        }
    }

    fn retain(&mut self, keep: impl Fn(&BuffSpan) -> bool) {
        let open: HashMap<usize, (i32, i64)> = self.open.iter().map(|(k, &i)| (i, *k)).collect();
        let mut spans = Vec::with_capacity(self.spans.len());
        self.open.clear();
        for (i, span) in std::mem::take(&mut self.spans).into_iter().enumerate() {
            if keep(&span) {
                if let Some(key) = open.get(&i) {
                    self.open.insert(*key, spans.len());
                }
                spans.push(span);
            }
        }
        self.spans = spans;
    }

    pub fn spans(&self) -> &[BuffSpan] {
        &self.spans
    }

    /// The effects that were on `target` between `from` and `to` (timed ones; endless effects are left out), most uptime first.
    pub fn uptimes(&self, target: i32, from: Micros, to: Micros) -> Vec<BuffUptime> {
        let window = (to - from).max(1) as f64;
        let mut by_effect: HashMap<u32, (Option<u32>, u32, bool, Vec<(Micros, Micros)>)> = HashMap::new();
        for s in self.spans.iter().filter(|s| s.target == target && !s.endless) {
            let (a, b) = (s.start.max(from), s.end.unwrap_or(to).min(to));
            if b <= a && !(s.start >= from && s.start <= to) {
                continue;
            }
            let entry = by_effect.entry(s.abnormal).or_insert((s.skill, 0, false, Vec::new()));
            entry.0 = entry.0.or(s.skill);
            if s.start >= from {
                entry.1 += 1;
            }
            entry.2 |= s.caster == target;
            if b > a {
                entry.3.push((a, b));
            }
        }
        let mut out: Vec<BuffUptime> = by_effect
            .into_iter()
            .map(|(abnormal, (skill, applications, own, mut parts))| {
                parts.sort_unstable();
                let mut merged: Vec<(Micros, Micros)> = Vec::new();
                for (a, b) in parts {
                    match merged.last_mut() {
                        Some(last) if a <= last.1 => last.1 = last.1.max(b),
                        _ => merged.push((a, b)),
                    }
                }
                let on: Micros = merged.iter().map(|(a, b)| b - a).sum();
                BuffUptime {
                    abnormal,
                    skill,
                    name: name_of(abnormal, skill),
                    uptime: (on as f64 / window).min(1.0),
                    applications,
                    spans: merged.iter().map(|(a, b)| ((a - from) as f64 / 1e6, (b - from) as f64 / 1e6)).collect(),
                    own,
                }
            })
            .collect();
        // the game's own test effects (one goes on at every landing after a jump)
        out.retain(|b| !b.name.starts_with("For Testing"));
        out.sort_by(|a, b| b.uptime.total_cmp(&a.uptime).then(a.name.cmp(&b.name)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(t: Micros, instance: i64, abnormal: u32, change: BuffChange) -> BuffEvent {
        BuffEvent { timestamp: t * SECOND, target: 1, instance, abnormal, caster: 1, skill: None, length_ms: 10_000, end_ms: (t + 10) * 1000, change }
    }

    #[test]
    fn uptime_merges_stacks_and_clips_to_the_window() {
        let mut tl = BuffTimeline::new();
        tl.note(&event(0, 1, 500, BuffChange::On));
        tl.note(&event(5, 2, 500, BuffChange::On));
        tl.note(&event(10, 1, 0, BuffChange::Off { reason: 1 }));
        tl.note(&event(15, 2, 0, BuffChange::Off { reason: 1 }));
        let up = tl.uptimes(1, 0, 20 * SECOND);
        assert_eq!(up.len(), 1);
        assert_eq!(up[0].applications, 2);
        assert_eq!(up[0].spans, vec![(0.0, 15.0)]);
        assert!((up[0].uptime - 0.75).abs() < 1e-9);
    }

    #[test]
    fn effects_end_on_death_and_when_overdue() {
        let mut tl = BuffTimeline::new();
        tl.note(&event(0, 1, 500, BuffChange::On));
        tl.note(&event(0, 2, 600, BuffChange::On));
        tl.depart(1, 4 * SECOND);
        assert!(tl.spans().iter().all(|s| s.end == Some(4 * SECOND)));

        let mut tl = BuffTimeline::new();
        tl.note(&event(0, 1, 500, BuffChange::On));
        tl.sweep(11 * SECOND);
        assert_eq!(tl.spans()[0].end, None);
        tl.sweep(13 * SECOND);
        assert_eq!(tl.spans()[0].end, Some(10 * SECOND));
    }

    #[test]
    fn spans_survive_compaction() {
        let mut tl = BuffTimeline::new();
        tl.note(&event(0, 1, 500, BuffChange::On));
        tl.note(&event(1, 2, 600, BuffChange::On));
        tl.note(&event(2, 2, 0, BuffChange::Off { reason: 1 }));
        tl.forget_before(3 * SECOND);
        assert_eq!(tl.spans().len(), 1);
        tl.note(&event(4, 1, 0, BuffChange::Off { reason: 1 }));
        assert_eq!(tl.spans()[0].end, Some(4 * SECOND));
    }
}
