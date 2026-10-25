//! Tile animation: each tile eases from where it is to where the layout wants it.

use std::time::{Duration, Instant};

use persephone_tiling::Rect;

pub const DURATION: Duration = Duration::from_millis(180);
/// New tiles grow from this fraction of their size, around their center.
const POP_IN_SCALE: f32 = 0.88;

#[derive(Clone, Copy, Debug)]
pub struct Motion {
    from: Rect,
    to: Rect,
    start: Instant,
}

impl Motion {
    /// Already at `rect`, not moving.
    pub fn at_rest(rect: Rect) -> Self {
        Self {
            from: rect,
            to: rect,
            start: Instant::now() - DURATION,
        }
    }

    /// Where the tile is drawn at `now`.
    pub fn rect(&self, now: Instant) -> Rect {
        self.from.lerp(&self.to, ease_out_cubic(self.progress(now)))
    }

    pub fn target(&self) -> Rect {
        self.to
    }

    pub fn is_moving(&self, now: Instant) -> bool {
        self.from != self.to && self.progress(now) < 1.0
    }

    /// Glides from the current position to `to`.
    pub fn retarget(&mut self, to: Rect, now: Instant) {
        if to != self.to {
            self.from = self.rect(now);
            self.to = to;
            self.start = now;
        }
    }

    /// Jumps to `to` (e.g. when the window is resized).
    pub fn snap(&mut self, to: Rect) {
        *self = Self::at_rest(to);
    }

    /// Grows into `to` from slightly smaller, the way new windows pop in on Hyprland.
    pub fn pop_in(to: Rect, now: Instant) -> Self {
        let (cx, cy) = to.center();
        let (w, h) = (to.width * POP_IN_SCALE, to.height * POP_IN_SCALE);
        Self {
            from: Rect::new(cx - w / 2.0, cy - h / 2.0, w, h),
            to,
            start: now,
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.start).as_secs_f32() / DURATION.as_secs_f32()).min(1.0)
    }
}

fn ease_out_cubic(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eases_to_the_target_and_stops() {
        let start = Instant::now();
        let a = Rect::new(0.0, 0.0, 100.0, 100.0);
        let b = Rect::new(100.0, 0.0, 50.0, 100.0);
        let mut motion = Motion::at_rest(a);
        motion.retarget(b, start);

        assert_eq!(motion.rect(start), a);
        let mid = motion.rect(start + DURATION / 2);
        assert!(
            mid.x > 50.0 && mid.x < 100.0,
            "ease-out is past halfway at half time: {mid:?}"
        );
        assert!(motion.is_moving(start + DURATION / 2));
        assert_eq!(motion.rect(start + DURATION), b);
        assert!(!motion.is_moving(start + DURATION));
    }

    #[test]
    fn retargeting_mid_flight_starts_from_the_current_position() {
        let start = Instant::now();
        let mut motion = Motion::at_rest(Rect::new(0.0, 0.0, 100.0, 100.0));
        motion.retarget(Rect::new(200.0, 0.0, 100.0, 100.0), start);
        let halfway = start + DURATION / 2;
        let here = motion.rect(halfway);
        motion.retarget(Rect::new(0.0, 0.0, 100.0, 100.0), halfway);
        assert_eq!(motion.rect(halfway), here, "no jump");
    }

    #[test]
    fn pops_in_from_the_center() {
        let now = Instant::now();
        let to = Rect::new(100.0, 100.0, 200.0, 100.0);
        let motion = Motion::pop_in(to, now);
        let first = motion.rect(now);
        assert_eq!(first.center(), to.center());
        assert!(first.width < to.width);
        assert_eq!(motion.rect(now + DURATION), to);
    }
}
