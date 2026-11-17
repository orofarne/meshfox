//! Keeping wheel scrolling smooth over bitmap (iTerm2/Sixel) images.
//!
//! A bitmap image that is clipped by the pane edge is a different encoded
//! fragment at every scroll offset. This module tracks how fast the wheel is
//! turning (so mid-scroll frames can skip encoding such fragments) and, once
//! the user pauses, warms the fragments the next few wheel steps will need.
use super::image_render::DocumentImage;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// Wheel events closer together than this count as a fast scroll.
const FAST_GAP: Duration = Duration::from_millis(150);
/// How long after the last fast wheel event frames stay "mid-scroll".
const SETTLE: Duration = Duration::from_millis(400);
/// Rows one scroll step moves the document until a real step is seen.
const DEFAULT_STEP: i32 = 3;
/// Wheel steps warmed ahead in the scroll direction, and behind it.
const WARM_AHEAD: i32 = 6;
const WARM_BEHIND: i32 = 2;

/// Where one bitmap image sat in the document pane in the last frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ImageWindow {
    pub path: PathBuf,
    /// Pane row of the image's first row (negative: scrolled partly off the top).
    pub top: i32,
    pub pane_height: u16,
    pub rows: u16,
    pub skip_col: u16,
    pub width: u16,
}

#[derive(Default)]
pub struct ScrollImages {
    last_event: Option<Instant>,
    fast_until: Option<Instant>,
    direction: i32,
    /// Rows the last scroll step moved: 3 for a wheel notch, 1 for an arrow key.
    step: i32,
    windows: Vec<ImageWindow>,
    recorded: Vec<ImageWindow>,
    queue: Vec<(PathBuf, [u16; 4])>,
    built: bool,
    /// Images that showed a placeholder in this fast scroll. They keep it
    /// until it settles: flipping between placeholder and picture from frame
    /// to frame (as the cache hits and misses come and go) is what flickers.
    deferred: std::collections::HashSet<PathBuf>,
}

impl ScrollImages {
    /// A document scroll of `delta` rows happened.
    pub fn note_scroll(&mut self, delta: i32, now: Instant) {
        if delta != 0 {
            self.direction = delta.signum();
            self.step = delta.abs();
        }
        if self
            .last_event
            .is_some_and(|t| now.duration_since(t) < FAST_GAP)
        {
            self.fast_until = Some(now + SETTLE);
        }
        self.last_event = Some(now);
    }

    /// Whether the wheel is turning fast enough that a frame should not pay
    /// for encoding new image fragments.
    pub fn is_scrolling(&self, now: Instant) -> bool {
        self.fast_until.is_some_and(|t| now < t)
    }

    /// When the current fast scroll counts as over (to redraw with real images).
    pub fn settle_deadline(&self) -> Option<Instant> {
        self.fast_until
    }

    pub fn settle(&mut self, now: Instant) {
        if self.fast_until.is_some_and(|t| now >= t) {
            self.fast_until = None;
            self.deferred.clear();
        }
    }

    pub fn is_deferred(&self, path: &PathBuf) -> bool {
        self.deferred.contains(path)
    }

    pub fn defer(&mut self, path: &PathBuf) {
        self.deferred.insert(path.clone());
    }

    pub fn begin_frame(&mut self) {
        self.recorded.clear();
    }

    pub fn record(&mut self, window: ImageWindow) {
        self.recorded.push(window);
    }

    /// Called after a frame: a changed picture invalidates the warm queue.
    pub fn end_frame(&mut self) {
        if self.recorded != self.windows {
            self.windows = std::mem::take(&mut self.recorded);
            self.queue.clear();
            self.built = false;
        }
    }

    /// Whether there is idle warming work. It also runs between the events
    /// of a fast scroll (an encode takes less than a key repeat), so the
    /// settled frame finds its fragments ready.
    pub fn warm_pending(&self) -> bool {
        !self.windows.is_empty() && (!self.built || !self.queue.is_empty())
    }

    fn build_queue(&mut self) {
        self.built = true;
        if self.step == 0 {
            self.step = DEFAULT_STEP;
        }
        let dir = if self.direction == 0 {
            1
        } else {
            self.direction
        };
        let steps = (1..=WARM_AHEAD)
            .map(|k| dir * k)
            .chain((1..=WARM_BEHIND).map(|k| -dir * k));
        let mut queue = Vec::new();
        for step in steps {
            for w in &self.windows {
                let top = w.top - step * self.step.max(1);
                let start = (-top).max(0);
                let end = (i32::from(w.pane_height) - top).min(i32::from(w.rows));
                if end <= start {
                    continue;
                }
                queue.push((
                    w.path.clone(),
                    [start as u16, (end - start) as u16, w.skip_col, w.width],
                ));
            }
        }
        // `pop` takes from the back, so the nearest step goes last.
        queue.reverse();
        self.queue = queue;
    }

    /// Encode at most one missing fragment. Returns whether work was done.
    pub fn warm_one(&mut self, images: &HashMap<PathBuf, Option<Arc<DocumentImage>>>) -> bool {
        if !self.built {
            self.build_queue();
        }
        while let Some((path, [skip, height, col, width])) = self.queue.pop() {
            if let Some(Some(image)) = images.get(&path) {
                if image.warm_fragment(skip, height, col, width) {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(top: i32) -> ImageWindow {
        ImageWindow {
            path: "a.png".into(),
            top,
            pane_height: 20,
            rows: 30,
            skip_col: 0,
            width: 10,
        }
    }

    #[test]
    fn only_closely_spaced_wheel_events_count_as_fast() {
        let mut s = ScrollImages::default();
        let t = Instant::now();
        s.note_scroll(3, t);
        assert!(!s.is_scrolling(t));
        s.note_scroll(3, t + Duration::from_millis(20));
        assert!(s.is_scrolling(t + Duration::from_millis(30)));
        assert!(!s.is_scrolling(t + Duration::from_millis(500)));
        s.note_scroll(3, t + Duration::from_millis(1000));
        assert!(!s.is_scrolling(t + Duration::from_millis(1001)));
    }

    #[test]
    fn a_deferred_image_stays_deferred_until_the_scroll_settles() {
        let mut s = ScrollImages::default();
        let t = Instant::now();
        s.note_scroll(3, t);
        s.note_scroll(3, t + Duration::from_millis(20));
        let path = PathBuf::from("a.png");
        s.defer(&path);
        assert!(s.is_deferred(&path));
        s.settle(t + Duration::from_millis(20) + SETTLE);
        assert!(!s.is_deferred(&path));
    }

    #[test]
    fn warm_queue_covers_the_windows_of_the_next_wheel_steps() {
        let mut s = ScrollImages::default();
        s.note_scroll(3, Instant::now());
        s.begin_frame();
        s.record(window(-4));
        s.end_frame();
        s.build_queue();
        // Nearest step down: top -7 → rows 7..27.
        assert_eq!(s.queue.last().unwrap().1, [7, 20, 0, 10]);
        // Every step is a distinct window.
        let mut keys: Vec<_> = s.queue.iter().map(|(_, k)| *k).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), s.queue.len());
    }

    #[test]
    fn an_unchanged_frame_keeps_the_queue() {
        let mut s = ScrollImages::default();
        s.begin_frame();
        s.record(window(0));
        s.end_frame();
        s.build_queue();
        let n = s.queue.len();
        s.begin_frame();
        s.record(window(0));
        s.end_frame();
        assert!(s.built);
        assert_eq!(s.queue.len(), n);
        s.begin_frame();
        s.record(window(-3));
        s.end_frame();
        assert!(!s.built);
    }
}
