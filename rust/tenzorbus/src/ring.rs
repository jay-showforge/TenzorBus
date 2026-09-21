//! The Linux production ring.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tenzor_core as proto;
use tenzor_core::{DType, seq_newer};

use crate::error::{Error, Result};
use crate::futex;
use crate::shm::{self, Mapping};

/// Ordering of the store that publishes a slot, and of the load a consumer uses
/// to validate one. Together they are the release/acquire pair that makes the
/// producer's payload copy visible to readers.
///
/// The `tsan-positive-control` feature downgrades both to `Relaxed` so the
/// ThreadSanitizer gate can demonstrate that it detects a real race in this
/// crate. See `scripts/run_tsan.sh`.
#[cfg(not(feature = "tsan-positive-control"))]
const COMMIT_RELEASE: Ordering = Ordering::Release;
#[cfg(not(feature = "tsan-positive-control"))]
const COMMIT_ACQUIRE: Ordering = Ordering::Acquire;
#[cfg(feature = "tsan-positive-control")]
const COMMIT_RELEASE: Ordering = Ordering::Relaxed;
#[cfg(feature = "tsan-positive-control")]
const COMMIT_ACQUIRE: Ordering = Ordering::Relaxed;

// ---------------------------------------------------------------------------
// Public value types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backpressure {
    /// Wait for a slot to become reclaimable, up to the publish timeout.
    Block,
    /// Give up immediately and count a drop.
    DropNewest,
}

#[derive(Debug, Clone, Copy)]
pub struct RingLayout {
    pub slot_count: usize,
    pub slot_capacity: usize,
}

