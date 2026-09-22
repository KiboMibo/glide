// Copyright The Glide Authors
// SPDX-License-Identifier: MIT OR Apache-2.0

use std::time::Instant;

/// Response of the default spring in seconds: 80% of the way in ~100 ms, 95%
/// in 150 ms.
const DEFAULT_RESPONSE: f64 = 0.2;
/// Critically damped: the fastest settle without overshoot.
const DEFAULT_DAMPING_FRACTION: f64 = 1.0;
/// The animation is complete once it is this close to the target, in points.
const POSITION_EPSILON: f64 = 0.5;
/// ...and slower than this, in points per second. Near the end of a critically
/// damped move the speed is ~15 pt/s; the bound only stops a fast pass through
/// the target (after a retarget) from counting as complete.
const VELOCITY_EPSILON: f64 = 20.0;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SpringAnimation {
    initial_value: f64,
    target_value: f64,
    initial_velocity: f64,
    start_time: Instant,
    response: f64,
    damping_fraction: f64,
    omega_n: f64,
    omega_d: f64,
    zeta: f64,
}

impl SpringAnimation {
    pub fn new(
        initial_value: f64,
        target_value: f64,
        initial_velocity: f64,
        response: f64,
        damping_fraction: f64,
        now: Instant,
    ) -> Self {
        let omega_n = 2.0 * std::f64::consts::PI / response;
        let zeta = damping_fraction;
        let omega_d = omega_n * (1.0 - zeta * zeta).max(0.0).sqrt();
        SpringAnimation {
            initial_value,
            target_value,
            initial_velocity,
            start_time: now,
            response,
            damping_fraction,
            omega_n,
            omega_d,
            zeta,
        }
    }

    pub fn with_defaults(initial_value: f64, target_value: f64, now: Instant) -> Self {
        Self::new(
            initial_value,
            target_value,
            0.0,
            DEFAULT_RESPONSE,
            DEFAULT_DAMPING_FRACTION,
            now,
        )
    }

    /// Moves the target, keeping the current value and velocity. The velocity
    /// toward the new target is capped at `omega_n * |remaining|`, the fastest a
    /// critically damped spring can approach its target without passing it.
    pub fn retarget(&mut self, new_target: f64, now: Instant) {
        let current = self.value_at(now);
        let mut vel = self.velocity_at(now);
        let remaining = new_target - current;
        let max_toward = self.omega_n * remaining.abs();
        if vel * remaining > 0.0 && vel.abs() > max_toward {
            vel = max_toward * remaining.signum();
        }
        self.initial_value = current;
        self.target_value = new_target;
        self.initial_velocity = vel;
        self.start_time = now;
    }

    pub fn value_at(&self, time: Instant) -> f64 {
        let t = time.duration_since(self.start_time).as_secs_f64();
        let x0 = self.initial_value - self.target_value;
        let v0 = self.initial_velocity;

        let displacement = if self.zeta >= 1.0 {
            let decay = (-self.omega_n * t).exp();
            decay * (x0 + (v0 + self.omega_n * x0) * t)
        } else {
            let decay = (-self.zeta * self.omega_n * t).exp();
            let cos_part = x0 * (self.omega_d * t).cos();
            let sin_part =
                ((v0 + self.zeta * self.omega_n * x0) / self.omega_d) * (self.omega_d * t).sin();
            decay * (cos_part + sin_part)
        };

        self.target_value + displacement
    }

    pub fn velocity_at(&self, time: Instant) -> f64 {
        let t = time.duration_since(self.start_time).as_secs_f64();
        let x0 = self.initial_value - self.target_value;
        let v0 = self.initial_velocity;

        if self.zeta >= 1.0 {
            let decay = (-self.omega_n * t).exp();
            let a = v0 + self.omega_n * x0;
            decay * (a - self.omega_n * (x0 + a * t))
        } else {
            let decay = (-self.zeta * self.omega_n * t).exp();
            let b = (v0 + self.zeta * self.omega_n * x0) / self.omega_d;
            let cos_t = (self.omega_d * t).cos();
            let sin_t = (self.omega_d * t).sin();
            decay
                * ((-self.zeta * self.omega_n) * (x0 * cos_t + b * sin_t)
                    + (-x0 * self.omega_d * sin_t + b * self.omega_d * cos_t))
        }
    }

