# TenzorBus v0.1 protocol

TenzorBus is a **single-producer, multi-consumer tensor ring** backed by shared memory.

There are two implementations of this protocol in the tree, and they share a byte layout:

| | `src/tenzorbus/` (Python) | `rust/tenzorbus/` (Rust) |
|---|---|---|
| role | executed v0.1 reference, semantic authority | Linux production transport |
| coordination | `/tmp` file lock + polling | in-mapping atomics + futex wait/wake |
| liveness | none | consumer registry with pid + generation, crash recovery |
| status | frozen; do not "improve" it | the one that ships |

The layout is frozen at the Phase 1 gate and checked by
`rust/tenzor-core/tests/layout_parity.rs`, which reads the constants out of the
actual `protocol.py`, and by `rust/tenzorbus/tests/golden_bytes.rs`, which makes
each implementation's real producer write an image the other one parses.

## Memory layout

- 4 KiB global header
- N fixed-capacity slots
- each slot has a 128-byte metadata header followed by a 64-byte-aligned payload region
- max rank: 8 dimensions in v0.1
- tensor metadata: dtype, shape, byte strides, sequence id, capture timestamp

### Global header

| offset | size | field | written by |
|---|---|---|---|
| 0 | 8 | magic `TZBUS001` | both |
| 8 | 4 | version (1) | both |
| 12 | 4 | slot count | both |
| 16 | 8 | slot capacity | both |
| 24 | 8 | next sequence | both |
| 32 | 4 | registered consumers | both |
| 36 | 4 | max consumers | both |
| 40 | 8 | publish count | both |
| 48 | 8 | drop count | both |
| 56 | 4 | publish futex word | Rust |
| 60 | 4 | reclaim futex word | Rust |
| 64 | 4 | registry lock | Rust |
| 68 | 4 | producer pid | Rust |
| 72 | 4 | registry epoch | Rust |
| 76 | 4 | registry lock owner pid | Rust |
| 80 | 8 | reaped-consumer count | Rust |
| 88 | 4 | producer pid start-time token | Rust |
| 92 | 4 | registry-lock owner start-time token | Rust |
| 128 | 64 × 32 | consumer registration table | Rust |

Everything from offset 56 on was zero-filled and unread by the v0.1 reference,
so a ring created by Python presents an empty registry to Rust rather than
garbage. `golden_bytes.rs` asserts exactly that.

Each 32-byte registry entry is `{ state: u32, pid: u32, generation: u32,
start_token: u32, heartbeat_ns: u64, last_sequence: u64 }`, where `start_token` is
the low 32 bits of the pid's process start time from `/proc/<pid>/stat`.

### Slot header

| offset | size | field | written by |
|---|---|---|---|
| 0 | 4 | magic `SLOT` | both |
| 4 | 4 | state (0 free / 1 committed / 2 writing) | both |
| 8 | 8 | sequence | both |
| 16 | 8 | timestamp (ns) | both |
| 24 | 8 | payload bytes | both |
| 32 | 1 | dtype code | both |
| 33 | 1 | ndim | both |
| 34 | 2 | readers remaining | both |
| 36 | 4 | flags | both |
| 40 | 32 | shape `[u32; 8]` | both |
| 72 | 32 | byte strides `[u32; 8]` | both |
| 104 | 4 | producer pid | both |
| 112 | 8 | reader bitmask | Rust |
| 120 | 4 | producer pid start-time token | Rust |

The reader bitmask is the Rust transport's addition, in the reference's reserved
tail. Bit *i* means registry entry *i* still holds a lease on this slot.
`readers remaining` is maintained as the mask's popcount, so the reference's own
view of the field stays truthful.

## Ownership contract

1. Producer reserves a reclaimable slot (free, or committed with no readers).
2. Producer copies the source tensor once into the shared slab and commits it.
3. The slot records which consumers were registered at commit time.
4. Every one of those consumers obtains a view into that same slot; no payload
   copy is needed to read it.
5. Each consumer releases its lease exactly once; the final release makes the
   slot reclaimable.
6. The producer never overwrites a slot with active readers.
7. A publication is delivered to a given consumer at most once.

**Lifetime rule:** a NumPy/PyTorch view must not outlive its lease. In Rust the
lease borrows the consumer, so this is a compile-time property. In the Python
bindings a view holds a buffer export and `release()` refuses while one is
outstanding — use `lease.copy()` to keep data past the lease.

## Concurrency rules (Rust transport)

* Slot state transitions are atomic. A commit is `store(COMMITTED, Release)`
  after the payload copy; a consumer's `load(Acquire) == COMMITTED` is what
  orders its payload reads against that copy.
