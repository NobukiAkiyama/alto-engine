#![no_main]
//! SVG path data (`d` attributes and the SDK's path strings).
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Ok(d) = std::str::from_utf8(data) else { return };
    let _ = alto_vector::PathData::from_svg(d);
});
