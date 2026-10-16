//! Time to kill: how long the whole fight against a target should last, from its remaining HP and the rate the
//! players take it off (as A2Tools estimates it).
//!
//! The rate comes from the target's HP, not the meter's rows, so damage the meter does not attribute counts too. It
//! is the HP lost over the last `WINDOW_MS` (the whole fight until that much has passed), smoothed so a burst does
//! not cut the estimate short: a faster rate is taken in over `RISE_TAU_MS`, a slower one over `FALL_TAU_MS`.

const WINDOW_MS: f64 = 20_000.0;
const RISE_TAU_MS: f64 = 30_000.0;
const FALL_TAU_MS: f64 = 10_000.0;
const MIN_ELAPSED_MS: f64 = 5_000.0;
const MAX_TOTAL_MS: f64 = 99.0 * 60_000.0;

#[derive(Clone, Copy)]
struct Sample {
    t: f64,
    hp: f64,
}

#[derive(Default)]
pub struct TtkEstimator {
    target: Option<i32>,
    samples: Vec<Sample>,
    rate: Option<f64>,
    last_t: Option<f64>,
}

impl TtkEstimator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self, target: Option<i32>) {
        *self = TtkEstimator { target, ..Default::default() };
    }

    /// `target`: the entity; `fight_ms`: time since the fight started; `remaining`, `max`: its HP. Returns the
    /// predicted length of the whole fight (ms).
    pub fn update(&mut self, target: i32, fight_ms: f64, remaining: i64, max: i64) -> Option<f64> {
        let (hp, max_hp) = (remaining as f64, max as f64);
        if max_hp <= 0.0 || hp < 0.0 || !fight_ms.is_finite() {
            return None;
        }
        // Another target, or the timer went back: a new fight.
        if self.target != Some(target) || self.last_t.is_some_and(|l| fight_ms < l) {
            self.reset(Some(target));
        }
        if hp <= 0.0 {
            return None;
        }

        match self.samples.last_mut() {
            // HP went up (a heal, a phase): measure from here.
            Some(last) if hp > last.hp => {
                self.samples.clear();
                self.samples.push(Sample { t: fight_ms, hp });
            }
            Some(last) if last.t == fight_ms => last.hp = hp,
            _ => self.samples.push(Sample { t: fight_ms, hp }),
        }
        // Keep one sample at or before the window's start.
        while self.samples.len() > 2 && self.samples[1].t <= fight_ms - WINDOW_MS {
            self.samples.remove(0);
        }

        if fight_ms < MIN_ELAPSED_MS {
            self.last_t = Some(fight_ms);
            return None;
        }
        let first = self.samples[0];
        let span = fight_ms - first.t;
        let fight_average = (max_hp - hp) / fight_ms;
        let observed = if span >= WINDOW_MS / 2.0 { (first.hp - hp) / span } else { fight_average };

        match (self.rate, self.last_t) {
            (None, _) => {
                let seed = if fight_average > 0.0 { fight_average } else { observed };
                if seed > 0.0 {
                    self.rate = Some(seed);
                }
            }
            (Some(rate), Some(last_t)) if observed >= 0.0 && fight_ms > last_t => {
                let tau = if observed > rate { RISE_TAU_MS } else { FALL_TAU_MS };
                self.rate = Some(rate + (1.0 - (-(fight_ms - last_t) / tau).exp()) * (observed - rate));
            }
            _ => {}
        }
        self.last_t = Some(fight_ms);
        let rate = self.rate.filter(|r| *r > 0.0)?;
        let total = fight_ms + hp / rate;
        (total <= MAX_TOTAL_MS).then_some(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steady_damage_predicts_the_fight_length() {
        let mut ttk = TtkEstimator::new();
        let max = 1_000_000;
        // 10,000 HP per second: a 100 s fight.
        let mut last = None;
        for s in 0..=30 {
            last = ttk.update(7, s as f64 * 1000.0, max - s * 10_000, max);
        }
        let total = last.unwrap();
        assert!((total - 100_000.0).abs() < 1.0, "{total}");
    }

    #[test]
    fn nothing_before_five_seconds() {
        let mut ttk = TtkEstimator::new();
        assert_eq!(ttk.update(7, 4000.0, 900, 1000), None);
    }

    #[test]
    fn another_target_starts_over() {
        let mut ttk = TtkEstimator::new();
        for s in 0..=10 {
            ttk.update(7, s as f64 * 1000.0, 1000 - s * 10, 1000);
        }
        assert_eq!(ttk.update(8, 11_000.0, 1000, 1000), None);
        assert_eq!(ttk.target, Some(8));
    }
}