    pub fn is_complete(&self, time: Instant) -> bool {
        let t = time.duration_since(self.start_time).as_secs_f64();
        if t < 0.01 {
            return false;
        }
        let val = self.value_at(time);
        let vel = self.velocity_at(time);
        (val - self.target_value).abs() < POSITION_EPSILON && vel.abs() < VELOCITY_EPSILON
    }

    pub fn target(&self) -> f64 {
        self.target_value
    }

    pub fn current(&self, now: Instant) -> f64 {
        self.value_at(now)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn critically_damped_converges() {
        let now = Instant::now();
        let spring = SpringAnimation::new(0.0, 100.0, 0.0, 0.5, 1.0, now);
        let end = spring.start_time + Duration::from_secs(2);
        let val = spring.value_at(end);
        assert!((val - 100.0).abs() < 1.0);
        assert!(spring.is_complete(end));
    }

    #[test]
    fn underdamped_oscillates() {
        let now = Instant::now();
        let spring = SpringAnimation::new(0.0, 100.0, 0.0, 0.5, 0.5, now);
        let mid = spring.start_time + Duration::from_millis(200);
        let val = spring.value_at(mid);
        assert!(val > 50.0);
    }

    #[test]
    fn retarget_preserves_continuity() {
        let now = Instant::now();
        let mut spring = SpringAnimation::new(0.0, 100.0, 0.0, 0.5, 1.0, now);
        let mid = now;
        let val_before = spring.value_at(mid);
        spring.retarget(200.0, now);
        let val_after = spring.value_at(now);
        assert!((val_before - val_after).abs() < 5.0);
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// First millisecond after `start` at which the spring reports completion.
    fn completion_time(spring: &SpringAnimation, start: Instant) -> Duration {
        (0..=2000)
            .map(ms)
            .find(|&d| spring.is_complete(start + d))
            .expect("spring should complete within 2 s")
    }

    #[test]
    fn default_spring_covers_most_of_the_path_quickly() {
        let now = Instant::now();
        let spring = SpringAnimation::with_defaults(0.0, 1000.0, now);
        let fraction = |t| spring.value_at(now + ms(t)) / 1000.0;
        assert!(fraction(50) > 0.4, "50 ms: {}", fraction(50));
        assert!(fraction(100) > 0.8, "100 ms: {}", fraction(100));
        assert!(fraction(150) > 0.9, "150 ms: {}", fraction(150));
        assert!(fraction(200) > 0.98, "200 ms: {}", fraction(200));
    }

    #[test]
    fn default_spring_completes_within_350ms_for_1000px() {
        let now = Instant::now();
        let spring = SpringAnimation::with_defaults(0.0, 1000.0, now);
        let done = completion_time(&spring, now);
        assert!(done <= ms(350), "completed after {done:?}");
        assert!((spring.value_at(now + done) - 1000.0).abs() < 0.5);
    }

    #[test]
    fn default_spring_does_not_overshoot() {
        let now = Instant::now();
        for (from, to) in [(0.0, 1000.0), (1000.0, 0.0), (0.0, 5.0)] {
            let spring = SpringAnimation::with_defaults(from, to, now);
            let mut prev = from;
            for t in 0..=1000 {
                let val = spring.value_at(now + ms(t));
                assert!(
                    (val - prev) * (to - from) >= 0.0,
                    "{from}->{to}: moved backwards at {t} ms"
                );
                assert!(
                    (to - val) * (to - from) >= 0.0,
                    "{from}->{to}: overshot at {t} ms"
                );
                prev = val;
            }
        }
    }

    #[test]
    fn retarget_keeps_velocity() {
        let now = Instant::now();
        let mut spring = SpringAnimation::with_defaults(0.0, 1000.0, now);
        let at = now + ms(60);
        let (val, vel) = (spring.value_at(at), spring.velocity_at(at));
        spring.retarget(2000.0, at);
        assert!((spring.value_at(at) - val).abs() < 1e-9);
        assert!((spring.velocity_at(at) - vel).abs() < 1e-6);
        assert!(vel > 1000.0);
    }

    #[test]
    fn retarget_mid_animation_does_not_slow_down() {
        let now = Instant::now();
        let mut spring = SpringAnimation::with_defaults(0.0, 1000.0, now);
        let at = now + ms(80);
        spring.retarget(2000.0, at);
        let done = completion_time(&spring, at);
        assert!(done <= ms(350), "completed {done:?} after the retarget");
        let mut prev = spring.value_at(at);
        for t in 0..=1000 {
            let val = spring.value_at(at + ms(t));
            assert!(
                val >= prev && val <= 2000.0,
                "non-monotonic or overshoot at {t} ms"
            );
            prev = val;
        }
    }

    /// Largest distance the spring goes past `target` in the direction of
    /// travel, sampled every millisecond for 1 s after `from`.
    fn overshoot(spring: &SpringAnimation, from: Instant, target: f64, dir: f64) -> f64 {
        (0..=1000)
            .map(|t| (spring.value_at(from + ms(t)) - target) * dir)
            .fold(0.0, f64::max)
    }

    #[test]
    fn retarget_in_the_opposite_direction_turns_around_smoothly() {
        let now = Instant::now();
        let mut spring = SpringAnimation::with_defaults(0.0, 1000.0, now);
        let at = now + ms(80);
        let turn_at = spring.value_at(at);
        spring.retarget(0.0, at);

        let mut prev = turn_at;
        let mut furthest = turn_at;
        for t in 1..=1000 {
            let val = spring.value_at(at + ms(t));
            assert!(val >= -1e-9, "went past the new target at {t} ms: {val}");
            assert!((val - prev).abs() < 30.0, "jumped {} pt at {t} ms", val - prev);
            furthest = furthest.max(val);
            prev = val;
        }
        // The spring keeps its momentum for a moment before turning around.
        assert!(
            furthest - turn_at < 0.05 * 1000.0,
            "kept going {} pt past the turn",
            furthest - turn_at
        );
        let done = completion_time(&spring, at);
        assert!(done <= ms(400), "completed {done:?} after the retarget");
    }

    #[test]
    fn quick_back_and_forth_retargets_do_not_overshoot() {
        // Right, right, left: 0 -> 960 -> 1920 -> 960. The second press comes
        // `second` ms after the first, the third `back` ms after the second.
        // On the way back the target is still ahead of the moving spring.
        let mut overshoots = vec![];
        for (second, back) in [(30, 30), (50, 15), (50, 30), (80, 15)] {
            let now = Instant::now();
            let mut spring = SpringAnimation::with_defaults(0.0, 960.0, now);
            spring.retarget(1920.0, now + ms(second));
            let at = now + ms(second + back);
            let before = spring.value_at(at);
            spring.retarget(960.0, at);
            let dir = (960.0 - before).signum();
            let over = overshoot(&spring, at, 960.0, dir);
            if over >= 1.0 {
                overshoots.push(format!("+{second}/+{back} ms: {over:.0} pt from {before:.0}"));
            }
        }
        assert!(overshoots.is_empty(), "overshot 960: {overshoots:?}");
    }

    #[test]
    fn retarget_to_a_nearer_target_ahead_approaches_as_fast_as_it_can_without_passing() {
        let now = Instant::now();
        let mut spring = SpringAnimation::with_defaults(0.0, 1920.0, now);
        let at = now + ms(40);
        let (val, vel) = (spring.value_at(at), spring.velocity_at(at));
        let target = val + 100.0;
        let fastest = spring.omega_n * 100.0;
        assert!(vel > fastest, "the cap must apply: {vel} <= {fastest}");

        spring.retarget(target, at);
        assert!((spring.value_at(at) - val).abs() < 1e-9, "position jumped");
        let capped = spring.velocity_at(at);
        assert!(
            (capped - fastest).abs() < 1e-6,
            "expected {fastest} pt/s toward the target, got {capped}"
        );
        let mut prev = val;
        for t in 1..=1000 {
            let v = spring.value_at(at + ms(t));
            assert!(
                v >= prev && v <= target,
                "non-monotonic or overshoot at {t} ms: {v}"
            );
            prev = v;
        }
        let done = completion_time(&spring, at);
        assert!(done <= ms(400), "completed {done:?} after the retarget");
    }

    #[test]
    fn retarget_keeps_velocity_unless_it_would_pass_the_target() {
        let now = Instant::now();
        for retarget_after in (10..=300).step_by(10) {
            for new_target in [-960.0, 0.0, 1500.0, 1920.0, 5000.0] {
                let mut spring = SpringAnimation::with_defaults(0.0, 960.0, now);
                let at = now + ms(retarget_after);
                let (val, vel) = (spring.value_at(at), spring.velocity_at(at));
                spring.retarget(new_target, at);
                let remaining: f64 = new_target - val;
                let label = format!("+{retarget_after} ms -> {new_target}");
                assert!(
                    (spring.value_at(at) - val).abs() < 1e-9,
                    "{label}: position jumped"
                );
                let fastest = spring.omega_n * remaining.abs();
                let expected = if vel * remaining > 0.0 && vel.abs() > fastest {
                    fastest * remaining.signum()
                } else {
                    vel
                };
                assert!(
                    (spring.velocity_at(at) - expected).abs() < 1e-6,
                    "{label}: velocity {} != {expected} (was {vel})",
                    spring.velocity_at(at)
                );
                if vel * remaining <= 0.0 || vel.abs() <= fastest {
                    assert_eq!(expected, vel, "{label}: velocity changed needlessly");
                }
            }
        }
    }

    #[test]
    fn retarget_forward_never_passes_the_new_target() {
        let now = Instant::now();
        for second in (5..=150).step_by(5) {
            for back in (5..=150).step_by(5) {
                let mut spring = SpringAnimation::with_defaults(0.0, 960.0, now);
                spring.retarget(1920.0, now + ms(second));
                let at = now + ms(second + back);
                let before = spring.value_at(at);
                spring.retarget(960.0, at);
                let dir = (960.0 - before).signum();
                let over = overshoot(&spring, at, 960.0, dir);
                assert!(over < 1.0, "+{second}/+{back} ms: {over:.1} pt past 960");
                let mut prev = before;
                for t in 1..=400 {
                    let v = spring.value_at(at + ms(t));
                    assert!((v - prev).abs() < 100.0, "+{second}/+{back}: jump at {t} ms");
                    prev = v;
                }
                let done = completion_time(&spring, at);
                assert!(done <= ms(400), "+{second}/+{back}: completed after {done:?}");
            }
        }
    }

    #[test]
    fn zero_distance_spring_completes_without_moving() {
        let now = Instant::now();
        let spring = SpringAnimation::with_defaults(500.0, 500.0, now);
        for t in 0..=100 {
            assert_eq!(spring.value_at(now + ms(t)), 500.0);
        }
        assert!(completion_time(&spring, now) <= ms(10));
    }

    #[test]
    fn completion_time_stays_short_for_small_and_large_moves() {
        let now = Instant::now();
        for distance in [1.0, 10.0, 100.0, 1000.0, 3000.0] {
            let spring = SpringAnimation::with_defaults(0.0, distance, now);
            let done = completion_time(&spring, now);
            assert!(done <= ms(400), "{distance} pt: completed after {done:?}");
            assert!((spring.value_at(now + done) - distance).abs() < 0.5);
        }
    }

    #[test]
    fn is_not_complete_while_passing_the_target_fast() {
        let now = Instant::now();
        // 100 pt short of the target and heading past it at 10000 pt/s.
        let spring = SpringAnimation::new(-100.0, 0.0, 10000.0, DEFAULT_RESPONSE, 1.0, now);
        let crossing = (100..=300)
            .map(|n| now + Duration::from_micros(n * 100))
            .find(|&t| spring.value_at(t).abs() < 0.5)
            .expect("the spring passes the target between 10 and 30 ms");
        assert!(spring.velocity_at(crossing).abs() > 1000.0);
        assert!(!spring.is_complete(crossing));
        assert!(spring.is_complete(now + ms(1000)));
    }

    #[test]
    fn nan_target_does_not_panic() {
        let now = Instant::now();
        let mut spring = SpringAnimation::with_defaults(0.0, f64::NAN, now);
        let _ = spring.value_at(now + ms(50));
        let _ = spring.velocity_at(now + ms(50));
        let _ = spring.is_complete(now + ms(50));
        spring.retarget(100.0, now + ms(60));
        let _ = spring.current(now + ms(70));
    }

    #[test]
    fn time_before_the_start_does_not_panic() {
        let now = Instant::now();
        let spring = SpringAnimation::with_defaults(0.0, 100.0, now + ms(50));
        assert_eq!(spring.value_at(now), 0.0);
        assert!(!spring.is_complete(now));
    }
}
