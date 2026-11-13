//! The newest decoded frame of the watched stream, shared in-process with
//! the desktop view. Video never crosses the WebSocket.

use super::decode::Frame;
use std::sync::{Mutex, OnceLock};

type Waker = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    generation: u64,
    sequence: u64,
    frame: Option<Frame>,
}

/// One frame, newest wins. A generation per watched stream keeps frames of
/// a closed stream from ever reaching the view.
#[derive(Default)]
pub struct FrameSlot {
    inner: Mutex<Inner>,
    waker: Mutex<Option<Waker>>,
}

impl FrameSlot {
    /// Starts a new stream; frames from older generations are ignored.
    pub fn open(&self) -> u64 {
        let mut inner = self.lock();
        inner.generation += 1;
        inner.frame = None;
        inner.generation
    }

    pub fn publish(&self, generation: u64, frame: Frame) {
        {
            let mut inner = self.lock();
            if inner.generation != generation {
                return;
            }
            inner.sequence += 1;
            inner.frame = Some(frame);
        }
        if let Some(wake) = self
            .waker
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            wake();
        }
    }

    /// The newest frame, only when it is newer than sequence `after`.
    pub fn latest(&self, after: u64) -> Option<(u64, Frame)> {
        let inner = self.lock();
        let frame = inner.frame.as_ref()?;
        (inner.sequence > after).then(|| (inner.sequence, frame.clone()))
    }

    /// Ends the current stream: its later frames are ignored.
    pub fn close(&self) {
        let mut inner = self.lock();
        inner.generation += 1;
        inner.frame = None;
    }

    pub fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(waker);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// The process-wide slot.
pub fn frames() -> &'static FrameSlot {
    static SLOT: OnceLock<FrameSlot> = OnceLock::new();
    SLOT.get_or_init(FrameSlot::default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn frame(tag: u8) -> Frame {
        Frame {
            width: 1,
            height: 1,
            rgba: vec![tag, 0, 0, 255],
        }
    }

    #[test]
    fn newest_frame_wins() {
        let slot = FrameSlot::default();
        let g = slot.open();
        for tag in 1..=3 {
            slot.publish(g, frame(tag));
        }
        let (sequence, newest) = slot.latest(0).unwrap();
        assert_eq!(newest.rgba[0], 3);
        assert!(slot.latest(sequence).is_none(), "nothing newer yet");
    }

    #[test]
    fn stale_generation_is_dropped() {
        let slot = FrameSlot::default();
        let first = slot.open();
        let second = slot.open();
        slot.publish(first, frame(1));
        assert!(slot.latest(0).is_none());
        slot.publish(second, frame(2));
        assert_eq!(slot.latest(0).unwrap().1.rgba[0], 2);
    }

    #[test]
    fn close_clears() {
        let slot = FrameSlot::default();
        let g = slot.open();
        slot.publish(g, frame(1));
        slot.close();
        assert!(slot.latest(0).is_none());
        slot.publish(g, frame(2));
        assert!(slot.latest(0).is_none(), "closed stream stays closed");
    }

    #[test]
    fn waker_called_once_per_publish() {
        let slot = FrameSlot::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        slot.set_waker(Box::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let g = slot.open();
        slot.publish(g, frame(1));
        slot.publish(g, frame(2));
        slot.publish(g + 99, frame(3));
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
