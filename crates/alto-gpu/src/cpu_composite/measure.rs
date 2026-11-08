//! Measurements written for CI summaries: alpha, composite v3, prequant and atlas reads.
use super::*;

pub(super) struct MeasuredPixel {
    pub(super) x: u32,
    pub(super) y: u32,
    pub(super) sources: Vec<[u8; 4]>,
    pub(super) gpu: [u8; 4],
    pub(super) cpu: [u8; 4],
    /// Accumulator f32 on each side, immediately before quantization.
    pub(super) gpu_pre: [f32; 4],
    pub(super) cpu_pre: [f32; 4],
}

pub(super) struct MeasuredCase {
    pub(super) render_path: Option<&'static str>,
    pub(super) name: String,
    pub(super) count: u32,
    pub(super) max_abs: [u32; 4],
    pub(super) mismatches: Vec<MeasuredPixel>,
    pub(super) length_note: String,
    /// Shader operations this case actually runs. Not a diagnosis.
    pub(super) ops: String,
}

pub(super) fn rgba(bytes: &[u8], offset: usize) -> [u8; 4] {
    bytes[offset..offset + 4].try_into().unwrap()
}

pub(super) fn push_sources(node: &Node, doc_x: i32, doc_y: i32, out: &mut Vec<[u8; 4]>) {
    match node {
        Node::Raster { image, .. } => {
            if doc_x >= 0 && doc_y >= 0 && (doc_x as u32) < image.w && (doc_y as u32) < image.h {
                let i = ((doc_y as u32 * image.w + doc_x as u32) * 4) as usize;
                out.push(rgba(&image.pixels, i));
            }
        }
        Node::Group { children, .. } => {
            for child in children {
                push_sources(child, doc_x, doc_y, out);
            }
        }
    }
}

/// Straight image texels at the output pixel's document position. Identity
/// layers use this texel. A transformed layer samples somewhere else.
pub(super) fn sources_at(case: &Case, x: u32, y: u32) -> Vec<[u8; 4]> {
    let scale = 1i32 << case.level.max(0);
    let doc_x = case.tx * 256 * scale + x as i32 * scale;
    let doc_y = case.ty * 256 * scale + y as i32 * scale;
    let mut sources = Vec::new();
    for node in &case.nodes {
        push_sources(node, doc_x, doc_y, &mut sources);
    }
    sources
}

pub(super) fn fmt_rgba(px: [u8; 4]) -> String {
    format!("{},{},{},{}", px[0], px[1], px[2], px[3])
}

pub(super) fn fmt_sources(sources: &[[u8; 4]]) -> String {
    if sources.is_empty() {
        return "none".into();
    }
    sources
        .iter()
        .map(|px| fmt_rgba(*px))
        .collect::<Vec<_>>()
        .join(" | ")
}

pub(super) fn pre_at(values: &[f32], pixel: usize) -> [f32; 4] {
    let o = pixel * 4;
    if o + 4 <= values.len() {
        [values[o], values[o + 1], values[o + 2], values[o + 3]]
    } else {
        [f32::NAN; 4]
    }
}

pub(super) fn measure_case(
    case: &Case,
    cpu: &[u8],
    gpu: &[u8],
    cpu_pre: &[f32],
    gpu_pre: &[f32],
) -> MeasuredCase {
    let mut count = 0u32;
    let mut max_abs = [0u32; 4];
    let mut mismatches = Vec::new();
    let n = cpu.len().min(gpu.len()) / 4;
    for i in 0..n {
        let o = i * 4;
        if cpu[o..o + 4] == gpu[o..o + 4] {
            continue;
        }
        count += 1;
        for channel in 0..4 {
            max_abs[channel] =
                max_abs[channel].max(u32::from(cpu[o + channel].abs_diff(gpu[o + channel])));
        }
        let x = (i % 256) as u32;
        let y = (i / 256) as u32;
        mismatches.push(MeasuredPixel {
            x,
            y,
            sources: sources_at(case, x, y),
            gpu: rgba(gpu, o),
            cpu: rgba(cpu, o),
            gpu_pre: pre_at(gpu_pre, i),
            cpu_pre: pre_at(cpu_pre, i),
        });
    }
    let length_note = if cpu.len() == gpu.len() {
        String::new()
    } else {
        format!(" lengths cpu {} gpu {}", cpu.len(), gpu.len())
    };
    MeasuredCase {
        render_path: None, // Case fixtures contain only plain raster/group nodes.
        name: case.name.clone(),
        count,
        max_abs,
        mismatches,
        length_note,
        ops: shader_ops(case),
    }
}

