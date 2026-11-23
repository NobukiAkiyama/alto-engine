#![no_main]
//! Decode worker request frames and compressed tile data.
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let out = alto_decode::decode_batch(data);
    if out.is_empty() {
        return;
    }
    let count = u32::from_le_bytes(out[..4].try_into().unwrap());
    let mut pos = 4usize;
    for _ in 0..count {
        let header = out.get(pos..pos + 37).expect("complete output header");
        let len = u32::from_le_bytes(header[33..37].try_into().unwrap()) as usize;
        pos = pos.checked_add(37 + len).expect("output length overflow");
        assert!(pos <= out.len(), "complete output payload");
    }
    assert_eq!(pos, out.len(), "output count must match its records");
});
