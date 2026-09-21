# Phase 4B: what TenzorPipe needs so the producer copy disappears

The TenzorBus half of Phase 4B is **done**: `Producer::reserve` hands a cooperating
producer the slot itself, and `rust/tenzorbus/tests/direct_write.rs` proves the
producer and the consumer use one buffer with every ownership rule intact. The
Python bindings expose it too (`producer.reserve(...)`).

What is **not** done is using it from the media path, because TenzorPipe v0.3.2
cannot hand its tensor to anything but its own file writer. This document is the
exact change that would close that, written against the v0.3.2 source so it can be
reviewed rather than guessed at.

## Where the copy actually is today

The integration reads a `.tenzor` file that TenzorPipe has already written, so the
tensor exists in a memory-mapped Arrow buffer and the transport copies it once into
the slot. **That copy is unavoidable on the file path**, and direct-write cannot
remove it: the bytes already live somewhere else. Removing it requires TenzorPipe to
write its resized frame into the slot in the first place, which is a live path
v0.3.2 does not have.

This is worth being blunt about, because it is easy to overclaim: filling a
reserved slot from an array you already hold is still one copy. Direct-write pays
off only when the producer *generates* into the slot. The benchmark matrix reports
both cases separately for that reason (`tenzorbus_direct` vs
`tenzorbus_direct_nofill`).

## The good news: the hook already exists

v0.3.2's engine is already structured for this.

`src/video.rs:432` — the resize allocates its own output and returns it:

```rust
pub(crate) fn yuv420_to_resized_chw(
    y: &[u8], u: &[u8], v: &[u8],
    source_size: (usize, usize),
    destination_size: (usize, usize),
    color: Color,
) -> Vec<f32> {
    let mut out = vec![0.0f32; 3 * dst_w * dst_h];
    ...
}
```

`src/video.rs:291` — and there is already a per-epoch callback, threaded from
`cli.rs` through both the H.264 and the HEVC decoders:

```rust
let chw = yuv420_to_resized_chw(frame.y(), frame.u(), frame.v(), ...);
while epoch < end {
    on_epoch(epoch, pts, &chw)?;
    epoch += 1;
}
```

So the engine already produces one finished CHW tensor per selected picture and
already announces it per epoch. It just allocates the destination itself and lends
it out as `&[f32]`.

## The change

Three scoped, backward-compatible steps. The `.tenzor` writer, the schema, the metadata, the
error texts and every output byte stay exactly as they are — which matters, because
it means the 1,056-case byte-identity matrix still covers the default path
unchanged.

### 1. Split the resize so it can write into a caller's buffer

```rust
/// Resize YUV420 into normalized CHW RGB inside `out`, which must be
/// `3 * dst_w * dst_h` long.
pub(crate) fn yuv420_to_resized_chw_into(
    out: &mut [f32],
    y: &[u8], u: &[u8], v: &[u8],
    source_size: (usize, usize),
    destination_size: (usize, usize),
    color: Color,
);

/// Unchanged signature, now a thin wrapper.
pub(crate) fn yuv420_to_resized_chw(...) -> Vec<f32> {
    let mut out = vec![0.0f32; 3 * dst_w * dst_h];
    yuv420_to_resized_chw_into(&mut out, y, u, v, source_size, destination_size, color);
    out
}
```

A pure refactor: the wrapper produces the same bytes by construction, so no
existing test or recorded digest can move. `scripts/test_idct8x8_simd.sh` and the
identity matrix are the check.

### 2. Give the epoch sink a chance to supply the destination

```rust
/// Where a converted epoch's video tensor goes.
pub trait EpochSink {
    /// Called before the picture for this epoch is resized. Return `Some(buf)`
    /// to have the engine write the CHW float32 tensor straight into `buf`
    /// (exactly `3 * resolution * resolution` floats). Return `None` for the
    /// existing behaviour, which is what the file writer does.
    fn video_destination(&mut self, epoch: usize, timestamp_ms: i64) -> Option<&mut [f32]>;

    /// The tensors for this epoch are complete and may be published.
    fn commit_epoch(&mut self, epoch: usize, timestamp_ms: i64) -> anyhow::Result<()>;

    /// The engine failed after handing out a destination; drop it unpublished.
    fn abort_epoch(&mut self, epoch: usize) {}
}

/// Scoped compatibility entry point. `convert` keeps its exact signature and behaviour and
/// becomes `convert_with_sink` driven by the existing file-writing sink.
pub fn convert_with_sink(cli: &Cli, sink: &mut dyn EpochSink) -> anyhow::Result<Summary>;
```

### 3. TenzorBus implements it

```rust
impl EpochSink for BusSink<'_> {
    fn video_destination(&mut self, _epoch: usize, timestamp_ms: i64) -> Option<&mut [f32]> {
        let mut writer = self.producer.reserve(
            DType::F32, &[3, self.res, self.res], Backpressure::Block, self.timeout,
        ).ok()??;
        writer.set_timestamp_ns(timestamp_ms as u64 * 1_000_000);
        self.pending = Some(writer);
        Some(unsafe { self.pending.as_mut()?.payload_as::<f32>() })
    }

    fn commit_epoch(&mut self, _epoch: usize, _timestamp_ms: i64) -> anyhow::Result<()> {
        if let Some(writer) = self.pending.take() { writer.commit(); }
        Ok(())
    }

    fn abort_epoch(&mut self, _epoch: usize) {
        if let Some(writer) = self.pending.take() { writer.abort(); }
    }
}
```

The decoder then writes its resized frame into the shared slot, and the consumers
read those same bytes. Copies on the whole path: **zero**.

## One case that still costs a copy, and why

At `video.rs:288-293` a single resized picture can satisfy **several** epochs:

```rust
while epoch < end {
    on_epoch(epoch, pts, &chw)?;
    epoch += 1;
}
```

Each epoch needs its own slot, so the engine can resize into the first epoch's slot
and must then copy into each additional one. Direct-write therefore eliminates the
copy for the one-picture-one-epoch case — which is the ordinary case at 30 fps with
0.5 s windows — and costs one copy per *repeated* epoch, which arises when the
frame rate is low relative to the window. Worth stating rather than discovering
later.

## Risk

* No change to the `.tenzor` writer, the Arrow schema, the metadata or any error
  text, so the recorded regression evidence still applies to the default path.
* The one behavioural risk is the refactor in step 1; it is guarded by the existing
  byte-identity matrix, which should be re-run (`--run-matrix`) rather than trusted.
* `EpochSink::video_destination` hands out a `&mut [f32]` into shared memory. The
  slot is in `STATE_WRITING` for that whole window, so no consumer can observe it;
  TenzorBus already tests exactly that (`a_reserved_slot_is_invisible_until_commit`).
* A producer that commits without filling the payload publishes whatever the slot's
  previous occupant left. TenzorBus poisons a freshly reserved payload in debug
  builds so a test catches it; the engine should use a debug build in CI for the
  direct-write path.
