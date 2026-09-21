//! Phase 1 gate: golden bytes in both directions.
//!
//! * Rust's real producer writes a ring image; the Python reference parser
//!   reads every field back out of it and checks the payload.
//! * The Python reference's real producer writes a ring image; Rust parses it
//!   with the `tenzor-core` offsets and checks the payload.
//!
//! Neither side is allowed a hand-written copy of the other's table.

use std::io::Write;
use std::process::Command;
use std::time::Duration;

use tenzor_core as proto;
use tenzorbus::{Backpressure, DType, Ring, RingOptions, TensorView};

fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn run_python(script: &str, args: &[&str]) -> std::process::Output {
    let root = repo_root();
    Command::new("python3")
        .arg("-c")
        .arg(script)
        .args(args)
        .env("PYTHONPATH", root.join("src"))
        .output()
        .expect("python3 must be available")
}

fn unique(tag: &str) -> String {
    format!(
        "golden_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    )
}

const PY_PARSE: &str = r#"
import sys, struct
import tenzorbus.protocol as p

path = sys.argv[1]
buf = bytearray(open(path, "rb").read())

assert bytes(buf[p.G_MAGIC:p.G_MAGIC+8]) == p.MAGIC, "global magic"
assert p.read_u32(buf, p.G_VERSION) == p.VERSION, "version"
slot_count = p.read_u32(buf, p.G_SLOT_COUNT)
slot_capacity = p.read_u64(buf, p.G_SLOT_CAPACITY)
assert slot_count == 4, f"slot_count {slot_count}"
assert slot_capacity == 4096, f"slot_capacity {slot_capacity}"
assert p.read_u64(buf, p.G_PUBLISH_COUNT) == 1, "publish count"
assert p.read_u64(buf, p.G_NEXT_SEQ) == 2, "next seq"

base = p.slot_base(0, slot_capacity)
assert bytes(buf[base:base+4]) == p.SLOT_MAGIC, "slot magic"
assert p.read_u32(buf, base + p.S_STATE) == p.STATE_COMMITTED, "state"
assert p.read_u64(buf, base + p.S_SEQUENCE) == 1, "sequence"
assert p.read_u64(buf, base + p.S_NBYTES) == 24, "nbytes"
assert buf[base + p.S_DTYPE] == p.DTYPE_TO_CODE["int32"], "dtype"
assert buf[base + p.S_NDIM] == 2, "ndim"
assert p.read_shape(buf, base + p.S_SHAPE, 2) == (2, 3), "shape"
assert p.read_shape(buf, base + p.S_STRIDES, 2) == (12, 4), "strides"
assert p.read_u64(buf, base + p.S_TIMESTAMP_NS) > 0, "timestamp"
assert p.read_u32(buf, base + p.S_PRODUCER_PID) > 0, "producer pid"

payload = p.payload_base(0, slot_capacity)
values = struct.unpack_from("<6i", buf, payload)
assert values == (10, 20, 30, 40, 50, 60), f"payload {values}"
print("PY_PARSE_OK")
"#;

const PY_WRITE: &str = r#"
import sys, shutil
import numpy as np
from tenzorbus.ring import SharedTensorRing

name = sys.argv[1]
out = sys.argv[2]
ring = SharedTensorRing.create(name, slot_count=4, slot_capacity=4096, force=True)
arr = np.array([[1.5, 2.5, 3.5], [4.5, 5.5, 6.5]], dtype=np.float32)
ring.publish(arr, timestamp_ns=123456789)
shutil.copyfile(f"/dev/shm/tzbus_{name}", out)
ring.close(unlink=True)
print("PY_WRITE_OK")
"#;

#[test]
fn python_reference_parses_a_rust_written_image() {
    let name = unique("r2p");
    let ring = Ring::create(
        &name,
        RingOptions {
            slot_count: 4,
            slot_capacity: 4096,
            force: true,
        },
    )
    .expect("create ring");
    let producer = ring.producer().expect("producer");

    let values: [i32; 6] = [10, 20, 30, 40, 50, 60];
    let bytes: &[u8] = unsafe {
        std::slice::from_raw_parts(values.as_ptr() as *const u8, std::mem::size_of_val(&values))
    };
    let view = TensorView::contiguous(DType::I32, &[2, 3], bytes).expect("view");
    producer
        .publish(&view, Backpressure::Block, Duration::from_secs(1))
        .expect("publish")
        .expect("slot");

    let image = ring.snapshot();
    let path = std::env::temp_dir().join(format!("{name}.bin"));
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&image)
        .unwrap();

    let out = run_python(PY_PARSE, &[path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    drop(producer);
    drop(ring);

    assert!(
        out.status.success(),
        "python reference rejected the Rust-written image:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("PY_PARSE_OK"));
}

#[test]
fn rust_parses_a_python_reference_written_image() {
    let name = unique("p2r");
    let path = std::env::temp_dir().join(format!("{name}.bin"));
    let out = run_python(PY_WRITE, &[&name, path.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "python reference failed to write an image:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let buf = std::fs::read(&path).expect("image");
    let _ = std::fs::remove_file(&path);

    let rd_u32 = |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
    let rd_u64 = |off: usize| u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());

    assert_eq!(
        &buf[proto::G_MAGIC..proto::G_MAGIC + 8],
        &proto::PROTOCOL_MAGIC
    );
    assert_eq!(rd_u32(proto::G_VERSION), proto::PROTOCOL_VERSION);
    let slot_count = rd_u32(proto::G_SLOT_COUNT) as usize;
    let slot_capacity = rd_u64(proto::G_SLOT_CAPACITY) as usize;
    assert_eq!(slot_count, 4);
    assert_eq!(slot_capacity, 4096);
    assert_eq!(rd_u64(proto::G_NEXT_SEQ), 2);
    assert_eq!(rd_u64(proto::G_PUBLISH_COUNT), 1);

    let base = proto::slot_base(0, slot_capacity);
    assert_eq!(&buf[base..base + 4], &proto::SLOT_MAGIC);
    assert_eq!(rd_u32(base + proto::S_STATE), proto::STATE_COMMITTED);
    assert_eq!(rd_u64(base + proto::S_SEQUENCE), 1);
    assert_eq!(rd_u64(base + proto::S_TIMESTAMP_NS), 123_456_789);
    assert_eq!(rd_u64(base + proto::S_NBYTES), 24);
    assert_eq!(
        DType::from_code(buf[base + proto::S_DTYPE]),
        Some(DType::F32)
    );
    assert_eq!(buf[base + proto::S_NDIM], 2);
    assert_eq!(rd_u32(base + proto::S_SHAPE), 2);
    assert_eq!(rd_u32(base + proto::S_SHAPE + 4), 3);
    assert_eq!(rd_u32(base + proto::S_STRIDES), 12);
    assert_eq!(rd_u32(base + proto::S_STRIDES + 4), 4);

    let payload = proto::payload_base(0, slot_capacity);
    let expected = [1.5f32, 2.5, 3.5, 4.5, 5.5, 6.5];
    for (i, want) in expected.iter().enumerate() {
        let off = payload + i * 4;
        let got = f32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        assert_eq!(got, *want, "payload element {i}");
    }

    // The bytes the Rust transport claims must be untouched zeros in an image
    // the reference produced. This is what makes the extension safe.
    assert_eq!(rd_u64(proto::G_REAPED_COUNT), 0);
    for i in 0..proto::MAX_CONSUMERS {
        assert_eq!(
            rd_u32(proto::registry_entry_offset(i) + proto::R_STATE),
            proto::REGISTRY_FREE,
            "registry entry {i} is not zero in a reference-written image"
        );
    }
    assert_eq!(rd_u64(base + proto::S_READERS_MASK), 0);
}