impl RingLayout {
    pub fn new(slot_count: usize, slot_capacity: usize) -> std::result::Result<Self, &'static str> {
        if slot_count < 2 {
            return Err("slot_count must be >= 2");
        }
        if slot_capacity < 64 {
            return Err("slot_capacity must be >= 64");
        }
        Ok(Self {
            slot_count,
            slot_capacity,
        })
    }

    pub fn total_bytes(&self) -> usize {
        proto::total_bytes(self.slot_count, self.slot_capacity)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RingOptions {
    pub slot_count: usize,
    pub slot_capacity: usize,
    /// Unlink an existing object of the same name before creating.
    pub force: bool,
}

impl Default for RingOptions {
    fn default() -> Self {
        Self {
            slot_count: 8,
            slot_capacity: 1 << 20,
            force: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublishResult {
    pub sequence: u64,
    pub slot_index: usize,
    pub nbytes: usize,
    pub readers: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TensorMeta {
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub nbytes: usize,
    pub dtype: DType,
    pub ndim: usize,
    pub shape: [u32; proto::MAX_NDIM],
    pub strides: [u32; proto::MAX_NDIM],
    pub slot_index: usize,
    pub readers_remaining: u32,
}

impl TensorMeta {
    pub fn shape(&self) -> &[u32] {
        &self.shape[..self.ndim]
    }
    pub fn strides(&self) -> &[u32] {
        &self.strides[..self.ndim]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingStats {
    pub slot_count: usize,
    pub slot_capacity: usize,
    pub next_sequence: u64,
    pub consumers: u32,
    pub published: u64,
    pub dropped: u64,
    pub reaped: u64,
    pub free_slots: usize,
    pub committed_slots: usize,
}

/// A borrowed, C-contiguous tensor ready to publish.
#[derive(Debug, Clone, Copy)]
pub struct TensorView<'a> {
    dtype: DType,
    ndim: usize,
    shape: [u32; proto::MAX_NDIM],
    strides: [u32; proto::MAX_NDIM],
    data: &'a [u8],
    /// Capture time of the tensor itself. `None` means "stamp it at publish".
    /// A media pipeline must set this: the slot timestamp is what lets a
    /// consumer align a frame with the rest of the clip, and wall-clock time at
    /// publish is not that.
    timestamp_ns: Option<u64>,
}

impl<'a> TensorView<'a> {
    pub fn new(
        dtype: DType,
        shape: &[u32],
        strides: &[u32],
        data: &'a [u8],
    ) -> Result<TensorView<'a>> {
        if shape.len() > proto::MAX_NDIM {
            return Err(Error::Invalid(format!(
                "ndim {} exceeds protocol max {}",
                shape.len(),
                proto::MAX_NDIM
            )));
        }
        if strides.len() != shape.len() {
            return Err(Error::Invalid("strides must match shape rank".into()));
        }
        let elements: u64 = shape.iter().map(|d| *d as u64).product::<u64>();
        let expected = elements as usize * dtype.itemsize();
        if expected != data.len() {
            return Err(Error::Invalid(format!(
                "shape implies {expected} bytes but buffer is {} bytes",
                data.len()
            )));
        }
        let mut s = [0u32; proto::MAX_NDIM];
        let mut st = [0u32; proto::MAX_NDIM];
        s[..shape.len()].copy_from_slice(shape);
        st[..strides.len()].copy_from_slice(strides);
        Ok(TensorView {
            dtype,
            ndim: shape.len(),
            shape: s,
            strides: st,
            data,
            timestamp_ns: None,
        })
    }

    /// Carry the source's own capture timestamp into the slot header instead of
    /// stamping wall-clock time at publish.
    pub fn with_timestamp_ns(mut self, timestamp_ns: u64) -> Self {
        self.timestamp_ns = Some(timestamp_ns);
        self
    }

    pub fn timestamp_ns(&self) -> Option<u64> {
        self.timestamp_ns
    }

    /// Build a view with C-contiguous byte strides, matching NumPy's convention.
    pub fn contiguous(dtype: DType, shape: &[u32], data: &'a [u8]) -> Result<TensorView<'a>> {
        let mut strides = vec![0u32; shape.len()];
        let mut acc = dtype.itemsize() as u64;
        for i in (0..shape.len()).rev() {
            strides[i] = acc as u32;
            acc *= shape[i] as u64;
        }
        TensorView::new(dtype, shape, &strides, data)
    }

    pub fn nbytes(&self) -> usize {
        self.data.len()
    }
}

// ---------------------------------------------------------------------------
// Ring
// ---------------------------------------------------------------------------

struct Inner {
    name: String,
    map: Mapping,
    slot_count: usize,
    slot_capacity: usize,
    slot_stride: usize,
    owner: bool,
    unlink_on_drop: Cell<bool>,
    /// Live `Producer` handles in this process. The producer role is released
    /// back to the ring when the last one drops.
    producers: std::sync::atomic::AtomicUsize,
}

// Cell<bool> is only touched from the owning handle before drop.
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.unlink_on_drop.get() {
            let _ = shm::unlink(&self.name);
        }
    }
}

#[derive(Clone)]
pub struct Ring {
    inner: Arc<Inner>,
}

fn safe_name(name: &str) -> Result<String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        return Err(Error::Invalid(
            "ring name must contain at least one safe character".into(),
        ));
    }
    Ok(cleaned.chars().take(64).collect())
}

impl Ring {
    pub fn create(name: &str, opts: RingOptions) -> Result<Ring> {
        let name = safe_name(name)?;
        let layout = RingLayout::new(opts.slot_count, opts.slot_capacity)
            .map_err(|m| Error::Invalid(m.into()))?;
        let map = shm::create(&name, layout.total_bytes(), opts.force)?;
        let inner = Inner {
            name,
            map,
            slot_count: layout.slot_count,
            slot_capacity: layout.slot_capacity,
            slot_stride: proto::slot_stride(layout.slot_capacity),
            owner: true,
            unlink_on_drop: Cell::new(true),
            producers: std::sync::atomic::AtomicUsize::new(0),
        };
        let ring = Ring {
            inner: Arc::new(inner),
        };
        ring.format();
        Ok(ring)
    }

    pub fn attach(name: &str) -> Result<Ring> {
        let name = safe_name(name)?;
        let map = shm::attach(&name)?;
        let inner = Inner {
            name,
            map,
            slot_count: 0,
            slot_capacity: 0,
            slot_stride: 0,
            owner: false,
            unlink_on_drop: Cell::new(false),
            producers: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut inner = inner;
        // Validate then learn the geometry from the header itself.
        {
            let base = inner.map.as_ptr();
            let magic = unsafe { std::slice::from_raw_parts(base.add(proto::G_MAGIC), 8) };
            if magic != proto::PROTOCOL_MAGIC {
                return Err(Error::BadHeader("invalid TenzorBus shared-memory magic"));
            }
            let version = unsafe { &*(base.add(proto::G_VERSION) as *const AtomicU32) }
                .load(Ordering::Relaxed);
            if version != proto::PROTOCOL_VERSION {
                return Err(Error::BadHeader("unsupported TenzorBus protocol version"));
            }
            let slot_count = unsafe { &*(base.add(proto::G_SLOT_COUNT) as *const AtomicU32) }
                .load(Ordering::Relaxed) as usize;
            let slot_capacity = unsafe { &*(base.add(proto::G_SLOT_CAPACITY) as *const AtomicU64) }
                .load(Ordering::Relaxed) as usize;
            if slot_count < 2 || slot_capacity < 64 {
                return Err(Error::BadHeader("degenerate ring geometry in header"));
            }
            if proto::total_bytes(slot_count, slot_capacity) > inner.map.len() {
                return Err(Error::BadHeader("header geometry exceeds mapping length"));
            }
            inner.slot_count = slot_count;
            inner.slot_capacity = slot_capacity;
            inner.slot_stride = proto::slot_stride(slot_capacity);
        }
        Ok(Ring {
            inner: Arc::new(inner),
        })
    }

    /// Open the ring, creating it if it does not exist yet.
    pub fn open_or_create(name: &str, opts: RingOptions) -> Result<Ring> {
        match Ring::create(name, opts) {
            Ok(r) => Ok(r),
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Ring::attach(name)
            }
            Err(e) => Err(e),
        }
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }
    pub fn slot_count(&self) -> usize {
        self.inner.slot_count
    }
    pub fn slot_capacity(&self) -> usize {
        self.inner.slot_capacity
    }
    pub fn is_owner(&self) -> bool {
        self.inner.owner
    }

    /// Detach without removing the shared object (the default for attached rings).
    pub fn keep_on_drop(&self) {
        self.inner.unlink_on_drop.set(false);
    }

    /// Remove the shared object when the last handle in this process drops.
    pub fn unlink_on_drop(&self) {
        self.inner.unlink_on_drop.set(true);
    }

    // -- raw accessors ------------------------------------------------------

    #[inline]
    fn base(&self) -> *mut u8 {
        self.inner.map.as_ptr()
    }

    #[inline]
    fn au32(&self, off: usize) -> &AtomicU32 {
        unsafe { &*(self.base().add(off) as *const AtomicU32) }
    }

    #[inline]
    fn au64(&self, off: usize) -> &AtomicU64 {
        unsafe { &*(self.base().add(off) as *const AtomicU64) }
    }

    #[inline]
    fn au16(&self, off: usize) -> &AtomicU16 {
        unsafe { &*(self.base().add(off) as *const AtomicU16) }
    }

    #[inline]
    fn slot_off(&self, index: usize) -> usize {
        proto::GLOBAL_HEADER_SIZE + index * self.inner.slot_stride
    }

    #[inline]
    fn payload_off(&self, index: usize) -> usize {
        self.slot_off(index) + proto::SLOT_HEADER_SIZE
    }

    #[inline]
    fn slot_state(&self, index: usize) -> &AtomicU32 {
        self.au32(self.slot_off(index) + proto::S_STATE)
    }

    #[inline]
    fn slot_seq(&self, index: usize) -> &AtomicU64 {
        self.au64(self.slot_off(index) + proto::S_SEQUENCE)
    }

    #[inline]
    fn slot_mask(&self, index: usize) -> &AtomicU64 {
        self.au64(self.slot_off(index) + proto::S_READERS_MASK)
    }

    #[inline]
    fn slot_readers(&self, index: usize) -> &AtomicU16 {
        self.au16(self.slot_off(index) + proto::S_READERS)
    }

    #[inline]
    fn slot_producer_pid(&self, index: usize) -> &AtomicU32 {
        self.au32(self.slot_off(index) + proto::S_PRODUCER_PID)
    }

    #[inline]
    fn entry(&self, index: usize, field: usize) -> usize {
        proto::registry_entry_offset(index) + field
    }

    fn format(&self) {
        let len = self.inner.map.len();
        unsafe { std::ptr::write_bytes(self.base(), 0, len) };
        unsafe {
            std::ptr::copy_nonoverlapping(
                proto::PROTOCOL_MAGIC.as_ptr(),
                self.base().add(proto::G_MAGIC),
                8,
            )
        };
        self.au32(proto::G_VERSION)
            .store(proto::PROTOCOL_VERSION, Ordering::Relaxed);
        self.au32(proto::G_SLOT_COUNT)
            .store(self.inner.slot_count as u32, Ordering::Relaxed);
        self.au64(proto::G_SLOT_CAPACITY)
            .store(self.inner.slot_capacity as u64, Ordering::Relaxed);
        self.au64(proto::G_NEXT_SEQ).store(1, Ordering::Relaxed);
        self.au32(proto::G_CONSUMERS).store(0, Ordering::Relaxed);
        self.au32(proto::G_MAX_CONSUMERS)
            .store(proto::MAX_CONSUMERS as u32, Ordering::Relaxed);
        for i in 0..self.inner.slot_count {
            let off = self.slot_off(i);
            unsafe {
                std::ptr::copy_nonoverlapping(
                    proto::SLOT_MAGIC.as_ptr(),
                    self.base().add(off + proto::S_MAGIC),
                    4,
                )
            };
            self.slot_state(i)
                .store(proto::STATE_FREE, Ordering::Relaxed);
        }
        std::sync::atomic::fence(Ordering::SeqCst);
    }

    /// Address of the shared mapping. Exposed so tests can prove a consumer
    /// view aliases the slab rather than a copy.
    #[doc(hidden)]
    pub fn base_addr(&self) -> usize {
        self.base() as usize
    }

    /// Byte-for-byte copy of the whole mapping. Used by the golden-bytes gate
    /// to hand a real, producer-written image to the Python reference parser.
    pub fn snapshot(&self) -> Vec<u8> {
        let len = self.inner.map.len();
        let mut out = vec![0u8; len];
        unsafe { std::ptr::copy_nonoverlapping(self.base(), out.as_mut_ptr(), len) };
        out
    }

    /// Test hook: start the sequence counter near the u64 boundary so the
    /// wraparound path is exercised without publishing 2^64 tensors.
    pub fn set_next_sequence(&self, seq: u64) {
        self.au64(proto::G_NEXT_SEQ).store(seq, Ordering::SeqCst);
    }

    // -- registry -----------------------------------------------------------

    fn lock_registry(&self) {
        futex::lock(
            self.au32(proto::G_REGISTRY_LOCK),
            self.au32(proto::G_REGISTRY_LOCK_OWNER),
            self.au32(proto::G_REGISTRY_LOCK_OWNER_TOKEN),
            std::process::id(),
            shm::pid_start_token(std::process::id()),
        );
    }

    fn unlock_registry(&self) {
        futex::unlock(
            self.au32(proto::G_REGISTRY_LOCK),
            self.au32(proto::G_REGISTRY_LOCK_OWNER),
            self.au32(proto::G_REGISTRY_LOCK_OWNER_TOKEN),
        );
    }

    fn active_mask(&self) -> u64 {
        let mut mask = 0u64;
        for i in 0..proto::MAX_CONSUMERS {
            if self
                .au32(self.entry(i, proto::R_STATE))
                .load(Ordering::Relaxed)
                == proto::REGISTRY_ACTIVE
            {
                mask |= 1u64 << i;
            }
        }
        mask
    }

    /// Clear one consumer's bit from every slot. Called under the registry lock
    /// on unregister and on reap.
    fn drop_consumer_bit(&self, index: usize) -> bool {
        let bit = 1u64 << index;
        let mut freed_any = false;
        for s in 0..self.inner.slot_count {
            let previous = self.slot_mask(s).fetch_and(!bit, Ordering::AcqRel);
            if previous & bit != 0 {
                let now = previous & !bit;
                self.slot_readers(s)
                    .store(now.count_ones() as u16, Ordering::Release);
                if now == 0 {
                    freed_any = true;
                }
            }
        }
        freed_any
    }

    /// Reclaim registry entries whose process is gone, and slots abandoned
    /// mid-write by a dead producer. Must be called under the registry lock.
    fn reap_dead(&self) {
        let me = std::process::id();
        let mut freed = false;
        for i in 0..proto::MAX_CONSUMERS {
            if self
                .au32(self.entry(i, proto::R_STATE))
                .load(Ordering::Relaxed)
                != proto::REGISTRY_ACTIVE
            {
                continue;
            }
            let pid = self
                .au32(self.entry(i, proto::R_PID))
                .load(Ordering::Relaxed);
            if pid == me || shm::pid_alive(pid) {
                continue;
            }
            freed |= self.drop_consumer_bit(i);
            self.au32(self.entry(i, proto::R_GENERATION))
                .fetch_add(1, Ordering::Relaxed);
            self.au32(self.entry(i, proto::R_STATE))
                .store(proto::REGISTRY_FREE, Ordering::Release);
            let previous = self.au32(proto::G_CONSUMERS).load(Ordering::Relaxed);
            self.au32(proto::G_CONSUMERS)
                .store(previous.saturating_sub(1), Ordering::Relaxed);
            self.au64(proto::G_REAPED_COUNT)
                .fetch_add(1, Ordering::Relaxed);
            self.au32(proto::G_REGISTRY_EPOCH)
                .fetch_add(1, Ordering::Relaxed);
        }
        // A producer killed between WRITING and COMMITTED leaves a slot pinned.
        for s in 0..self.inner.slot_count {
            if self.slot_state(s).load(Ordering::Acquire) != proto::STATE_WRITING {
                continue;
            }
            let pid = self.slot_producer_pid(s).load(Ordering::Relaxed);
            let token = self
                .au32(self.slot_off(s) + proto::S_PRODUCER_START_TOKEN)
                .load(Ordering::Relaxed);
            if pid != me && !shm::pid_alive_with_token(pid, token) {
                self.slot_mask(s).store(0, Ordering::Release);
                self.slot_readers(s).store(0, Ordering::Release);
                self.slot_state(s)
                    .store(proto::STATE_FREE, Ordering::Release);
                freed = true;
            }
        }
        if freed {
            futex::signal(self.au32(proto::G_RECLAIM_FUTEX));
        }
    }

    // -- handles ------------------------------------------------------------

    /// Claim the single-producer role. Fails if a live producer already holds it.
    pub fn producer(&self) -> Result<Producer> {
        let me = std::process::id();
        self.lock_registry();
        let held = self.au32(proto::G_PRODUCER_PID).load(Ordering::Relaxed);
        if held != 0 && held != me && shm::pid_alive(held) {
            self.unlock_registry();
            return Err(Error::Capacity("ring already has a live producer"));
        }
        self.au32(proto::G_PRODUCER_PID)
            .store(me, Ordering::Relaxed);
        self.inner.producers.fetch_add(1, Ordering::Relaxed);
        self.unlock_registry();
        Ok(Producer { ring: self.clone() })
    }

    pub fn consumer(&self) -> Result<Consumer> {
        self.lock_registry();
        self.reap_dead();
        let mut chosen = None;
        for i in 0..proto::MAX_CONSUMERS {
            if self
                .au32(self.entry(i, proto::R_STATE))
                .load(Ordering::Relaxed)
                == proto::REGISTRY_FREE
            {
                chosen = Some(i);
                break;
            }
        }
        let Some(index) = chosen else {
            self.unlock_registry();
            return Err(Error::Capacity("maximum consumer count reached"));
        };
        // Subscribe to future publications only, exactly as the reference does.
        let last = self
            .au64(proto::G_NEXT_SEQ)
            .load(Ordering::Relaxed)
            .wrapping_sub(1);
        let generation = self
            .au32(self.entry(index, proto::R_GENERATION))
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let me = std::process::id();
        self.au32(self.entry(index, proto::R_PID))
            .store(me, Ordering::Relaxed);
        // The start-time token distinguishes this process from a later one that
        // inherits the same pid after this one dies.
        self.au32(self.entry(index, proto::R_FLAGS))
            .store(shm::pid_start_token(me), Ordering::Relaxed);
        self.au64(self.entry(index, proto::R_HEARTBEAT_NS))
            .store(shm::now_ns(), Ordering::Relaxed);
        self.au64(self.entry(index, proto::R_LAST_SEQUENCE))
            .store(last, Ordering::Relaxed);
        self.slot_mask(0); // no-op touch keeps the mapping warm
        self.au32(self.entry(index, proto::R_STATE))
            .store(proto::REGISTRY_ACTIVE, Ordering::Release);
        let count = self.au32(proto::G_CONSUMERS).load(Ordering::Relaxed);
        self.au32(proto::G_CONSUMERS)
            .store(count + 1, Ordering::Relaxed);
        self.au32(proto::G_REGISTRY_EPOCH)
            .fetch_add(1, Ordering::Relaxed);
        self.unlock_registry();
        Ok(Consumer {
            ring: self.clone(),
            index,
            generation,
            last_sequence: Cell::new(last),
            closed: Cell::new(false),
        })
    }

    pub fn stats(&self) -> RingStats {
        let mut free = 0;
        let mut committed = 0;
        for i in 0..self.inner.slot_count {
            match self.slot_state(i).load(Ordering::Acquire) {
                proto::STATE_FREE => free += 1,
                proto::STATE_COMMITTED => committed += 1,
                _ => {}
            }
        }
        RingStats {
            slot_count: self.inner.slot_count,
            slot_capacity: self.inner.slot_capacity,
            next_sequence: self.au64(proto::G_NEXT_SEQ).load(Ordering::Relaxed),
            consumers: self.au32(proto::G_CONSUMERS).load(Ordering::Relaxed),
            published: self.au64(proto::G_PUBLISH_COUNT).load(Ordering::Relaxed),
            dropped: self.au64(proto::G_DROP_COUNT).load(Ordering::Relaxed),
            reaped: self.au64(proto::G_REAPED_COUNT).load(Ordering::Relaxed),
            free_slots: free,
            committed_slots: committed,
        }
    }

    /// Force a liveness sweep. Useful for tests and for a supervisor process.
    pub fn sweep(&self) {
        self.lock_registry();
        self.reap_dead();
        self.unlock_registry();
    }
}

// ---------------------------------------------------------------------------
// Producer
// ---------------------------------------------------------------------------

pub struct Producer {
    ring: Ring,
}

impl Drop for Producer {
    fn drop(&mut self) {
        let r = &self.ring;
        if r.inner.producers.fetch_sub(1, Ordering::AcqRel) != 1 {
            return;
        }
        // Last handle in this process: hand the role back so another process
        // can take over without waiting for this one to exit.
        let me = std::process::id();
        let _ = r.au32(proto::G_PRODUCER_PID).compare_exchange(
            me,
            0,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }
}

impl Producer {
    pub fn ring(&self) -> &Ring {
        &self.ring
    }

    /// Reserve a slot under the registry lock. Returns the slot index, the
    /// sequence assigned and the reader mask captured at reservation time.
    fn reserve_for_copy(&self, view: &TensorView<'_>) -> Option<(usize, u64, u64)> {
        self.reserve_header(
            view.dtype,
            view.ndim,
            &view.shape,
            &view.strides,
            view.data.len(),
            view.timestamp_ns,
        )
    }

    /// Reserve a slot and write its header, leaving the slot in `STATE_WRITING`
    /// with this publication's reader mask already set. The payload is *not*
    /// touched: the caller either copies into it (`publish`) or fills it in place
    /// (`reserve`). Must be called with the registry lock held.
    // Clippy would fold the inner `if`s below into match guards; keeping them in
    // the body keeps the two selection rules (prefer a free slot, else the oldest
    // unreferenced committed one) readable side by side.
    #[allow(clippy::too_many_arguments, clippy::collapsible_match)]
    fn reserve_header(
        &self,
        dtype: DType,
        ndim: usize,
        shape: &[u32; proto::MAX_NDIM],
        strides: &[u32; proto::MAX_NDIM],
        nbytes: usize,
        timestamp_ns: Option<u64>,
    ) -> Option<(usize, u64, u64)> {
        let r = &self.ring;
        let me = std::process::id();
        r.reap_dead();

        let mut free_slot: Option<usize> = None;
        let mut oldest: Option<(u64, usize)> = None;
        for i in 0..r.inner.slot_count {
            match r.slot_state(i).load(Ordering::Acquire) {
                proto::STATE_FREE => {
                    if free_slot.is_none() {
                        free_slot = Some(i);
                    }
                }
                proto::STATE_COMMITTED => {
                    if r.slot_mask(i).load(Ordering::Acquire) == 0 {
                        let seq = r.slot_seq(i).load(Ordering::Relaxed);
                        match oldest {
                            Some((best, _)) if !seq_newer(best, seq) => {}
                            _ => oldest = Some((seq, i)),
                        }
                    }
                }
                _ => {}
            }
        }
        let slot = free_slot.or(oldest.map(|(_, i)| i))?;

        let sequence = r.au64(proto::G_NEXT_SEQ).load(Ordering::Relaxed);
        let mask = r.active_mask();
        let off = r.slot_off(slot);

        r.slot_state(slot)
            .store(proto::STATE_WRITING, Ordering::Release);
        r.slot_producer_pid(slot).store(me, Ordering::Relaxed);
        r.au32(off + proto::S_PRODUCER_START_TOKEN)
            .store(shm::pid_start_token(me), Ordering::Relaxed);
        r.slot_seq(slot).store(sequence, Ordering::Relaxed);
        r.au64(off + proto::S_TIMESTAMP_NS)
            .store(timestamp_ns.unwrap_or_else(shm::now_ns), Ordering::Relaxed);
        r.au64(off + proto::S_NBYTES)
            .store(nbytes as u64, Ordering::Relaxed);
        unsafe {
            *r.base().add(off + proto::S_DTYPE) = dtype.code();
            *r.base().add(off + proto::S_NDIM) = ndim as u8;
        }
        r.au32(off + proto::S_FLAGS).store(0, Ordering::Relaxed);
        for d in 0..proto::MAX_NDIM {
            r.au32(off + proto::S_SHAPE + d * 4)
                .store(shape[d], Ordering::Relaxed);
            r.au32(off + proto::S_STRIDES + d * 4)
                .store(strides[d], Ordering::Relaxed);
        }
        r.slot_mask(slot).store(mask, Ordering::Release);
        r.slot_readers(slot)
            .store(mask.count_ones() as u16, Ordering::Release);
        r.au64(proto::G_NEXT_SEQ)
            .store(sequence.wrapping_add(1), Ordering::Relaxed);
        Some((slot, sequence, mask))
    }

    /// Phase 4B: reserve a slot and fill its payload in place, with no producer
    /// copy at all.
    ///
    /// This is the opt-in direct-write path. `publish` copies a tensor the caller
    /// already has; `reserve` is for a producer that can *generate* its tensor
    /// straight into the destination — a decoder writing its resized frame, for
    /// instance. Returns a [`SlotWriter`] holding the slot in `STATE_WRITING`.
    ///
    /// Ownership semantics are exactly the same as `publish`: the slot was chosen
    /// only if free or fully released, the reader mask was captured at
    /// reservation, and no consumer can observe the slot until `commit`. Dropping
    /// the writer without committing aborts the reservation and returns the slot,
    /// so a producer that fails midway cannot leak a slot or deliver a
    /// half-written tensor.
    ///
    /// The payload is **not** zeroed, because zeroing it would reintroduce the
    /// write this path exists to avoid. A caller that commits without filling
    /// every byte publishes whatever the slot's previous occupant left there.
    /// Debug builds poison the payload on reservation so a test catches that.
    pub fn reserve(
        &self,
        dtype: DType,
        shape: &[u32],
        policy: Backpressure,
        timeout: Duration,
    ) -> Result<Option<SlotWriter<'_>>> {
        let r = &self.ring;
        if shape.len() > proto::MAX_NDIM {
            return Err(Error::Invalid(format!(
                "ndim {} exceeds protocol max {}",
                shape.len(),
                proto::MAX_NDIM
            )));
        }
        let elements: u64 = shape.iter().map(|d| *d as u64).product();
        let nbytes = elements as usize * dtype.itemsize();
        if nbytes > r.inner.slot_capacity {
            return Err(Error::Invalid(format!(
                "tensor requires {nbytes} bytes; slot capacity is {}",
                r.inner.slot_capacity
            )));
        }
        let mut shape_arr = [0u32; proto::MAX_NDIM];
        let mut strides_arr = [0u32; proto::MAX_NDIM];
        shape_arr[..shape.len()].copy_from_slice(shape);
        let mut acc = dtype.itemsize() as u64;
        for i in (0..shape.len()).rev() {
            strides_arr[i] = acc as u32;
            acc *= shape[i] as u64;
        }

        let deadline = Instant::now() + timeout;
        loop {
            r.lock_registry();
            let reserved =
                self.reserve_header(dtype, shape.len(), &shape_arr, &strides_arr, nbytes, None);
            r.unlock_registry();

            if let Some((slot, sequence, mask)) = reserved {
                let ptr = unsafe { r.base().add(r.payload_off(slot)) };
                #[cfg(debug_assertions)]
                unsafe {
                    // Poison, so a producer that commits without filling the
                    // payload fails loudly in tests instead of shipping the
                    // previous occupant's bytes.
                    std::ptr::write_bytes(ptr, 0xA5, nbytes);
                }
                return Ok(Some(SlotWriter {
                    producer: self,
                    slot,
                    sequence,
                    mask,
                    nbytes,
                    ptr,
                    committed: false,
                }));
            }

            if policy == Backpressure::DropNewest {
                r.au64(proto::G_DROP_COUNT).fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::RingFull);
            }
            let word = r.au32(proto::G_RECLAIM_FUTEX);
            let observed = word.load(Ordering::Acquire);
            let remaining = (deadline - now).min(Duration::from_millis(5));
            futex::wait(word, observed, remaining);
        }
    }

    /// Fault-injection hook: reserve a slot and return without committing,
    /// leaving it in `STATE_WRITING` owned by this pid. Used by the
    /// producer-death tests to prove the ring recovers an abandoned slot.
    /// Not part of the supported publishing API.
    #[doc(hidden)]
    pub fn reserve_without_commit(&self, view: &TensorView<'_>) -> Result<usize> {
        let r = &self.ring;
        r.lock_registry();
        let reserved = self.reserve_for_copy(view);
        r.unlock_registry();
        reserved.map(|(slot, _, _)| slot).ok_or(Error::RingFull)
    }

    pub fn publish(
        &self,
        view: &TensorView<'_>,
        policy: Backpressure,
        timeout: Duration,
    ) -> Result<Option<PublishResult>> {
        let r = &self.ring;
        if view.data.len() > r.inner.slot_capacity {
            return Err(Error::Invalid(format!(
                "tensor requires {} bytes; slot capacity is {}",
                view.data.len(),
                r.inner.slot_capacity
            )));
        }
        let deadline = Instant::now() + timeout;
        loop {
            r.lock_registry();
            let reserved = self.reserve_for_copy(view);
            r.unlock_registry();

            if let Some((slot, sequence, mask)) = reserved {
                // The payload copy happens outside the lock; STATE_WRITING is
                // what protects the slot, and no consumer will read it.
                let dst = unsafe { r.base().add(r.payload_off(slot)) };
                unsafe {
                    std::ptr::copy_nonoverlapping(view.data.as_ptr(), dst, view.data.len());
                }
                r.slot_state(slot)
                    .store(proto::STATE_COMMITTED, COMMIT_RELEASE);
                r.au64(proto::G_PUBLISH_COUNT)
                    .fetch_add(1, Ordering::Relaxed);
                futex::signal(r.au32(proto::G_PUBLISH_FUTEX));
                return Ok(Some(PublishResult {
                    sequence,
                    slot_index: slot,
                    nbytes: view.data.len(),
                    readers: mask.count_ones(),
                }));
            }

            if policy == Backpressure::DropNewest {
                r.au64(proto::G_DROP_COUNT).fetch_add(1, Ordering::Relaxed);
                return Ok(None);
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(Error::RingFull);
            }
            let word = r.au32(proto::G_RECLAIM_FUTEX);
            let observed = word.load(Ordering::Acquire);
            let remaining = (deadline - now).min(Duration::from_millis(5));
            futex::wait(word, observed, remaining);
        }
    }
}

// ---------------------------------------------------------------------------
// Consumer + Lease
// ---------------------------------------------------------------------------

pub struct Consumer {
    ring: Ring,
    index: usize,
    generation: u32,
    last_sequence: Cell<u64>,
    closed: Cell<bool>,
}

impl Consumer {
    pub fn ring(&self) -> &Ring {
        &self.ring
    }
    pub fn slot_index(&self) -> usize {
        self.index
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence.get()
    }

    #[inline]
    fn bit(&self) -> u64 {
        1u64 << self.index
    }

    fn heartbeat(&self) {
        self.ring
            .au64(self.ring.entry(self.index, proto::R_HEARTBEAT_NS))
            .store(shm::now_ns(), Ordering::Relaxed);
    }

    /// One pass over the slots, returning the oldest publication addressed to
    /// this consumer that was observed as committed during the pass.
    fn scan_oldest(&self) -> Option<(u64, usize)> {
        let r = &self.ring;
        let bit = self.bit();
        let last = self.last_sequence.get();
        let mut best: Option<(u64, usize)> = None;
        for i in 0..r.inner.slot_count {
            if r.slot_state(i).load(Ordering::Acquire) != proto::STATE_COMMITTED {
                continue;
            }
            if r.slot_mask(i).load(Ordering::Acquire) & bit == 0 {
                continue;
            }
            let seq = r.slot_seq(i).load(Ordering::Acquire);
            // The reader bit stays set until the lease is released, so the bit
            // alone would let this consumer take the same publication twice
            // while its first lease is still outstanding. `last_sequence` is
            // what makes delivery once-only, exactly as in the reference.
            if !seq_newer(seq, last) {
                continue;
            }
            match best {
                Some((b, _)) if !seq_newer(b, seq) => {}
                _ => best = Some((seq, i)),
            }
        }
        best
    }

    fn try_take(&self) -> Option<TensorMeta> {
        let r = &self.ring;
        // A single pass is not a consistent snapshot: sequence N's commit can
        // become visible to us *after* we have already walked past its slot,
        // while N+1's commit is visible in the same pass. Taking N+1 there
        // would deliver out of order. The producer only ever adds newer
        // sequences, so re-scanning until two passes agree on the oldest
        // candidate converges, and it is bounded by the slot count.
        let mut best = self.scan_oldest()?;
        for _ in 0..r.inner.slot_count {
            match self.scan_oldest() {
                Some(next) if seq_newer(best.0, next.0) => best = next,
                _ => break,
            }
        }
        self.read_validated(best.1, best.0)
    }

    /// Read one slot's header as a consistent snapshot.
    ///
    /// A plain read is not safe even when the first `state` load says
    /// COMMITTED: that load can be stale while `sequence` and `readers_mask`
    /// already carry the *next* publication, whose payload the producer is
    /// still copying. Re-reading `state` and `sequence` after the header
    /// closes that window — if either moved, the snapshot is discarded. The
    /// successful re-read of `state == COMMITTED` with Acquire is also what
    /// orders the caller's payload reads against the producer's copy.
    fn read_validated(&self, slot: usize, expected_sequence: u64) -> Option<TensorMeta> {
        let r = &self.ring;
        let bit = self.bit();
        let off = r.slot_off(slot);

        if r.slot_state(slot).load(COMMIT_ACQUIRE) != proto::STATE_COMMITTED {
            return None;
        }
        if r.slot_seq(slot).load(Ordering::Acquire) != expected_sequence {
            return None;
        }
        let ndim = unsafe { std::ptr::read_volatile(r.base().add(off + proto::S_NDIM)) } as usize;
        let dtype_code = unsafe { std::ptr::read_volatile(r.base().add(off + proto::S_DTYPE)) };
        let nbytes = r.au64(off + proto::S_NBYTES).load(Ordering::Relaxed) as usize;
        let timestamp_ns = r.au64(off + proto::S_TIMESTAMP_NS).load(Ordering::Relaxed);
        let mut shape = [0u32; proto::MAX_NDIM];
        let mut strides = [0u32; proto::MAX_NDIM];
        for d in 0..proto::MAX_NDIM {
            shape[d] = r.au32(off + proto::S_SHAPE + d * 4).load(Ordering::Relaxed);
            strides[d] = r
                .au32(off + proto::S_STRIDES + d * 4)
                .load(Ordering::Relaxed);
        }
        let mask = r.slot_mask(slot).load(Ordering::Acquire);

        // Validation pass. Everything above is discarded unless the slot is
        // still committed, still carrying this sequence, and still addressed
        // to this consumer.
        if r.slot_state(slot).load(COMMIT_ACQUIRE) != proto::STATE_COMMITTED {
            return None;
        }
        if r.slot_seq(slot).load(Ordering::Acquire) != expected_sequence {
            return None;
        }
        if mask & bit == 0 || r.slot_mask(slot).load(Ordering::Acquire) & bit == 0 {
            return None;
        }
        if ndim > proto::MAX_NDIM || nbytes > r.inner.slot_capacity {
            return None;
        }
        let dtype = DType::from_code(dtype_code)?;

        Some(TensorMeta {
            sequence: expected_sequence,
            timestamp_ns,
            nbytes,
            dtype,
            ndim,
            shape,
            strides,
            slot_index: slot,
            readers_remaining: mask.count_ones(),
        })
    }

    /// Block until the next publication addressed to this consumer arrives.
    pub fn next(&self, timeout: Duration) -> Result<Lease<'_>> {
        let (meta, ptr, len) = self.next_raw(timeout)?;
        let data = unsafe { std::slice::from_raw_parts(ptr, len) };
        Ok(Lease {
            consumer: self,
            meta,
            data,
            released: false,
        })
    }

    /// Lease-taking without the borrow, for foreign-function bindings that
    /// cannot express a Rust lifetime. The caller becomes responsible for
    /// calling [`Consumer::release_raw`] exactly once with the returned meta,
    /// and must not read the payload afterwards. Prefer [`Consumer::next`] in
    /// Rust code, where the borrow makes that a compile-time guarantee.
    pub fn next_raw(&self, timeout: Duration) -> Result<(TensorMeta, *const u8, usize)> {
        let r = &self.ring;
        let deadline = Instant::now() + timeout;
        loop {
            let word = r.au32(proto::G_PUBLISH_FUTEX);
            let observed = word.load(Ordering::Acquire);
            if let Some(meta) = self.try_take() {
                self.last_sequence.set(meta.sequence);
                r.au64(r.entry(self.index, proto::R_LAST_SEQUENCE))
                    .store(meta.sequence, Ordering::Relaxed);
                self.heartbeat();
                let ptr = unsafe { r.base().add(r.payload_off(meta.slot_index)) as *const u8 };
                return Ok((meta, ptr, meta.nbytes));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(Error::Timeout);
            }
            let remaining = (deadline - now).min(Duration::from_millis(5));
            futex::wait(word, observed, remaining);
        }
    }

    /// Counterpart to [`Consumer::next_raw`].
    pub fn release_raw(&self, meta: &TensorMeta) -> Result<()> {
        self.release(meta)
    }

    fn release(&self, meta: &TensorMeta) -> Result<()> {
        let r = &self.ring;
        let slot = meta.slot_index;
        if r.slot_seq(slot).load(Ordering::Acquire) != meta.sequence {
            return Err(Error::LeaseViolation(format!(
                "slot {slot} was recycled while sequence {} was still leased",
                meta.sequence
            )));
        }
        let bit = self.bit();
        let previous = r.slot_mask(slot).fetch_and(!bit, Ordering::AcqRel);
        if previous & bit == 0 {
            return Err(Error::LeaseViolation(
                "lease released more than once".into(),
            ));
        }
        let now = previous & !bit;
        r.slot_readers(slot)
            .store(now.count_ones() as u16, Ordering::Release);
        self.heartbeat();
        if now == 0 {
            futex::signal(r.au32(proto::G_RECLAIM_FUTEX));
        }
        Ok(())
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        let r = &self.ring;
        r.lock_registry();
        if r.drop_consumer_bit(self.index) {
            futex::signal(r.au32(proto::G_RECLAIM_FUTEX));
        }
        r.au32(r.entry(self.index, proto::R_STATE))
            .store(proto::REGISTRY_FREE, Ordering::Release);
        let count = r.au32(proto::G_CONSUMERS).load(Ordering::Relaxed);
        r.au32(proto::G_CONSUMERS)
            .store(count.saturating_sub(1), Ordering::Relaxed);
        r.au32(proto::G_REGISTRY_EPOCH)
            .fetch_add(1, Ordering::Relaxed);
        r.unlock_registry();
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.close();
    }
}