pub(super) fn shader_ops(case: &Case) -> String {
    let mut parts = Vec::new();
    fn walk(nodes: &[Node], parts: &mut Vec<String>) {
        for node in nodes {
            match node {
                Node::Raster { mode, .. } => {
                    let name = BLENDS.get(*mode as usize).copied().unwrap_or("unknown");
                    parts.push(format!(
                        "draw blend {name}: unpremultiply dst (rgb/max(a, 0.000001)) when dst.a > 0, mix_color, three-term sum"
                    ));
                }
                Node::Group {
                    isolated, children, ..
                } => {
                    let kind = if *isolated {
                        "isolated"
                    } else {
                        "pass-through"
                    };
                    parts.push(format!(
                        "{kind} group pop: unpremultiply child accum (rgb/max(a, 0.000001)) when a > 0, scale alpha, then blend"
                    ));
                    walk(children, parts);
                }
            }
        }
    }
    walk(&case.nodes, &mut parts);
    parts.join("; ")
}

/// Distance in byte space from `v * 255` to the nearest `n + 0.5` boundary.
pub(super) fn dist_to_half(v: f32) -> f32 {
    if !v.is_finite() {
        return f32::NAN;
    }
    let scaled = v.clamp(0.0, 1.0) * 255.0;
    let shifted = scaled - 0.5;
    (shifted - shifted.round()).abs()
}

/// `clamp(v, 0, 1) * 255` when that product is finite.
pub(super) fn scaled_byte(v: f32) -> Option<f32> {
    if !v.is_finite() {
        return None;
    }
    let scaled = v.clamp(0.0, 1.0) * 255.0;
    scaled.is_finite().then_some(scaled)
}

/// Exact f32 equality with `n + 0.5`. Not an epsilon test.
pub(super) fn exactly_half_boundary(scaled: f32) -> bool {
    let n = scaled.floor();
    scaled == n + 0.5
}

/// `floor(x + 0.5)` on a non-negative byte-scaled value. At an exact tie this is
/// `n + 1`. Elsewhere it is the nearest integer.
pub(super) fn round_half_up_byte(scaled: f32) -> u8 {
    (scaled + 0.5).floor().clamp(0.0, 255.0) as u8
}

pub(super) fn round_ties_even_byte(scaled: f32) -> u8 {
    scaled.round_ties_even().clamp(0.0, 255.0) as u8
}

/// At an exact `n + 0.5`, the odd integer of `{n, n + 1}`.
pub(super) fn round_ties_odd_byte(scaled: f32) -> u8 {
    let n = scaled.floor() as u32;
    let odd = if n.is_multiple_of(2) { n + 1 } else { n };
    odd.min(255) as u8
}

pub(super) struct ChannelBoundary {
    /// Pre-store f32 bits match. The byte still differs; the caller checked that.
    pub(super) bits_equal: bool,
    pub(super) exact_half: bool,
    /// GPU byte is round-half-up and CPU byte is `round_ties_even`, and those
    /// two bytes are different.
    pub(super) tie_even_vs_half_up: bool,
    /// Exact tie, and the pair is not tie-even versus round-half-up.
    pub(super) exact_half_not_half_up: bool,
    /// Bit-identical pre-store value whose `v * 255` is not exactly `n + 0.5`.
    pub(super) not_exact_half: bool,
    /// Exact tie and the GPU byte is round-ties-to-odd.
    pub(super) gpu_ties_odd: bool,
    pub(super) class: &'static str,
}

