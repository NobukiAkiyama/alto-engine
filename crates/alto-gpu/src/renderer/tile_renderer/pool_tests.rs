use super::*;
#[test]
fn buckets_bound_padding_and_leases_hold_ancestors() {
    assert_eq!(bucket(256), 256);
    assert_eq!(bucket(280), 320);
    assert_eq!(bucket(1), 64);
    let owner = Arc::new(());
    let lease = Arc::downgrade(&owner);
    let ancestor = owner.clone();
    drop(owner);
    assert_eq!(lease.strong_count(), 1);
    drop(ancestor);
    assert_eq!(lease.strong_count(), 0);
}
#[test]
fn shader_bounds_every_logical_input_and_output() {
    let s = include_str!("../../../shaders/effects.wgsl");
    assert!(s.contains("vec2i(c.sizes.xy)"));
    assert!(s.contains("vec2i(c.sizes.zw)"));
    assert!(s.contains("id.xy>=c.output_clip.xy"));
    assert!(s.contains("vec2i(c.output_clip.zw)"));
    assert!(!s.contains("textureDimensions("));
}
fn roi(x: f32, y: f32, scale: f32) -> Roi {
    Roi {
        x,
        y,
        scale,
        w: 256,
        h: 256,
    }
}
/// Every input region and mask a v3 mask stage cannot place in integers is
/// refused with `GpuUnsupported` (DQ3-4); equal regions need no integers.
#[test]
fn v3_mask_stages_refuse_regions_without_an_integer_map() {
    let map = super::super::input_map;
    // Equal regions are the pixel index itself, fractional or not.
    assert_eq!(
        map([0.5, 3.25, 0.5], [256, 256], [0.5, 3.25, 0.5]),
        Some([0; 4])
    );
    // 2 (out - in) + out_scale, 2 out_scale, 2 in_scale.
    assert_eq!(
        map([-300.0, 37.0, 1.0], [256, 256], [-295.0, 34.0, 1.0]),
        Some([-9, 7, 2, 2])
    );
    assert_eq!(
        map([-256.0, -512.0, 2.0], [256, 256], [-260.0, -520.0, 4.0]),
        Some([5, 9, 2, 4])
    );
    // Levels below 0 share the finer grid: texel floor((2i + 1) / 4), then 2i + 1.
    assert_eq!(
        map([0.0, 0.0, 0.5], [256, 256], [0.0, 0.0, 1.0]),
        Some([1, 1, 2, 4])
    );
    assert_eq!(
        map([0.0, 0.0, 1.0], [256, 256], [0.0, 0.0, 0.5]),
        Some([2, 2, 4, 2])
    );
    for (out, input) in [
        ([0.5, 0.0, 1.0], [0.0, 0.0, 1.0]),
        ([0.0, 0.0, 1.0], [0.0, -0.25, 1.0]),
        ([0.25, 0.0, 0.5], [0.0, 0.0, 1.0]),
        // A numerator past i32.
        ([1_073_741_824.0, 0.0, 1.0], [-1_073_741_824.0, 0.0, 1.0]),
        // The step times the last pixel past i32.
        ([0.0, 0.0, 16_777_216.0], [0.0, 0.0, 1.0]),
    ] {
        assert_eq!(map(out, [256, 256], input), None, "{out:?} from {input:?}");
    }
    let unsupported = |r: Result<Params>| matches!(r, Err(RenderError::GpuUnsupported(_)));
    let out = roi(-300.0, 37.0, 1.0);
    for mode in [21, 22, 23] {
        // Either input off the integer grid.
        assert!(unsupported(Context::mask_params(
            mode,
            None,
            out,
            roi(-299.5, 37.0, 1.0),
            out,
            [16384, 0, 0, 0]
        )));
        assert!(unsupported(Context::mask_params(
            mode,
            None,
            out,
            out,
            roi(-300.25, 37.0, 0.5),
            [16384, 0, 0, 0]
        )));
        // A mask origin off its level's grid, below or at level 0.
        for at in [roi(0.25, 0.0, 0.5), roi(0.5, 0.0, 1.0)] {
            assert!(unsupported(Context::mask_params(
                mode,
                Some(WorldTransform::V3(alto_core::FixedAffine::IDENTITY)),
                at,
                at,
                at,
                [16384, 1, 0, 0]
            )));
        }
    }
    // An unmasked mode 23 at a fractional level with equal regions draws.
    let zoomed = roi(64.0, 32.0, 0.5);
    let p = Context::mask_params(23, None, zoomed, zoomed, zoomed, [8192, 0, 2, 0]).unwrap();
    assert_eq!(
        p.curve[0..12]
            .iter()
            .map(|v| v.to_bits() as i32)
            .collect::<Vec<_>>(),
        [8192, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    assert!(p.out_region == [0.0; 4] && p.inv0 == [0.0; 4] && p.extra == [0.0; 4]);
}
/// Removes `//` comments.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|l| l.split("//").next().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}
/// Every `fn` of a WGSL source and its body, braces included.
fn wgsl_functions(src: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut rest = src;
    while let Some(at) = rest.find("fn ") {
        let boundary = at == 0 || !rest.as_bytes()[at - 1].is_ascii_alphanumeric();
        let after = &rest[at + 3..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        let Some(open) = after.find('{') else {
            break;
        };
        if !boundary || name.is_empty() {
            rest = after;
            continue;
        }
        let mut depth = 0;
        let mut end = open;
        for (i, ch) in after[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.insert(name, after[open..end].to_string());
        rest = &after[end..];
    }
    out
}
/// Float tokens a v3 stage must not contain: float types, float built-ins,
/// float literals (a digit, a point, a digit) and the f32 uniform slots.
fn float_tokens(body: &str) -> Vec<String> {
    let mut found: Vec<String> = [
        "f32",
        "vec2f",
        "vec3f",
        "vec4f",
        "floor(",
        "fract(",
        "round(",
        "mix(",
        "c.unused",
        "c.extra",
        "out_region",
        "region_a",
        "region_b",
        "inv0",
        "inv1",
        "c.settings",
        "origin_scale",
    ]
    .into_iter()
    .filter(|t| body.contains(t))
    .map(str::to_string)
    .collect();
    let b = body.as_bytes();
    for i in 1..b.len().saturating_sub(1) {
        if b[i] == b'.' && b[i - 1].is_ascii_digit() && b[i + 1].is_ascii_digit() {
            found.push(format!("literal at {i}"));
        }
    }
    found
}
/// The v3 paint shaders' only float is the `R8Unorm` coverage texel: one
/// `texture_2d<f32>` binding, read once and passed straight to `unorm_byte`.
#[test]
fn v3_paint_shaders_read_only_the_coverage_byte_as_float() {
    for (name, text, texture) in [
        (
            "vector-q14",
            include_str!("../../../shaders/vector-q14.wgsl"),
            "coverage",
        ),
        (
            "glyph-q14",
            include_str!("../../../shaders/glyph-q14.wgsl"),
            "scratch",
        ),
    ] {
        let code = strip_comments(text);
        assert_eq!(code.matches("f32").count(), 1, "{name}: one float binding");
        assert!(
            code.contains(&format!("var {texture}: texture_2d<f32>;")),
            "{name}"
        );
        assert_eq!(
            code.matches(&format!("textureLoad({texture},")).count(),
            1,
            "{name}"
        );
        assert!(
            code.contains(&format!("unorm_byte(textureLoad({texture},")),
            "{name}"
        );
        assert!(!code.contains("textureSample"), "{name}");
    }
}
/// The float purge: no v3 M5 stage (`effects-q14.wgsl`, every mode from
/// `main`) and not the v3 resolve reads or computes a float, except the
/// Rgba8Unorm atlas texel in `atlas_bytes`, which goes straight through
/// `unorm_byte` back to its bytes. The f32 coordinate helpers of earlier
/// versions (`load_a`, `load_b`, `sparse_bytes`) are gone, the uniform
/// declares no float field, and every mode reaches the integer placement.
#[test]
fn v3_stages_call_no_f32_coordinate_helper() {
    let shader = strip_comments(&crate::integer_effects_shader());
    let functions = wgsl_functions(&shader);
    for gone in ["load_a", "load_b", "sparse_bytes", "sparse", "load_clip"] {
        assert!(!functions.contains_key(gone), "{gone} is still defined");
    }
    let mut reached = vec!["main".to_string()];
    let mut i = 0;
    while i < reached.len() {
        let body = &functions[&reached[i]];
        for name in functions.keys() {
            if !reached.contains(name)
                && body.match_indices(&format!("{name}(")).any(|(at, _)| {
                    at == 0 || {
                        let c = body.as_bytes()[at - 1];
                        !(c.is_ascii_alphanumeric() || c == b'_')
                    }
                })
            {
                reached.push(name.clone());
            }
        }
        i += 1;
    }
    for needed in [
        "mask_stage",
        "texel_a",
        "texel_b",
        "input_texel",
        "floor_div",
        "color_adjust_q14",
        "layer_q14",
        "lookup",
        "tile_texel",
        "atlas_bytes",
        "mask_q",
        "pixel_q8",
        "coord_q8",
        "div28",
        "unorm_byte",
        "premul_byte_q14",
        "byte_q14",
    ] {
        assert!(
            reached.iter().any(|r| r == needed),
            "main does not reach {needed}"
        );
    }
    for name in reached
        .iter()
        .filter(|n| !["unorm_byte", "atlas_bytes"].contains(&n.as_str()))
    {
        let tokens = float_tokens(&functions[name]);
        assert!(
            tokens.is_empty(),
            "{name} (reached from main) uses {tokens:?}"
        );
    }
    // The atlas is read only in `atlas_bytes`, and each channel goes through
    // `unorm_byte`.
    assert_eq!(shader.matches("textureLoad(atlas").count(), 1);
    let atlas = &functions["atlas_bytes"];
    assert!(atlas.contains("textureLoad(atlas"));
    for ch in ["s.r", "s.g", "s.b", "s.a"] {
        assert!(atlas.contains(&format!("unorm_byte({ch})")), "{ch}");
    }
    // The uniform declares integers only.
    let config = &shader[shader.find("struct Config").unwrap()..];
    let config = &config[..config.find('}').unwrap()];
    assert!(float_tokens(config).is_empty(), "{config}");
    assert!(config.contains("grid: array<vec4i, 16>"));
    // The v3 resolve is integer throughout, its canvas test included.
    let resolve = strip_comments(include_str!("../../../shaders/effects-resolve-q14.wgsl"));
    assert!(
        float_tokens(&resolve).is_empty(),
        "{:?}",
        float_tokens(&resolve)
    );
    assert!(resolve.contains("all(id.xy>=c.span.xy)&&all(id.xy<c.span.zw)"));
}
/// Every function `main` reaches in `src`, `main` included.
fn reached_from_main(functions: &HashMap<String, String>) -> Vec<String> {
    let mut reached = vec!["main".to_string()];
    let mut i = 0;
    while i < reached.len() {
        let body = &functions[&reached[i]];
        for name in functions.keys() {
            if !reached.contains(name)
                && body.match_indices(&format!("{name}(")).any(|(at, _)| {
                    at == 0 || {
                        let c = body.as_bytes()[at - 1];
                        !(c.is_ascii_alphanumeric() || c == b'_')
                    }
                })
            {
                reached.push(name.clone());
            }
        }
        i += 1;
    }
    reached
}
/// The plain-path float purge: the v3 integer tile compositor
/// (`integer_composite_shader`) has no f32 fetch, f32 canvas test or float
/// uniform. Everything `main` reaches is integer except the atlas texel in
/// `sample_byte`, which goes straight through `unorm_byte`.
#[test]
fn v3_plain_compositor_reads_no_float() {
    let shader = strip_comments(&crate::integer_composite_shader());
    let functions = wgsl_functions(&shader);
    for gone in ["fetch_legacy", "sample_texel", "fetch"] {
        assert!(!functions.contains_key(gone), "{gone} is still defined");
    }
    let reached = reached_from_main(&functions);
    for needed in [
        "fetch_bytes",
        "fetch_integer",
        "sample_byte",
        "channel_byte",
        "pixel_q8",
        "coord_q8",
        "unorm_byte",
        "blend_q",
    ] {
        assert!(
            reached.iter().any(|r| r == needed),
            "main does not reach {needed}"
        );
    }
    // `origin_scale` and `inv0` are integer fields here (the struct
    // declarations are checked below).
    let tokens = |body: &str| -> Vec<String> {
        float_tokens(body)
            .into_iter()
            .filter(|t| t != "origin_scale" && t != "inv0")
            .collect()
    };
    for name in reached
        .iter()
        .filter(|n| !["unorm_byte", "sample_byte"].contains(&n.as_str()))
    {
        let found = tokens(&functions[name]);
        assert!(
            found.is_empty(),
            "{name} (reached from main) uses {found:?}"
        );
    }
    let sample = &functions["sample_byte"];
    assert_eq!(shader.matches("textureLoad(atlas").count(), 1);
    assert!(sample.contains("textureLoad(atlas"));
    for ch in ["s.r", "s.g", "s.b", "s.a"] {
        assert!(sample.contains(&format!("unorm_byte({ch})")), "{ch}");
    }
    assert!(tokens(&sample.replace("textureLoad(atlas", "")).is_empty());
    for decl in ["struct Command", "struct Config"] {
        let at = &shader[shader.find(decl).unwrap()..];
        let body = &at[..at.find('}').unwrap()];
        assert!(tokens(body).is_empty(), "{body}");
    }
    assert!(shader.contains("all(id.xy>=config.span.xy) && all(id.xy<config.span.zw)"));
    assert!(shader.contains("let fx = select(qx.v - (x << 8), 0, c.info.w == 2u);"));
    assert!(shader.contains("let fy = select(qy.v - (y << 8), 0, c.info.w == 2u);"));
}
/// The v3 resolve span is exactly the pixels whose center is on the canvas,
/// at origins where an f32 center is not exact (past 2^24 px) and on both
/// sides of 0.
#[test]
fn v3_resolve_span_is_exact() {
    let span = super::super::resolve_span;
    assert_eq!(span([0, 0], 1, [256, 256]), [0, 0, 256, 256]);
    assert_eq!(span([999_936, 0], 1, [1_000_036, 77]), [0, 0, 100, 77]);
    assert_eq!(span([-44, 37], 1, [1_000, 400]), [44, 0, 256, 256]);
    assert_eq!(span([-300, 0], 1, [-10, 400]), [0; 4]);
    for (origin, scale) in [
        (0i64, 1i64),
        (-1, 1),
        (16_777_216, 1),
        (33_554_432 + 256, 2),
        (-8_388_608, 4),
        (999_936, 1),
    ] {
        for end in [
            origin - 1,
            origin,
            origin + 1,
            origin + 77,
            origin + 255,
            origin + 256 * scale,
        ] {
            let [x0, _, x1, _] = span([origin, 0], scale, [end, 1 << 40]);
            for i in 0..256i64 {
                // The center in exact doubled integers.
                let c = 2 * origin + (2 * i + 1) * scale;
                let on = c >= 0 && c < 2 * end;
                assert_eq!(
                    (x0..x1).contains(&(i as u32)),
                    on,
                    "origin {origin} scale {scale} end {end} pixel {i}"
                );
            }
        }
    }
    // f32 would round the canvas edge 16777217 to 16777216 and drop pixel 0.
    assert_eq!(span([16_777_216, 0], 1, [16_777_217, 1]), [0, 0, 1, 1]);
    assert_eq!(16_777_217f32, 16_777_216f32);
}
fn grid(level: i32, tx: i64, ty: i64) -> RoiQ {
    RoiQ {
        level,
        ox: tx * 256,
        oy: ty * 256,
        w: 256,
        h: 256,
    }
}
fn same(a: Roi, b: Roi) -> bool {
    a.x.to_bits() == b.x.to_bits()
        && a.y.to_bits() == b.y.to_bits()
        && a.scale.to_bits() == b.scale.to_bits()
        && (a.w, a.h) == (b.w, b.h)
}
#[test]
fn blur_roi_q_matches_f32_roi() {
    let tiles = [-(1i64 << 15), -3, -1, 0, 1, 7, 1 << 15];
    let radii = [1u32, 2, 255, 256, 257, 4096, 8191, 8192, 8193, 16383, 16384];
    for level in -6..=12 {
        for &tx in &tiles {
            for &ty in &tiles {
                let q = grid(level, tx, ty);
                let f = q.to_roi().unwrap();
                assert_eq!(RoiQ::from_roi(f).unwrap(), q);
                for &r8 in &radii {
                    // R / 256 is exact in f32, so v2 and v3 read one radius.
                    let rf = r8 as f32 / 256.0;
                    let k = alto_core::blur::blur_coarsen_k(rf).unwrap();
                    let sample = q.coarsen(k.max(level)).unwrap();
                    let sample_f = f.coarsen(rf / 64.0);
                    assert!(
                        same(sample.to_roi().unwrap(), sample_f),
                        "coarsen level {level} tile ({tx}, {ty}) R {r8}"
                    );
                    let (taps, _) = sample.taps(r8).unwrap();
                    assert_eq!(taps, (rf / sample_f.scale).ceil() as u32);
                    let input = sample.expand(taps, taps).unwrap();
                    assert!(
                        same(input.to_roi().unwrap(), sample_f.expand(taps, taps)),
                        "expand level {level} tile ({tx}, {ty}) R {r8}"
                    );
                }
            }
        }
    }
}
#[test]
fn blur_rejects_out_of_range_levels() {
    for level in [-7, -40, 13, i32::MIN, i32::MAX] {
        let q = RoiQ {
            level,
            ox: 0,
            oy: 0,
            w: 256,
            h: 256,
        };
        assert!(matches!(
            q.taps(16384),
            Err(RenderError::InvalidArgument(_))
        ));
        assert!(matches!(q.coarsen(0), Err(RenderError::InvalidArgument(_))));
        assert!(matches!(
            q.coarsen(i32::MAX),
            Err(RenderError::InvalidArgument(_))
        ));
        let f = Roi {
            x: 0.0,
            y: 0.0,
            scale: 2f32.powi(level.clamp(-126, 127)),
            w: 256,
            h: 256,
        };
        assert!(RoiQ::from_roi(f).is_err());
    }
    // In range, a ratio over 128 and a finer target are refused too.
    assert!(grid(-6, 0, 0).coarsen(2).is_err());
    assert!(grid(0, 0, 0).coarsen(-1).is_err());
    assert!(grid(0, 0, 0).taps(16385).is_err());
}
