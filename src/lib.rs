pub mod noise_gate;

use std::sync::atomic::{AtomicUsize, Ordering};

pub const RING_SIZE: usize = 48000 * 2;

/// Lock-free single-producer single-consumer ring buffer for audio.
/// Uses UnsafeCell + atomics — safe because only one thread writes and one reads.
pub struct SpscRing {
    buf: std::cell::UnsafeCell<Vec<f32>>,
    pub write: AtomicUsize,
    read: AtomicUsize,
}

unsafe impl Send for SpscRing {}
unsafe impl Sync for SpscRing {}

impl SpscRing {
    pub fn new(size: usize) -> Self {
        Self {
            buf: std::cell::UnsafeCell::new(vec![0.0; size]),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }

    pub fn reset(&self) {
        self.write.store(0, Ordering::Release);
        self.read.store(0, Ordering::Release);
    }

    /// Push a sample (called from input/producer thread only)
    #[inline]
    pub fn push(&self, sample: f32) {
        let w = self.write.load(Ordering::Relaxed);
        let buf = unsafe { &mut *self.buf.get() };
        buf[w % RING_SIZE] = sample;
        self.write.store((w + 1) % (RING_SIZE * 2), Ordering::Release);
    }

    /// Number of samples available to read
    #[inline]
    pub fn available(&self) -> usize {
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Relaxed);
        if w >= r { w - r } else { RING_SIZE * 2 - r + w }
    }

    /// Read sample at current position without advancing
    #[inline]
    pub fn peek(&self, offset: usize) -> f32 {
        let r = self.read.load(Ordering::Relaxed);
        let buf = unsafe { &*self.buf.get() };
        buf[(r + offset) % RING_SIZE]
    }

    /// Advance read pointer by n
    #[inline]
    pub fn advance(&self, n: usize) {
        let r = self.read.load(Ordering::Relaxed);
        self.read.store((r + n) % (RING_SIZE * 2), Ordering::Release);
    }

    /// Read a sample at an absolute position (for recording tap)
    #[inline]
    pub fn read_at(&self, pos: usize) -> f32 {
        let buf = unsafe { &*self.buf.get() };
        buf[pos % RING_SIZE]
    }
}