pub(super) fn classify_boundary(
    gpu_v: f32,
    cpu_v: f32,
    gpu_byte: u8,
    cpu_byte: u8,
) -> ChannelBoundary {
    if gpu_v.to_bits() != cpu_v.to_bits() {
        return ChannelBoundary {
            bits_equal: false,
            exact_half: false,
            tie_even_vs_half_up: false,
            exact_half_not_half_up: false,
            not_exact_half: false,
            gpu_ties_odd: false,
            class: "floats-differ",
        };
    }
    let Some(scaled) = scaled_byte(gpu_v) else {
        return ChannelBoundary {
            bits_equal: true,
            exact_half: false,
            tie_even_vs_half_up: false,
            exact_half_not_half_up: false,
            not_exact_half: true,
            gpu_ties_odd: false,
            class: "not-exact-half",
        };
    };
    let ties_even = round_ties_even_byte(scaled);
    let half_up = round_half_up_byte(scaled);
    if !exactly_half_boundary(scaled) {
        return ChannelBoundary {
            bits_equal: true,
            exact_half: false,
            tie_even_vs_half_up: false,
            exact_half_not_half_up: false,
            not_exact_half: true,
            gpu_ties_odd: false,
            class: "not-exact-half",
        };
    }
    let tie_even_vs_half_up = half_up != ties_even && gpu_byte == half_up && cpu_byte == ties_even;
    ChannelBoundary {
        bits_equal: true,
        exact_half: true,
        tie_even_vs_half_up,
        exact_half_not_half_up: !tie_even_vs_half_up,
        not_exact_half: false,
        gpu_ties_odd: gpu_byte == round_ties_odd_byte(scaled),
        class: if tie_even_vs_half_up {
            "tie-even-vs-half-up"
        } else {
            "exact-half-not-half-up"
        },
    }
}

pub(super) fn json_f32(v: f32) -> String {
    if v.is_finite() {
        format!("{v:.8e}")
    } else {
        "null".into()
    }
}

