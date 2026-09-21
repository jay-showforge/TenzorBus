//! Phase 1 gate: the Rust layout constants must equal the executed Python
//! reference's constants, field for field. This test shells out to the actual
//! `src/tenzorbus/protocol.py` rather than to a copied table, so the two can
//! never drift silently.

use std::process::Command;

use tenzor_core as proto;

fn python_constants() -> Vec<(String, i128)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root");
    let script = r#"
import json, sys
import tenzorbus.protocol as p
names = [
 "VERSION","GLOBAL_HEADER_SIZE","SLOT_HEADER_SIZE","MAX_NDIM",
 "STATE_FREE","STATE_COMMITTED","STATE_WRITING",
 "G_MAGIC","G_VERSION","G_SLOT_COUNT","G_SLOT_CAPACITY","G_NEXT_SEQ",
 "G_CONSUMERS","G_MAX_CONSUMERS","G_PUBLISH_COUNT","G_DROP_COUNT",
 "S_MAGIC","S_STATE","S_SEQUENCE","S_TIMESTAMP_NS","S_NBYTES","S_DTYPE",
 "S_NDIM","S_READERS","S_FLAGS","S_SHAPE","S_STRIDES","S_PRODUCER_PID",
]
out = {n: getattr(p, n) for n in names}
out["MAGIC"] = list(p.MAGIC)
out["SLOT_MAGIC"] = list(p.SLOT_MAGIC)
out["DTYPES"] = p.DTYPE_TO_CODE
out["stride_602112"] = p.slot_stride(602112)
out["total_8x1MiB"] = p.total_bytes(8, 1 << 20)
out["slot_base_3"] = p.slot_base(3, 602112)
out["payload_base_3"] = p.payload_base(3, 602112)
json.dump(out, sys.stdout)
"#;
    // The reference implementation is the Python one, so this gate needs an
    // interpreter that can import it -- which means numpy. Set TENZORBUS_PYTHON
    // to point at a virtualenv; otherwise the system python3 is used.
    let interpreter = std::env::var("TENZORBUS_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let output = Command::new(&interpreter)
        .arg("-c")
        .arg(script)
        .env("PYTHONPATH", root.join("src"))
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "could not run {interpreter} for the parity gate: {e}\n\
                 This gate compares Rust offsets against the Python reference, so it \
                 needs an interpreter with numpy installed. Set TENZORBUS_PYTHON to one."
            )
        });
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the Python reference implementation could not be imported by {interpreter}.\n\
         This is an environment problem, not a layout mismatch: the gate needs an \
         interpreter with numpy. Set TENZORBUS_PYTHON to a virtualenv that has it.\n\
         {stderr}"
    );
    let text = String::from_utf8(output.stdout).expect("utf8");
    parse_flat_json(&text)
}

