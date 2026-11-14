//! Replay drain for the audio interface.
//!
//! While the replay clock runs, the guest-visible AI is driven by this drain instead of
//! the output device. Replay runs faster than the device plays, so the device's
//! position would make AI length, status and submit results depend on host timing.
//! The drain keeps the live model and swaps its clock: the same queue cap and 80 ms
//! prebuffer as the Arie player, and frames retired at the source rate against the
//! replay clock. Replay pushes nothing to the device.
//!
//! The model is reality64's live AI, not the console's. The N64 AI has a two-entry
//! FIFO, reports the length left in the buffer it is playing, and has no prebuffer;
//! this drain leaves those differences exactly as they are in live play.

/// Frames covered by `nanos` at `rate`, rounded up as Arie sizes its queue and prebuffer.
pub(crate) fn frames_ceil(nanos: u128, rate: u32) -> u64 {
    (nanos * u128::from(rate)).div_ceil(1_000_000_000) as u64
}

#[derive(Clone, Debug)]
pub(crate) struct ReplayDrain {
    rate: u32,
    capacity: u64,
    prebuffer: u64,
    accepted: u64,
    retired: u64,
    /// Replay time and retired position when playback last started; `None` while
    /// prebuffering or starved.
    run: Option<(u64, u64)>,
}

impl ReplayDrain {
    pub(crate) fn new(rate: u32, capacity: u64, prebuffer: u64) -> Self {
        Self {
            rate,
            capacity,
            prebuffer,
            accepted: 0,
            retired: 0,
            run: None,
        }
    }

    /// Retire the frames played by `now`. Playback stops when the queue empties and
    /// waits for another prebuffer, as the device gate does after a starve.
    pub(crate) fn advance(&mut self, now: u64) {
        if let Some((start, from)) = self.run {
            let played = frames_floor(now.saturating_sub(start), self.rate);
            self.retired = from.saturating_add(played).min(self.accepted);
            if self.retired == self.accepted {
                self.run = None;
            }
        }
    }

    /// Accept `frames` at `now`, or `None` when they don't fit under the cap.
    pub(crate) fn queue(&mut self, frames: u64, now: u64) -> Option<(u64, u64)> {
        if self.queued().saturating_add(frames) > self.capacity {
            return None;
        }
        let start = self.accepted;
        self.accepted += frames;
        if self.run.is_none() && self.queued() >= self.prebuffer {
            self.run = Some((now, self.retired));
        }
        Some((start, self.accepted))
    }

    pub(crate) fn clear(&mut self) {
        self.retired = self.accepted;
        self.run = None;
    }

    pub(crate) fn retired(&self) -> u64 {
        self.retired
    }

    pub(crate) fn queued(&self) -> u64 {
        self.accepted - self.retired
    }

    pub(crate) fn running(&self) -> bool {
        self.run.is_some()
    }
}

fn frames_floor(nanos: u64, rate: u32) -> u64 {
    (u128::from(nanos) * u128::from(rate) / 1_000_000_000) as u64
}