pub(super) fn measure_line(row: &MeasuredCase) -> String {
    let pixels = row
        .mismatches
        .iter()
        .take(5)
        .map(|pixel| {
            format!(
                "pixel {},{} source {} gpu {} cpu {}",
                pixel.x,
                pixel.y,
                fmt_sources(&pixel.sources),
                fmt_rgba(pixel.gpu),
                fmt_rgba(pixel.cpu)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let pixels = if pixels.is_empty() {
        String::new()
    } else {
        format!("; {pixels}")
    };
    format!(
        "MEASURED-NOT-ENFORCED {}: {} pixels differ; max abs {},{},{},{}{}{}",
        row.name,
        row.count,
        row.max_abs[0],
        row.max_abs[1],
        row.max_abs[2],
        row.max_abs[3],
        pixels,
        row.length_note
    )
}

pub(super) fn json_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The `unorm_byte` that every v3 atlas read uses (`q14.wgsl`). The atlas-read
/// measurement runs this exact text; `atlas_read_uses_effects_unorm_byte` checks it.
pub(super) const UNORM_BYTE_WGSL: &str = "fn unorm_byte(v: f32) -> u32 {
  return u32(clamp(v, 0.0, 1.0) * 255.0 + 0.5);
}";

/// Texel `i` of the 256x1 atlas-read texture. Every channel sweeps all 256 bytes.
pub(super) fn atlas_read_texel(i: usize) -> [u8; 4] {
    let i = i as u8;
    [i, 255 - i, i ^ 0x55, i.wrapping_add(128)]
}

pub(super) fn atlas_read_shader() -> String {
    format!(
        "@group(0) @binding(0) var atlas: texture_2d<f32>;
@group(0) @binding(1) var<storage, read_write> out: array<u32, 1024>;
{UNORM_BYTE_WGSL}
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3u) {{
  if (id.x >= 256u) {{ return; }}
  let v = textureLoad(atlas, vec2u(id.x, 0u), 0);
  out[id.x * 4u] = unorm_byte(v.r);
  out[id.x * 4u + 1u] = unorm_byte(v.g);
  out[id.x * 4u + 2u] = unorm_byte(v.b);
  out[id.x * 4u + 3u] = unorm_byte(v.a);
}}
"
    )
}

/// Result of reading all 256 bytes from an `Rgba8Unorm` (all 4 channels) or an
/// `R8Unorm` (the v3 glyph atlas and scratch, `.r`) texture with `textureLoad` +
/// `unorm_byte`. Enforced.
pub(super) struct AtlasRead {
    pub(super) format: &'static str,
    pub(super) mismatches: Vec<(usize, usize, u8, u32)>,
}

impl AtlasRead {
    pub(super) fn line(&self) -> String {
        let channels = if self.format == "R8Unorm" {
            "the r channel"
        } else {
            "all 4 channels"
        };
        if self.mismatches.is_empty() {
            format!(
                "atlas-read {} (enforced): 256/256 byte values round-trip exactly on {channels}",
                self.format
            )
        } else {
            let shown: Vec<String> = self
                .mismatches
                .iter()
                .take(8)
                .map(|(t, c, want, got)| format!("texel {t} ch {c} byte {want} read {got}"))
                .collect();
            format!(
                "atlas-read {} (enforced): {} channel mismatches: {}",
                self.format,
                self.mismatches.len(),
                shown.join("; ")
            )
        }
    }
}

/// The JSON fields of one atlas read.
pub(super) fn atlas_read_fields(result: &Result<AtlasRead, String>) -> String {
    let (status, count, error) = match result {
        Ok(r) if r.mismatches.is_empty() => ("exact", 0, String::new()),
        Ok(r) => ("mismatch", r.mismatches.len(), String::new()),
        Err(e) => ("error", 0, e.clone()),
    };
    let mut list = String::new();
    if let Ok(r) = result {
        for (n, (t, c, want, got)) in r.mismatches.iter().enumerate() {
            if n > 0 {
                list.push_str(", ");
            }
            list.push_str(&format!(
                "{{\"texel\": {t}, \"channel\": {c}, \"byte\": {want}, \"read\": {got}}}"
            ));
        }
    }
    format!(
        "\"read\": \"textureLoad + unorm_byte\", \"values\": 256, \"status\": \"{status}\", \"mismatchedChannels\": {count}, \"error\": \"{}\", \"mismatches\": [{list}]",
        json_escape(&error)
    )
}

/// `atlas-read.json`: the `Rgba8Unorm` read at the top level (as before) and the
/// `R8Unorm` read under `r8Unorm`.
pub(super) fn write_atlas_read(
    adapter: &str,
    rgba: &Result<AtlasRead, String>,
    r8: &Result<AtlasRead, String>,
) {
    let Some(dir) = std::env::var_os("ALTO_ALPHA_MEASURE_OUT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create alpha measure directory");
    let json = format!(
        "{{\"label\": \"enforced\", \"backend\": \"{}\", \"format\": \"Rgba8Unorm\", {}, \"r8Unorm\": {{\"format\": \"R8Unorm\", {}}}}}\n",
        json_escape(adapter),
        atlas_read_fields(rgba),
        atlas_read_fields(r8)
    );
    std::fs::write(dir.join("atlas-read.json"), json).expect("write atlas read json");
    let line = |result: &Result<AtlasRead, String>, format: &str| match result {
        Ok(r) => r.line(),
        Err(e) => format!("atlas-read {format} (enforced): GPU error: {e}"),
    };
    let markdown = format!(
        "### Atlas read (`textureLoad` + `unorm_byte`)\n\nBackend: `{}`\n\n- {}\n- {}\n",
        adapter,
        line(rgba, "Rgba8Unorm"),
        line(r8, "R8Unorm")
    );
    std::fs::write(dir.join("atlas-read.md"), markdown).expect("write atlas read markdown");
}

pub(super) fn write_alpha_measure(adapter: &str, rows: &[MeasuredCase]) {
    write_measure(adapter, rows, false, &[]);
}
pub(super) fn write_v3_measure(adapter: &str, rows: &[MeasuredCase], errors: &[String]) {
    write_measure(adapter, rows, true, errors);
}
pub(super) fn write_measure(
    adapter: &str,
    rows: &[MeasuredCase],
    integer: bool,
    errors: &[String],
) {
    let Some(dir) = std::env::var_os("ALTO_ALPHA_MEASURE_OUT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create alpha measure directory");
    let mut json = format!(
        "{{\n  \"label\": \"measured-not-enforced\",\n  \"backend\": \"{}\",\n  \"cases\": [\n",
        json_escape(adapter)
    );
    for (index, row) in rows.iter().enumerate() {
        let enforced = integer && row.render_path != Some("v3-unsupported");
        let render_path = row
            .render_path
            .map_or("null".to_string(), |p| format!("\"{p}\""));
        if index > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "    {{\"name\": \"{}\", \"renderPath\": {render_path}, \"enforced\": {enforced}, \"status\": \"{}\", \"count\": {}, \"maxAbs\": [{},{},{},{}], \"mismatches\": [",
            json_escape(&row.name),
            if row.count==0 && row.length_note.is_empty() {"exact"} else {"mismatch"},
            row.count,
            row.max_abs[0],
            row.max_abs[1],
            row.max_abs[2],
            row.max_abs[3]
        ));
        for (pixel_index, pixel) in row.mismatches.iter().enumerate() {
            if pixel_index > 0 {
                json.push_str(", ");
            }
            let sources = pixel
                .sources
                .iter()
                .map(|px| format!("[{},{},{},{}]", px[0], px[1], px[2], px[3]))
                .collect::<Vec<_>>()
                .join(", ");
            json.push_str(&format!(
                "{{\"x\": {}, \"y\": {}, \"sources\": [{sources}], \"gpu\": [{},{},{},{}], \"cpu\": [{},{},{},{}]}}",
                pixel.x,
                pixel.y,
                pixel.gpu[0],
                pixel.gpu[1],
                pixel.gpu[2],
                pixel.gpu[3],
                pixel.cpu[0],
                pixel.cpu[1],
                pixel.cpu[2],
                pixel.cpu[3]
            ));
        }
        json.push_str("]}");
    }
    json.push_str("\n  ]");
    if integer {
        let exact_count = rows
            .iter()
            .filter(|r| {
                r.render_path != Some("v3-unsupported") && r.count == 0 && r.length_note.is_empty()
            })
            .count();
        let unsupported_count = rows
            .iter()
            .filter(|r| r.render_path == Some("v3-unsupported"))
            .count();
        json.push_str(&format!(
            ",\n  \"integerExactCount\": {exact_count},\n  \"v3UnsupportedCount\": {unsupported_count}"
        ));
        let status = if !errors.is_empty() {
            "error"
        } else if rows.iter().any(|r| {
            r.render_path != Some("v3-unsupported") && (r.count != 0 || !r.length_note.is_empty())
        }) {
            "mismatch"
        } else {
            "exact"
        };
        let list = errors
            .iter()
            .map(|e| format!("\"{}\"", json_escape(e)))
            .collect::<Vec<_>>()
            .join(",");
        json.push_str(&format!(
            ",\n  \"status\": \"{status}\",\n  \"errors\": [{list}]"
        ));
    }
    json.push_str("\n}\n");
    let mut markdown = format!(
        "### Semi-transparent parity ({})\n\nThese cases are **measured-not-enforced**. A mismatch does not fail the job.\n\nBackend: `{adapter}`\n\n| case | mismatched pixels | max abs r,g,b,a | first mismatches |\n| --- | ---: | --- | --- |\n",
        json_escape(adapter)
    );
    for row in rows {
        let shown = row
            .mismatches
            .iter()
            .take(3)
            .map(|pixel| {
                format!(
                    "({},{}) source {} gpu {} cpu {}",
                    pixel.x,
                    pixel.y,
                    fmt_sources(&pixel.sources),
                    fmt_rgba(pixel.gpu),
                    fmt_rgba(pixel.cpu)
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let shown = if shown.is_empty() {
            "none".into()
        } else {
            shown
        };
        markdown.push_str(&format!(
            "| {}{} | {} | {},{},{},{} | {}{} |\n",
            row.name,
            if row.render_path == Some("v3-unsupported") {
                " (v3-unsupported; refused by the host)"
            } else {
                ""
            },
            row.count,
            row.max_abs[0],
            row.max_abs[1],
            row.max_abs[2],
            row.max_abs[3],
            shown,
            row.length_note
        ));
    }
    if integer && !errors.is_empty() {
        markdown.push_str("\nGPU errors:\n\n");
        for error in errors {
            markdown.push_str(&format!("- {error}\n"));
        }
    }
    if integer {
        json = json.replacen("measured-not-enforced", "enforced", 1);
        markdown = markdown
            .replacen("measured-not-enforced", "enforced", 1)
            .replace("A mismatch does not fail", "A mismatch fails");
    }
    std::fs::write(
        dir.join(if integer {
            "composite-v3.json"
        } else {
            "alpha-measure.json"
        }),
        json,
    )
    .expect("write alpha measure json");
    std::fs::write(
        dir.join(if integer {
            "composite-v3.md"
        } else {
            "alpha-measure.md"
        }),
        markdown,
    )
    .expect("write alpha measure markdown");
    if !integer {
        write_prequant(&dir, adapter, rows);
    }
}

/// Every mismatching channel: pre-quantization f32 on both sides, and the
/// distance of each from the nearest `n+0.5` boundary. Epsilon is 1e-3 in
/// byte space (`channel * 255`).
pub(super) fn write_prequant(dir: &std::path::Path, adapter: &str, rows: &[MeasuredCase]) {
    const EPS: f32 = 1.0e-3;
    const TIGHTER: f32 = 1.0e-4;
    let channels = ["r", "g", "b", "a"];
    let mut differing = 0u32;
    let mut on_boundary = 0u32;
    let mut on_boundary_tight = 0u32;
    let mut floats_within = 0u32;
    let mut floats_apart = 0u32;
    let mut bit_identical = 0u32;
    let mut exact_half = 0u32;
    let mut tie_even_vs_half_up = 0u32;
    let mut exact_half_not_half_up = 0u32;
    let mut not_exact_half = 0u32;
    let mut gpu_ties_odd = 0u32;
    let mut max_delta = 0f32;
    let mut lines = Vec::new();
    for row in rows {
        for pixel in &row.mismatches {
            for (index, name) in channels.iter().enumerate() {
                if pixel.gpu[index] == pixel.cpu[index] {
                    continue;
                }
                differing += 1;
                let gpu_v = pixel.gpu_pre[index];
                let cpu_v = pixel.cpu_pre[index];
                let boundary = classify_boundary(gpu_v, cpu_v, pixel.gpu[index], pixel.cpu[index]);
                let gpu_dist = dist_to_half(gpu_v);
                let cpu_dist = dist_to_half(cpu_v);
                let nearer = gpu_dist.min(cpu_dist);
                let delta = ((gpu_v.clamp(0.0, 1.0) - cpu_v.clamp(0.0, 1.0)) * 255.0).abs();
                if nearer <= EPS {
                    on_boundary += 1;
                }
                if nearer <= TIGHTER {
                    on_boundary_tight += 1;
                }
                if delta <= EPS {
                    floats_within += 1;
                } else {
                    floats_apart += 1;
                }
                if boundary.bits_equal {
                    bit_identical += 1;
                }
                if boundary.exact_half {
                    exact_half += 1;
                }
                if boundary.tie_even_vs_half_up {
                    tie_even_vs_half_up += 1;
                }
                if boundary.exact_half_not_half_up {
                    exact_half_not_half_up += 1;
                }
                if boundary.not_exact_half {
                    not_exact_half += 1;
                }
                if boundary.gpu_ties_odd {
                    gpu_ties_odd += 1;
                }
                let (ties_even_byte, half_up_byte) = scaled_byte(gpu_v)
                    .filter(|_| boundary.bits_equal)
                    .map(|scaled| (round_ties_even_byte(scaled), round_half_up_byte(scaled)))
                    .unwrap_or((0, 0));
                max_delta = max_delta.max(delta);
                lines.push(format!(
                    "    {{\"case\": \"{}\", \"x\": {}, \"y\": {}, \"channel\": \"{name}\", \"gpuByte\": {}, \"cpuByte\": {}, \"gpu\": {}, \"cpu\": {}, \"gpuBits\": {}, \"cpuBits\": {}, \"gpuDistToHalf\": {}, \"cpuDistToHalf\": {}, \"absScaledDelta\": {}, \"exactHalf\": {}, \"tiesEvenByte\": {ties_even_byte}, \"halfUpByte\": {half_up_byte}, \"boundaryClass\": \"{}\", \"ops\": \"{}\"}}",
                    json_escape(&row.name),
                    pixel.x,
                    pixel.y,
                    pixel.gpu[index],
                    pixel.cpu[index],
                    json_f32(gpu_v),
                    json_f32(cpu_v),
                    gpu_v.to_bits(),
                    cpu_v.to_bits(),
                    json_f32(gpu_dist),
                    json_f32(cpu_dist),
                    json_f32(delta),
                    if boundary.exact_half { "true" } else { "false" },
                    boundary.class,
                    json_escape(&row.ops),
                ));
            }
        }
    }
    let body = if lines.is_empty() {
        String::new()
    } else {
        lines.join(",\n")
    };
    let json = format!(
        "{{\n  \"backend\": \"{}\",\n  \"epsilonScaled\": {EPS:.0e},\n  \"tighterEpsilonScaled\": {TIGHTER:.0e},\n  \"differingChannels\": {differing},\n  \"withinEpsilonOfHalf\": {on_boundary},\n  \"withinTighterEpsilonOfHalf\": {on_boundary_tight},\n  \"prequantAbsDeltaWithinEpsilon\": {floats_within},\n  \"prequantAbsDeltaBeyondEpsilon\": {floats_apart},\n  \"bitIdenticalChannels\": {bit_identical},\n  \"bitIdenticalExactHalf\": {exact_half},\n  \"bitIdenticalTieEvenVsHalfUp\": {tie_even_vs_half_up},\n  \"bitIdenticalExactHalfNotHalfUp\": {exact_half_not_half_up},\n  \"bitIdenticalNotExactHalf\": {not_exact_half},\n  \"bitIdenticalExactHalfGpuTiesToOdd\": {gpu_ties_odd},\n  \"maxAbsScaledDelta\": {},\n  \"channels\": [\n{body}\n  ]\n}}\n",
        json_escape(adapter),
        json_f32(max_delta)
    );
    let mut markdown = format!(
        "### Pre-quantization dump ({})\n\n\
         Copied from the f32 accumulator immediately before quantization. \
         GPU values are `accum` stored to a debug buffer before the same `textureStore` into `rgba8unorm`. \
         CPU values are the same accumulator before `round_ties_even`. \
         The v2 formula is unchanged. Distance is `|channel * 255 - nearest n+0.5|` in byte space. \
         Epsilon is {EPS:.0e}. A tighter count uses {TIGHTER:.0e}.\n\n\
         - Differing channels: {differing}\n\
         - Within {EPS:.0e} of an n+0.5 boundary: {on_boundary}\n\
         - Within {TIGHTER:.0e} of an n+0.5 boundary: {on_boundary_tight}\n\
         - `|GPU - CPU| * 255` within {EPS:.0e}: {floats_within}\n\
         - `|GPU - CPU| * 255` beyond {EPS:.0e}: {floats_apart}\n\
         - Bit-identical pre-quant channels: {bit_identical}\n\
         - Max `|GPU - CPU| * 255`: {}\n\
         - Bit-identical and `v * 255` exactly `n + 0.5`: {exact_half}\n\
         - Of those, GPU byte is round-half-up and CPU byte is round-ties-even: {tie_even_vs_half_up}\n\
         - Of those, exact `n + 0.5` but not that pair: {exact_half_not_half_up}\n\
         - Bit-identical and not exactly `n + 0.5`: {not_exact_half}\n\
         - Of the exact halves, GPU byte equals round-ties-to-odd: {gpu_ties_odd}\n\n",
        json_escape(adapter),
        json_f32(max_delta)
    );
    markdown.push_str(
        "Exact means f32 equality of `clamp(v, 0, 1) * 255` with `floor(that) + 0.5`, not an epsilon. \
         round-half-up is `floor(clamp(v, 0, 1) * 255 + 0.5)`. \
         CPU bytes use `round_ties_even`. \
         A bit-identical channel that is not exactly `n + 0.5` means the unorm store differed from the CPU quantizer away from a tie.\n\n",
    );
    if not_exact_half > 0 {
        markdown.push_str(
            "At least one bit-identical channel is not exactly on `n + 0.5`. The unorm store conversion itself differs from CPU away from a tie.\n\n",
        );
    }
    if exact_half_not_half_up > 0 {
        markdown.push_str(
            "Some exact ties are not round-half-up versus round-ties-even: those two rules agree, and the GPU byte is the other integer.\n\n",
        );
    }
    if floats_apart == 0 && differing > 0 && bit_identical == differing {
        markdown.push_str(
            "Every differing channel has bit-identical pre-quantization floats. The ±1 is the `rgba8unorm` store versus CPU `round_ties_even`.\n\n",
        );
    } else if floats_apart == 0 && differing > 0 {
        markdown.push_str(&format!(
            "{bit_identical} of {differing} pre-quant channels are bit-identical. {on_boundary_tight} of {differing} are within {TIGHTER:.0e} of an n+0.5 boundary, and {on_boundary} of {differing} are within {EPS:.0e}. Max `|GPU - CPU| * 255` is {}. Where a channel sits on that boundary, the stored ±1 is the gap crossing it. The f32 ops in the mismatching cases are unpremultiply division and the blend FMAs. This dump does not separate those ops.\n\n",
            json_f32(max_delta)
        ));
    } else if floats_apart > 0 {
        markdown.push_str(
            "Some pre-quantization floats already differ by more than 1e-3 byte. Those pixels ran the f32 ops listed below (unpremultiply division and blend FMAs). This dump does not separate those two ops.\n\n",
        );
    }
    markdown.push_str("| case | channel pixels | on boundary | floats differ | ops |\n| --- | ---: | ---: | ---: | --- |\n");
    for row in rows {
        if row.count == 0 {
            continue;
        }
        let mut row_channels = 0u32;
        let mut row_boundary = 0u32;
        let mut row_apart = 0u32;
        for pixel in &row.mismatches {
            for index in 0..4 {
                if pixel.gpu[index] == pixel.cpu[index] {
                    continue;
                }
                row_channels += 1;
                let nearer =
                    dist_to_half(pixel.gpu_pre[index]).min(dist_to_half(pixel.cpu_pre[index]));
                let delta = ((pixel.gpu_pre[index].clamp(0.0, 1.0)
                    - pixel.cpu_pre[index].clamp(0.0, 1.0))
                    * 255.0)
                    .abs();
                if nearer <= EPS {
                    row_boundary += 1;
                }
                if delta > EPS {
                    row_apart += 1;
                }
            }
        }
        markdown.push_str(&format!(
            "| {} | {row_channels} | {row_boundary} | {row_apart} | {} |\n",
            row.name,
            json_escape(&row.ops)
        ));
    }
    std::fs::write(dir.join("prequant.json"), json).expect("write prequant json");
    std::fs::write(dir.join("prequant.md"), markdown).expect("write prequant markdown");
}

pub(super) fn diff_report(cpu: &[u8], gpu: &[u8]) -> String {
    let mut count = 0u32;
    let mut at = None;
    let n = cpu.len().min(gpu.len()) / 4;
    for i in 0..n {
        let o = i * 4;
        if cpu[o..o + 4] != gpu[o..o + 4] {
            count += 1;
            if at.is_none() {
                at = Some((i % 256, i / 256, o));
            }
        }
    }
    match at {
        Some((x, y, o)) => format!(
            "{count} pixels differ; first {x},{y}; gpu {},{},{},{}; cpu {},{},{},{}; lengths cpu {} gpu {}",
            gpu[o],
            gpu[o + 1],
            gpu[o + 2],
            gpu[o + 3],
            cpu[o],
            cpu[o + 1],
            cpu[o + 2],
            cpu[o + 3],
            cpu.len(),
            gpu.len()
        ),
        None => format!(
            "0 pixels differ; first none; gpu none; cpu none; lengths cpu {} gpu {}",
            cpu.len(),
            gpu.len()
        ),
    }
}

pub(super) fn parse_reference(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut parts = line.split(' ');
            (
                parts.next().unwrap().to_string(),
                parts.next().unwrap().to_string(),
                parts.next().unwrap().to_string(),
            )
        })
        .collect()
}