* **A consumer's read of `state`, `sequence` and `readers_mask` is not a
  consistent snapshot on its own.** A stale `COMMITTED` can pair with the *next*
  publication's sequence while the producer is still copying. Consumers
  therefore re-read `state` and `sequence` after the header and discard the
  snapshot if either moved. Skipping this validation produced roughly one torn
  payload per two million publications under slot pressure.
* A single slot-scan pass is likewise not ordered: a commit that lands mid-scan
  can be seen while an older one is not. Consumers rescan until two passes agree
  on the oldest candidate.
* The reader bit stays set until release, so `last_sequence` — not the bit — is
  what makes delivery once-only.
* Sequence comparison is wrapping (`seq_newer`), so the u64 counter may wrap
  without breaking ordering.

## Failure modes and recovery

| fault | recovery |
|---|---|
| consumer killed holding a lease | producer's liveness sweep clears its bit from every slot, frees its registry entry, bumps its generation, counts it in `reaped` |
| consumer exits cleanly | same, on `Drop` |
| producer killed between reserve and commit | the slot is `WRITING` with a dead pid; the next sweep returns it to `FREE` |
| **a dead consumer that is the producer's own child** | a zombie is treated as dead: `kill(pid, 0)` reports a zombie as alive, so liveness reads `/proc/<pid>/stat` and treats state `Z`/`X` as dead. A zombie has been reaped by the kernel and can never execute again, so it can never release a lease |
| a new process inheriting a dead consumer's pid | the recorded start-time token no longer matches, so the original is treated as dead |
| producer killed holding the registry lock | the lock records its owner pid; a waiter whose owner is dead steals the lock |
| producer exits cleanly | the producer role is released, so another process can claim it |
| registry full (64 consumers) | registration fails with a capacity error; entries are reusable |

Liveness is decided by the pid's existence and process state, not by heartbeat age,
so a legitimately slow consumer is never reaped. Heartbeats are recorded for
observability only.

## Timestamps

The slot's `timestamp_ns` is the **source's** capture time, not the time of publish.
A media producer must set it (`TensorView::with_timestamp_ns`,
`SlotWriter::set_timestamp_ns`, or `timestamp_ns=` in Python); left unset, the ring
stamps wall-clock time at publish, which a consumer cannot use to align a frame with
its clip.

A timestamp of **0 is a legitimate value** — it is what the first epoch of every clip
carries — and must never be treated as "unset". Both implementations have regression
tests for exactly that.

## Two publish paths

| | `publish` (copy) | `reserve` (direct write) |
|---|---|---|
| for | a tensor the caller already holds | a producer that can generate into the destination |
| producer copies | 1 | 0 |
| consumer copies | 0 | 0 |
| API | `Producer::publish(&TensorView, policy, timeout)` | `Producer::reserve(dtype, shape, policy, timeout) -> SlotWriter` |

Both reserve a slot the same way, set the same reader mask, and publish with the same
release store, so a consumer cannot tell them apart. A `SlotWriter` holds the slot in
`STATE_WRITING` until `commit()`; `abort()` or dropping it returns the slot **and the
sequence number**, so delivered sequences stay contiguous either way.

The reserved payload is **not** zeroed, because zeroing it would reintroduce the
write this path exists to avoid. A producer that commits without filling every byte
publishes whatever the slot's previous occupant left. Debug builds poison a freshly
reserved payload (`0xA5`) so a test catches that.

## What "zero-copy" means

Consumer fan-out is always zero-copy: the tests assert the consumer's NumPy/PyTorch
data pointer equals the slot's payload address.

On the producer side, `publish` performs **one** copy and `reserve` performs **none**
— but only for a producer that *generates* into the slot. Filling a reserved slot
from an array you already hold is still one copy; direct-write cannot remove a copy
of data that already exists elsewhere. See `docs/TENZORPIPE_DIRECT_WRITE.md`.

## Backpressure

- `block`: wait for a reclaimable slot, bounded by the publish timeout. The
  producer futex-waits on the reclaim word rather than spinning.
- `drop_newest`: leave active readers untouched, count a dropped publication,
  return `None`.

A dropped publication **does not consume a sequence number**. Delivered
sequences are contiguous under both policies, so a consumer cannot infer drops
from sequence gaps — read `dropped` from the ring stats instead. The Live Lab
must follow that rule when it reports drop rate.

## Remaining work

- Windows named file mappings + `WaitOnAddress`; macOS POSIX shm + native waits
- stable C ABI and cross-compiler ABI tests
- direct TenzorPipe slot writer (Phase 4 stage 2)
- fair benchmark matrix against Unix sockets / HTTP binary / HTTP JSON (Phase 5)
