//! A wallpaper's timeline is shared by its outputs, including late arrivals.
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Timeline {
    pub position_ns: u64,
    pub sampled_ns: u64,
    pub running: bool,
}

impl Timeline {
    pub fn new(now: u64, running: bool) -> Self {
        Self {
            position_ns: 0,
            sampled_ns: now,
            running,
        }
    }

    pub fn position(self, now: u64) -> u64 {
        self.position_ns.saturating_add(if self.running {
            now.saturating_sub(self.sampled_ns)
        } else {
            0
        })
    }

    pub fn set_running(&mut self, now: u64, running: bool) {
        if self.running != running {
            self.position_ns = self.position(now);
            self.sampled_ns = now;
            self.running = running;
        }
    }
}

/// A consumer can pause independently, then rejoin the owner's shared timeline.
/// Local elapsed time and shared progress both use CLOCK_MONOTONIC nanoseconds.
pub struct PlaybackClock {
    elapsed_ns: u64,
    running_since: Option<u64>,
    timeline: Option<Timeline>,
}
impl PlaybackClock {
    pub fn new(paused: bool) -> Self {
        Self {
            elapsed_ns: 0,
            running_since: (!paused).then(now),
            timeline: None,
        }
    }
    pub fn pause(&mut self, paused: bool) {
        match (paused, self.running_since) {
            (true, Some(_)) => {
                self.elapsed_ns = self.position(now());
                self.running_since = None;
            }
            (false, None) => self.running_since = Some(now()),
            _ => {}
        }
    }
    fn position(&self, now: u64) -> u64 {
        if self.running_since.is_some()
            && let Some(timeline) = self.timeline
        {
            return timeline.position(now);
        }
        self.elapsed_ns.saturating_add(
            self.running_since
                .map_or(0, |start| now.saturating_sub(start)),
        )
    }
    pub fn elapsed(&self) -> Duration {
        Duration::from_nanos(self.position(now()))
    }
    pub fn seek(&mut self, time: Duration) {
        self.elapsed_ns = time.as_nanos().min(u64::MAX as u128) as u64;
        if self.running_since.is_some() {
            self.running_since = Some(now());
        }
    }
    pub fn synchronize(&mut self, timeline: Timeline) {
        if self.running_since.is_none() && (self.timeline.is_none() || !timeline.running) {
            self.elapsed_ns = timeline.position(now());
        }
        self.timeline = Some(timeline);
    }
}

pub fn now() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_MONOTONIC is available on Linux; time is a writable timespec.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) },
        0
    );
    time.tv_sec as u64 * 1_000_000_000 + time.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_joiners_share_progress_and_all_paused_time_is_excluded() {
        let mut clock = Timeline::new(100, true);
        let first = clock;
        let joined = clock;
        assert_eq!(first.position(300), joined.position(300));
        clock.set_running(400, false);
        assert_eq!(clock.position(900), 300);
        clock.set_running(1000, true);
        assert_eq!(clock.position(1100), 400);
        let resumed = clock;
        assert_eq!(resumed.position(1200), clock.position(1200));
    }

    #[test]
    fn consumer_pause_keeps_local_frame_and_resume_rejoins_owner() {
        let mut clock = PlaybackClock {
            elapsed_ns: 10,
            running_since: None,
            timeline: None,
        };
        clock.timeline = Some(Timeline {
            position_ns: 100,
            sampled_ns: 1000,
            running: true,
        });
        assert_eq!(clock.position(1500), 10);
        clock.running_since = Some(1500);
        assert_eq!(clock.position(1500), 600);
        clock.elapsed_ns = clock.position(1600);
        clock.running_since = None;
        assert_eq!(clock.position(9000), 700);
    }
}