/// A borrowed window onto one committed slot.
///
/// The borrow of `Consumer` is what makes "a view cannot outlive its lease"
/// a compile-time property rather than a convention.
pub struct Lease<'a> {
    consumer: &'a Consumer,
    meta: TensorMeta,
    data: &'a [u8],
    released: bool,
}

impl<'a> Lease<'a> {
    pub fn meta(&self) -> &TensorMeta {
        &self.meta
    }

    /// Zero-copy view of the payload inside the shared slab.
    pub fn data(&self) -> &[u8] {
        self.data
    }

    pub fn sequence(&self) -> u64 {
        self.meta.sequence
    }

    /// Reinterpret the payload as a slice of `T`. The caller is responsible for
    /// matching `T` to `meta().dtype`.
    ///
    /// # Safety
    /// `T` must be a plain-old-data type whose size divides the payload length
    /// and whose alignment is satisfied by the 64-byte-aligned slot payload.
    pub unsafe fn as_slice<T: Copy>(&self) -> &[T] {
        unsafe {
            std::slice::from_raw_parts(
                self.data.as_ptr() as *const T,
                self.data.len() / std::mem::size_of::<T>(),
            )
        }
    }

    pub fn release(mut self) -> Result<()> {
        self.released = true;
        self.consumer.release(&self.meta)
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.consumer.release(&self.meta);
        }
    }
}

