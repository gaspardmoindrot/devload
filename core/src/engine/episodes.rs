//! Pressure episodes: a stretch of orange or red, stored for the Analyze
//! prompt with its level and biggest group.

use crate::model::{Episode, Gid, Pressure};

/// An episode that ended this recently before the app (re)started continues
/// when the colour is still orange or red: a relaunch, or a crash (the store
/// ends a crashed episode at its last write, up to 5 min before the crash),
/// doesn't start a new one.
pub const RESUME_SECS: i64 = 600;

#[derive(Default)]
pub struct Episodes {
    open: Option<Episode>,
    /// Closed episodes not yet written.
    closed: Vec<Episode>,
    /// The last episode of the previous run, which the first episode of this
    /// run may continue.
    resume: Option<Episode>,
}

impl Episodes {
    /// `last`: the latest stored episode, from the previous run.
    pub fn resuming(last: Option<Episode>) -> Self {
        Episodes { resume: last.filter(|e| e.end.is_some()), ..Episodes::default() }
    }

    /// Feeds the displayed colour (and since when it holds). Returns the
    /// level when a new episode starts; escalating from orange to red stays
    /// in the same episode, and so does one that continues the previous
    /// run's (see [`RESUME_SECS`]).
    pub fn update(&mut self, shown: Pressure, since: i64, top_gid: Option<Gid>) -> Option<Pressure> {
        match (&mut self.open, shown) {
            (None, Pressure::Normal) => None,
            (None, level) => {
                // Same or lower level than the previous run's episode, soon
                // after it ended: the same episode. Higher: a new one.
                if let Some(prev) = self.resume.take()
                    && let Some(end) = prev.end
                    && level <= prev.level
                    && since >= end
                    && since - end <= RESUME_SECS
                {
                    self.open = Some(Episode { end: None, ..prev });
                    return None;
                }
                self.open = Some(Episode { start: since, end: None, level, top_gid });
                Some(level)
            }
            (Some(e), Pressure::Normal) => {
                e.end = Some(since);
                self.closed.push(self.open.take().expect("open"));
                None
            }
            (Some(e), level) => {
                if level > e.level {
                    e.level = level;
                    // Who is biggest when it turns red says more than at the start.
                    e.top_gid = top_gid.or(e.top_gid);
                }
                None
            }
        }
    }

    /// The app stops: an open episode ends at `now`.
    pub fn close(&mut self, now: i64) {
        if let Some(mut e) = self.open.take() {
            e.end = Some(now);
            self.closed.push(e);
        }
    }

    pub fn current(&self) -> Option<&Episode> {
        self.open.as_ref()
    }

    /// Episodes to write: the closed ones, then the open one.
    pub fn pending(&self) -> Vec<Episode> {
        self.closed.iter().chain(self.open.as_ref()).cloned().collect()
    }

    /// After a successful write.
    pub fn written(&mut self) {
        self.closed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Pressure::*;

    #[test]
    fn one_episode_per_stretch_and_its_bounds() {
        let mut e = Episodes::default();
        assert_eq!(e.update(Normal, 0, Some(1)), None);
        assert_eq!(e.update(Tight, 100, Some(1)), Some(Tight));
        assert_eq!(e.update(Tight, 100, Some(2)), None);
        assert_eq!(e.update(Critical, 200, Some(3)), None, "escalation stays in the episode");
        assert_eq!(e.current().unwrap().level, Critical);
        assert_eq!(e.current().unwrap().top_gid, Some(3));
        assert_eq!(e.update(Tight, 300, Some(3)), None);
        assert_eq!(e.pending().len(), 1);
        assert_eq!(e.update(Normal, 400, Some(1)), None);
        let p = e.pending();
        assert_eq!(p, [Episode { start: 100, end: Some(400), level: Critical, top_gid: Some(3) }]);
        e.written();
        assert!(e.pending().is_empty());
        assert_eq!(e.update(Critical, 500, None), Some(Critical));
        assert_eq!(e.pending()[0].end, None);
    }

    const PREV: Episode = Episode { start: 1000, end: Some(2000), level: Critical, top_gid: Some(7) };

    #[test]
    fn a_relaunch_during_an_episode_continues_it_silently() {
        for level in [Tight, Critical] {
            let mut e = Episodes::resuming(Some(PREV));
            // A Normal tick first (low-memory Tight needs two ticks) keeps it.
            assert_eq!(e.update(Normal, 2030, None), None);
            assert_eq!(e.update(level, 2035, Some(9)), None, "{level:?}");
            assert_eq!(e.pending(), [Episode { end: None, ..PREV }], "same start: the stored row is reopened");
            assert_eq!(e.update(Normal, 2500, None), None);
            assert_eq!(e.pending(), [Episode { end: Some(2500), ..PREV }]);
            // The next episode is a new one.
            assert_eq!(e.update(level, 2600, None), Some(level));
        }
    }

    #[test]
    fn a_new_episode_after_a_relaunch_is_new() {
        // Too long after the previous one ended.
        let mut e = Episodes::resuming(Some(PREV));
        assert_eq!(e.update(Tight, 2000 + RESUME_SECS + 1, None), Some(Tight));
        assert_eq!(e.current().unwrap().start, 2000 + RESUME_SECS + 1);
        // Worse than the previous one: tight before, critical now.
        let mut e = Episodes::resuming(Some(Episode { level: Tight, ..PREV }));
        assert_eq!(e.update(Critical, 2010, None), Some(Critical));
        // Nothing stored, or a row still open (the store had no chance to
        // close it).
        assert_eq!(Episodes::resuming(None).update(Tight, 2010, None), Some(Tight));
        assert_eq!(Episodes::resuming(Some(Episode { end: None, ..PREV })).update(Tight, 2010, None), Some(Tight));
        // Clock set back before the previous end.
        assert_eq!(Episodes::resuming(Some(PREV)).update(Tight, 1500, None), Some(Tight));
    }
}
