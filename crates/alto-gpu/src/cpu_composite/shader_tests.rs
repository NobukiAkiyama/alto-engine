//! Integer shader validation and the remaining v3 host checks.
use super::*;

#[test]
fn v3_hosts_refuse_blend_modes_17_and_up() {
    // RenderBlend has variants 0..=16 only, so a v3 RenderScene cannot carry 17.
    assert_eq!(RenderBlend::Add as u32, 16);
    for mode in [0u32, 16] {
        let mut ops = Vec::new();
        walk(
            &raster(solid(1, 1, [0; 4]), mode, 1.0, true, ident()),
            ident(),
            &mut ops,
        );
        assert!(!has_unspecified_v3_mode(&ops), "mode {mode}");
    }
    for mode in [17u32, 18, 255] {
        let mut ops = Vec::new();
        walk(
            &raster(solid(1, 1, [0; 4]), mode, 1.0, true, ident()),
            ident(),
            &mut ops,
        );
        assert!(has_unspecified_v3_mode(&ops), "draw mode {mode}");
        ops.clear();
        let mut g = group(true, 0.5, Vec::new());
        if let Node::Group { mode: m, .. } = &mut g {
            *m = mode;
        }
        walk(&g, ident(), &mut ops);
        assert!(has_unspecified_v3_mode(&ops), "pop mode {mode}");
    }
}

#[test]
fn m5_opacity_half_over_blue_is_the_known_pixel() {
    let images = opacity_images();
    let programs = opacity_programs();
    let half = programs
        .iter()
        .find(|p| p.name == "m5-opacity-half")
        .unwrap();
    let tile = q14_program_cpu(&images, half);
    // sa = 8192, pre = 8192 red; blue keeps 16384 * (1 - 0.5). 8192 * 255 / 16384
    // is the tie 127.5, which goes up.
    assert_eq!(pixel(&tile, 7, 9), [128, 0, 128, 255]);
    assert!(tile.chunks(4).all(|p| p == &tile[..4]));
    // The Q14 program reproduces the color-adjust reference used by the browser.
    let src = color_adjust_gradient();
    for settings in color_adjust_grid() {
        let program = color_adjust_program(settings);
        assert_eq!(
            q14_program_cpu(std::slice::from_ref(&src), &program),
            color_adjust_image_v3(&src, settings.2, settings.0, settings.1),
            "{}",
            program.name
        );
    }
    // Every half-opacity program differs from the same graph at opacity 1.
    for program in &programs {
        let mut opaque = Q14Program {
            name: program.name.clone(),
            targets: program.targets,
            regions: program.regions.clone(),
            stages: program.stages.clone(),
            result: program.result,
        };
        for stage in &mut opaque.stages {
            if matches!(stage.mode, 19 | 20) {
                stage.settings[0] = q14::Q as i32;
            }
        }
        assert_ne!(
            q14_program_cpu(&images, program),
            q14_program_cpu(&images, &opaque),
            "{}",
            program.name
        );
    }
}

#[test]
fn integer_effects_shader_has_no_float_targets_and_validates() {
    let shader = crate::integer_effects_shader();
    assert!(shader.contains("var output: texture_storage_2d<rgba16uint,write>"));
    assert!(shader.contains("var a: texture_2d<u32>"));
    let resolve = include_str!("../../shaders/effects-resolve-q14.wgsl");
    assert!(resolve.contains("texture_storage_2d_array<rgba8uint,write>"));
    for text in [shader.as_str(), resolve] {
        for forbidden in [
            "rgba16float",
            "rgba8unorm",
            "textureSample",
            "f16",
            "round(",
            "=sqrt(",
            "= sqrt(",
        ] {
            assert!(!text.contains(forbidden), "v3 M5 shader uses {forbidden}");
        }
    }
    let v2 = include_str!("../../shaders/effects.wgsl");
    for gone in ["f16", "Q14", "info.x==17u", "info.x==18u", "info.x==19u"] {
        assert!(!v2.contains(gone), "v1/v2 effects.wgsl still has {gone}");
    }
    // The third input is the v3 vector coverage used by mode 29.
    assert!(shader.contains("@binding(2)"));
    for text in [shader, resolve.to_string()] {
        let module = naga::front::wgsl::parse_str(&text)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(&text)));
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}

#[test]
fn composite_v3_rust_and_js_references_agree_byte_for_byte() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let js = std::process::Command::new("node")
        .arg(root.join("tests/e2e/composite-v3-reference.mjs"))
        .output()
        .expect("node is required to compare the Rust and JavaScript v3 references");
    assert!(
        js.status.success(),
        "JS reference failed: {}",
        String::from_utf8_lossy(&js.stderr)
    );
    let mut data = js.stdout.as_slice();
    let mut count = 0;
    for case in cases().into_iter().filter(|c| c.measure) {
        let end = data.iter().position(|b| *b == b'\n').expect("JS case name");
        assert_eq!(std::str::from_utf8(&data[..end]).unwrap(), case.name);
        data = &data[end + 1..];
        let (bytes, rest) = data.split_at(256 * 256 * 4);
        let rust = render_at(&case, 3);
        assert!(
            rust == bytes,
            "{}: {}",
            case.name,
            diff_report(&rust, bytes)
        );
        data = rest;
        count += 1;
    }
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.level = 1;
        case.tx = 0;
        case.ty = 0;
        let end = data
            .iter()
            .position(|b| *b == b'\n')
            .expect("JS pyramid case name");
        assert_eq!(
            std::str::from_utf8(&data[..end]).unwrap(),
            format!("{}-level1", case.name)
        );
        data = &data[end + 1..];
        let (bytes, rest) = data.split_at(256 * 256 * 4);
        let rust = render_pyramid(&case);
        assert!(
            rust == bytes,
            "{} level1: {}",
            case.name,
            diff_report(&rust, bytes)
        );
        data = rest;
    }
    assert!(data.is_empty(), "JS produced extra cases");
    assert!(count >= 37);
}

#[test]
fn integer_composite_shader_has_only_integer_color_output() {
    let shader = crate::integer_composite_shader();
    assert!(shader.contains("var<storage, read_write> output: array<u32>"));
    assert!(shader.contains("opacity_q: u32"));
    assert!(shader.contains("unorm_byte(s.r)"));
    for forbidden in [
        "textureStore",
        "textureSample",
        "rgba8unorm",
        "rgba16float",
        "round(",
        "=sqrt(",
        "= sqrt(",
    ] {
        assert!(
            !shader.contains(forbidden),
            "unexpected v3 shader operation: {forbidden}"
        );
    }
}

#[test]
fn v3_isolation_uses_document_opacity_before_q14_quantization() {
    let almost_one = f32::from_bits(1.0f32.to_bits() - 1);
    assert_eq!(crate::composite_opacity_q14(almost_one), 16384);
    let mut ops = Vec::new();
    walk(&group(false, almost_one, Vec::new()), ident(), &mut ops);
    assert!(matches!(ops.as_slice(), [Op::Push, Op::Pop { .. }]));
    ops.clear();
    walk(&group(false, 1.0, Vec::new()), ident(), &mut ops);
    assert!(ops.is_empty());
}

#[test]
fn integer_parity_shader_records_the_q14_accumulator_and_validates() {
    let shader = integer_parity_shader();
    assert!(shader.contains("pre_quant[id.y*256u+id.x]=vec4f(accum)/16384.0;"));
    let module = naga::front::wgsl::parse_str(&shader).unwrap();
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .unwrap();
}