/// Minimal JSON reader for the flat object the script above emits. Keeping the
/// gate dependency-free means it cannot be broken by a serde upgrade.
fn parse_flat_json(text: &str) -> Vec<(String, i128)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let mut prefix: Option<String> = None;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let start = i + 1;
                let mut end = start;
                while bytes[end] != b'"' {
                    end += 1;
                }
                let key = &text[start..end];
                i = end + 1;
                while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b':') {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b'{' {
                    prefix = Some(key.to_string());
                    i += 1;
                    continue;
                }
                if i < bytes.len() && bytes[i] == b'[' {
                    let mut idx = 0;
                    i += 1;
                    while bytes[i] != b']' {
                        if bytes[i].is_ascii_digit() {
                            let s = i;
                            while bytes[i].is_ascii_digit() {
                                i += 1;
                            }
                            let v: i128 = text[s..i].parse().unwrap();
                            out.push((format!("{key}[{idx}]"), v));
                            idx += 1;
                        } else {
                            i += 1;
                        }
                    }
                    i += 1;
                    continue;
                }
                let s = i;
                while i < bytes.len() && (bytes[i] == b'-' || bytes[i].is_ascii_digit()) {
                    i += 1;
                }
                let v: i128 = text[s..i]
                    .parse()
                    .unwrap_or_else(|_| panic!("number at {key}"));
                let full = match &prefix {
                    Some(p) => format!("{p}.{key}"),
                    None => key.to_string(),
                };
                out.push((full, v));
            }
            b'}' => {
                prefix = None;
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn get(consts: &[(String, i128)], key: &str) -> i128 {
    consts
        .iter()
        .find(|(k, _)| k == key)
        .unwrap_or_else(|| panic!("python reference did not export {key}"))
        .1
}

#[test]
fn rust_offsets_match_python_reference() {
    let c = python_constants();

    assert_eq!(get(&c, "VERSION"), proto::PROTOCOL_VERSION as i128);
    assert_eq!(
        get(&c, "GLOBAL_HEADER_SIZE"),
        proto::GLOBAL_HEADER_SIZE as i128
    );
    assert_eq!(get(&c, "SLOT_HEADER_SIZE"), proto::SLOT_HEADER_SIZE as i128);
    assert_eq!(get(&c, "MAX_NDIM"), proto::MAX_NDIM as i128);

    assert_eq!(get(&c, "STATE_FREE"), proto::STATE_FREE as i128);
    assert_eq!(get(&c, "STATE_COMMITTED"), proto::STATE_COMMITTED as i128);
    assert_eq!(get(&c, "STATE_WRITING"), proto::STATE_WRITING as i128);

    for (name, rust) in [
        ("G_MAGIC", proto::G_MAGIC),
        ("G_VERSION", proto::G_VERSION),
        ("G_SLOT_COUNT", proto::G_SLOT_COUNT),
        ("G_SLOT_CAPACITY", proto::G_SLOT_CAPACITY),
        ("G_NEXT_SEQ", proto::G_NEXT_SEQ),
        ("G_CONSUMERS", proto::G_CONSUMERS),
        ("G_MAX_CONSUMERS", proto::G_MAX_CONSUMERS),
        ("G_PUBLISH_COUNT", proto::G_PUBLISH_COUNT),
        ("G_DROP_COUNT", proto::G_DROP_COUNT),
        ("S_MAGIC", proto::S_MAGIC),
        ("S_STATE", proto::S_STATE),
        ("S_SEQUENCE", proto::S_SEQUENCE),
        ("S_TIMESTAMP_NS", proto::S_TIMESTAMP_NS),
        ("S_NBYTES", proto::S_NBYTES),
        ("S_DTYPE", proto::S_DTYPE),
        ("S_NDIM", proto::S_NDIM),
        ("S_READERS", proto::S_READERS),
        ("S_FLAGS", proto::S_FLAGS),
        ("S_SHAPE", proto::S_SHAPE),
        ("S_STRIDES", proto::S_STRIDES),
        ("S_PRODUCER_PID", proto::S_PRODUCER_PID),
    ] {
        assert_eq!(get(&c, name), rust as i128, "offset mismatch for {name}");
    }

    for i in 0..8 {
        assert_eq!(
            get(&c, &format!("MAGIC[{i}]")),
            proto::PROTOCOL_MAGIC[i] as i128
        );
    }
    for i in 0..4 {
        assert_eq!(
            get(&c, &format!("SLOT_MAGIC[{i}]")),
            proto::SLOT_MAGIC[i] as i128
        );
    }

    for d in [
        proto::DType::F32,
        proto::DType::F16,
        proto::DType::U8,
        proto::DType::I8,
        proto::DType::I16,
        proto::DType::I32,
        proto::DType::I64,
        proto::DType::F64,
        proto::DType::Bool,
    ] {
        assert_eq!(
            get(&c, &format!("DTYPES.{}", d.numpy_name())),
            d.code() as i128,
            "dtype code mismatch for {}",
            d.numpy_name()
        );
    }
}

#[test]
fn rust_layout_arithmetic_matches_python_reference() {
    let c = python_constants();
    assert_eq!(
        get(&c, "stride_602112"),
        proto::slot_stride(602_112) as i128
    );
    assert_eq!(
        get(&c, "total_8x1MiB"),
        proto::total_bytes(8, 1 << 20) as i128
    );
    assert_eq!(get(&c, "slot_base_3"), proto::slot_base(3, 602_112) as i128);
    assert_eq!(
        get(&c, "payload_base_3"),
        proto::payload_base(3, 602_112) as i128
    );
}

// These compare compile-time constants on purpose: the assertion *is* the
// documented invariant, and it fires the moment someone moves an offset.
#[allow(clippy::assertions_on_constants)]
#[test]
fn rust_extensions_live_in_reference_reserved_space() {
    // Everything the Rust transport added must sit in bytes the Python
    // reference zero-fills and never reads, or the two cannot share a mapping.
    assert!(proto::G_PUBLISH_FUTEX >= proto::G_DROP_COUNT + 8);
    assert!(proto::G_REGISTRY_OFF >= proto::G_REAPED_COUNT + 8);
    assert!(
        proto::registry_entry_offset(proto::MAX_CONSUMERS) <= proto::GLOBAL_HEADER_SIZE,
        "registry table overflows the global header"
    );
    assert!(proto::S_READERS_MASK >= proto::S_PRODUCER_PID + 4);
    assert!(proto::S_READERS_MASK + 8 <= proto::SLOT_HEADER_SIZE);
}