// ---------------------------------------------------------------------------
// SlotWriter: the direct-write (zero producer copy) path
// ---------------------------------------------------------------------------

/// Exclusive write access to a reserved slot's payload.
///
/// The slot stays in `STATE_WRITING` for the whole lifetime of this value, so no
/// consumer can observe it. Committing makes it visible with a release store;
/// dropping it without committing returns the slot to the ring.
pub struct SlotWriter<'a> {
    producer: &'a Producer,
    slot: usize,
    sequence: u64,
    mask: u64,
    nbytes: usize,
    ptr: *mut u8,
    committed: bool,
}

impl<'a> SlotWriter<'a> {
    /// The slot's payload, to be filled in place. Exactly `nbytes` long and
    /// 64-byte aligned.
    pub fn payload(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.nbytes) }
    }

    /// The payload as a typed slice.
    ///
    /// # Safety
    /// `T` must be plain-old-data whose size divides the payload length and
    /// whose alignment is satisfied by a 64-byte-aligned address.
    pub unsafe fn payload_as<T: Copy>(&mut self) -> &mut [T] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.ptr as *mut T,
                self.nbytes / std::mem::size_of::<T>(),
            )
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn slot_index(&self) -> usize {
        self.slot
    }

    pub fn nbytes(&self) -> usize {
        self.nbytes
    }

    pub fn readers(&self) -> u32 {
        self.mask.count_ones()
    }

    /// Record the source's own capture time rather than the time of commit.
    pub fn set_timestamp_ns(&mut self, timestamp_ns: u64) {
        let r = &self.producer.ring;
        r.au64(r.slot_off(self.slot) + proto::S_TIMESTAMP_NS)
            .store(timestamp_ns, Ordering::Relaxed);
    }

    /// Publish the slot. The release store is what orders every byte written
    /// through `payload()` against the consumers' acquire load.
    pub fn commit(mut self) -> PublishResult {
        let r = &self.producer.ring;
        self.committed = true;
        r.slot_state(self.slot)
            .store(proto::STATE_COMMITTED, COMMIT_RELEASE);
        r.au64(proto::G_PUBLISH_COUNT)
            .fetch_add(1, Ordering::Relaxed);
        futex::signal(r.au32(proto::G_PUBLISH_FUTEX));
        PublishResult {
            sequence: self.sequence,
            slot_index: self.slot,
            nbytes: self.nbytes,
            readers: self.mask.count_ones(),
        }
    }

    /// Give the slot back without publishing. No consumer ever saw it, and the
    /// sequence number is returned to the counter so delivered sequences stay
    /// contiguous.
    pub fn abort(mut self) {
        self.committed = true;
        self.release_slot();
    }

    fn release_slot(&mut self) {
        let r = &self.producer.ring;
        r.lock_registry();
        r.slot_mask(self.slot).store(0, Ordering::Release);
        r.slot_readers(self.slot).store(0, Ordering::Release);
        r.slot_state(self.slot)
            .store(proto::STATE_FREE, Ordering::Release);
        // Single producer, and no publication can have happened since this
        // reservation, so the sequence counter can be handed back.
        let _ = r.au64(proto::G_NEXT_SEQ).compare_exchange(
            self.sequence.wrapping_add(1),
            self.sequence,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        r.unlock_registry();
        futex::signal(r.au32(proto::G_RECLAIM_FUTEX));
    }
}

impl Drop for SlotWriter<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.release_slot();
        }
    }
}
