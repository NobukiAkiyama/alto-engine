//! GPU shader output equals the CPU reference, byte for byte.
use super::*;

#[test]
fn gpu_shader_matches_cpu_reference() {
    let _gpu_lock = GPU_RENDERER_TEST_LOCK.lock().unwrap();
    let gpu = match Gpu::new() {
        Ok(Some(gpu)) => gpu,
        Ok(None) => {
            // macOS CI always has Metal. ALTO_REQUIRE_GPU_PARITY makes a missing
            // adapter a failure on lavapipe CI too, instead of a skip.
            if std::env::var_os("ALTO_REQUIRE_GPU_PARITY").is_some() || cfg!(target_os = "macos") {
                panic!("GPU pixel parity requires a Metal, Vulkan, or DX12 adapter");
            }
            eprintln!("gpu pixel parity skipped: no Metal, Vulkan, or DX12 adapter");
            return;
        }
        Err(err) => panic!("GPU pixel parity could not start: {err}"),
    };
    if std::env::var_os("ALTO_REQUIRE_LAVAPIPE").is_some() {
        assert!(
            is_lavapipe_adapter(&gpu.adapter_name),
            "ALTO_REQUIRE_LAVAPIPE=1 but the adapter is {}",
            gpu.adapter_name
        );
    }
    let mut failed = Vec::new();
    let mut measured = Vec::new();
    for case in cases() {
        let (cpu, cpu_pre) = render_prequant(&case);
        let (gpu_tile, gpu_pre) = match gpu.render(&case) {
            Ok(pair) => pair,
            Err(err) => {
                println!("{}: GPU render failed: {err}", case.name);
                failed.push(format!("{}: {err}", case.name));
                continue;
            }
        };
        if case.measure {
            let row = measure_case(&case, &cpu, &gpu_tile, &cpu_pre, &gpu_pre);
            println!("{}", measure_line(&row));
            measured.push(row);
            continue;
        }
        let report = diff_report(&cpu, &gpu_tile);
        println!("{}: {report}", case.name);
        // Enforced cases have no allowlist. Measured cases are skipped above.
        if cpu != gpu_tile {
            failed.push(format!("{}: {report}", case.name));
        }
    }
    write_alpha_measure(&gpu.adapter_name, &measured);
    let mut measured_v3 = Vec::new();
    let mut v3_errors = Vec::new();
    // Version 3 composes only level 0 directly (higher levels are the integer
    // pyramid, checked below). A draw that reaches none of its tiles is skipped,
    // so large-translation-out-of-range is transparent and equals the CPU.
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.name = format!("v3-{}", case.name);
        if case.level > 0 {
            let cpu = render_pyramid(&case);
            match gpu.render_pyramid(&case) {
                Ok(bytes) => {
                    let row = measure_case(&case, &cpu, &bytes, &[], &[]);
                    println!("v3 composite (enforced): {}", measure_line(&row));
                    if cpu != bytes {
                        failed.push(format!("{}: {}", case.name, diff_report(&cpu, &bytes)));
                    }
                    measured_v3.push(row);
                }
                Err(err) => {
                    v3_errors.push(format!("{}: {err}", case.name));
                    failed.push(format!("{}: {err}", case.name));
                }
            }
            continue;
        }
        let (cpu, cpu_pre) = render_pair(&case, 3);
        match gpu.render_version(&case, 3) {
            Ok((bytes, pre)) => {
                let row = measure_case(&case, &cpu, &bytes, &cpu_pre, &pre);
                println!("v3 composite (enforced): {}", measure_line(&row));
                if cpu != bytes {
                    failed.push(format!("{}: {}", case.name, diff_report(&cpu, &bytes)));
                }
                measured_v3.push(row);
            }
            Err(err) => {
                v3_errors.push(format!("{}: {err}", case.name));
                failed.push(format!("{}: {err}", case.name));
            }
        }
    }
    for mut case in cases().into_iter().filter(|c| c.measure) {
        case.level = 1;
        case.tx = 0;
        case.ty = 0;
        case.name = format!("v3-{}-level1", case.name);
        let cpu = render_pyramid(&case);
        match gpu.render_pyramid(&case) {
            Ok(bytes) => {
                let row = measure_case(&case, &cpu, &bytes, &[], &[]);
                if cpu != bytes {
                    failed.push(format!("{}: {}", case.name, diff_report(&cpu, &bytes)));
                }
                measured_v3.push(row);
            }
            Err(err) => {
                v3_errors.push(format!("{}: {err}", case.name));
                failed.push(format!("{}: {err}", case.name));
            }
        }
    }
    write_v3_measure(&gpu.adapter_name, &measured_v3, &v3_errors);

    // Enforced: every byte on every channel must round-trip, in the Rgba8Unorm
    // raster read and the R8Unorm glyph atlas read. Stored first, then failed.
    let atlas = gpu.atlas_read(false);
    let atlas_r8 = gpu.atlas_read(true);
    for (format, result) in [("Rgba8Unorm", &atlas), ("R8Unorm", &atlas_r8)] {
        match result {
            Ok(r) => {
                println!("{}", r.line());
                if !r.mismatches.is_empty() {
                    failed.push(r.line());
                }
            }
            Err(e) => {
                println!("atlas-read {format} (enforced): GPU error: {e}");
                failed.push(format!("atlas-read {format}: {e}"));
            }
        }
    }
    write_atlas_read(&gpu.adapter_name, &atlas, &atlas_r8);
    // Enforced: the v3 M5 color-adjust chain and opacity 0.5, integer-exact.
    let src = color_adjust_gradient();
    let adjust: Vec<_> = color_adjust_grid()
        .into_iter()
        .map(color_adjust_program)
        .collect();
    let images = opacity_images();
    let opacity = opacity_programs();
    // Enforced: the v3 pixel-mask stages (modes 21, 22 and 23).
    let mask_atlas = mask_images();
    let masks = mask_programs();
    // Enforced: the same stages at non-zero and negative ROI origins, and with
    // inputs at other ROIs (the integer texel map).
    let origins = mask_origin_programs();
    // Enforced: the float purge, modes 18, 17, 19, 8 and 20 placed by integers
    // at far ROIs, with transformed rasters.
    let purge = purge_programs();
    for (images, programs) in [
        (std::slice::from_ref(&src), &adjust),
        (&images[..], &opacity),
        (&mask_atlas[..], &masks),
        (&mask_atlas[..], &origins),
        (&mask_atlas[..], &purge),
    ] {
        match gpu.render_q14(images, programs) {
            Ok(tiles) => {
                if tiles.len() != programs.len() {
                    failed.push(format!(
                        "q14 stages: {} GPU tiles for {} programs",
                        tiles.len(),
                        programs.len()
                    ));
                }
                for (tile, program) in tiles.iter().zip(programs.iter()) {
                    let cpu = q14_program_cpu(images, program);
                    let report = diff_report(&cpu, tile);
                    println!("{} (v3 M5 integer, enforced): {report}", program.name);
                    if cpu != *tile {
                        failed.push(format!("{}: {report}", program.name));
                    }
                }
            }
            Err(err) => {
                println!("q14 stages: GPU render failed: {err}");
                failed.push(format!("q14 stages: {err}"));
            }
        }
    }
    // Enforced: v3 scenes through the renderer, so every uniform, the raster
    // and mask tiles `Context::lookup_entries` selects and uploads, and the
    // resolve are the host's own. Refused cases must fail with GpuUnsupported.
    let cases = renderer_cases();
    match renderer_gpu(&cases) {
        Ok(tiles) => {
            for (tile, case) in tiles.iter().zip(&cases) {
                match (tile, case.refused) {
                    (Ok(tile), None) => {
                        let cpu = renderer_case_cpu(case);
                        let report = diff_report(&cpu, tile);
                        println!("{} (v3 renderer, enforced): {report}", case.name);
                        if cpu != *tile {
                            failed.push(format!("{}: {report}", case.name));
                        }
                    }
                    (Err(crate::RenderError::GpuUnsupported(m)), Some(want)) if m == want => {
                        println!("{} (v3 renderer, enforced): refused: {m}", case.name);
                    }
                    (result, want) => {
                        let got = result.as_ref().map(|_| "a tile").map_err(|e| e.to_string());
                        println!("{}: expected {want:?}, got {got:?}", case.name);
                        failed.push(format!("{}: expected {want:?}, got {got:?}", case.name));
                    }
                }
            }
        }
        Err(err) => {
            println!("renderer cases: GPU render failed: {err}");
            failed.push(format!("renderer cases: {err}"));
        }
    }
    // Enforced (Linus 04:40): the same scene read warm after `scene_damage` and
    // cold gives the same bytes or the same refusal, and drawn tiles equal the CPU.
    let cold_warm = cold_warm_cases();
    match renderer_cold_warm(&cold_warm) {
        Ok(results) => {
            for (case, per_key) in cold_warm.iter().zip(results) {
                for (((level, x, y), want), (warm, cold)) in case.keys.iter().zip(per_key) {
                    let name = format!("cold/warm {} ({level}, {x}, {y})", case.name);
                    let refusal = |r: &Result<Vec<u8>, crate::RenderError>| match r {
                        Err(crate::RenderError::GpuUnsupported(m)) => Some(m.clone()),
                        _ => None,
                    };
                    match (&warm, &cold) {
                        (Ok(w), Ok(c)) if want.is_none() => {
                            let cpu = renderer_case_cpu(&RendererCase {
                                level: *level,
                                key: [*x, *y],
                                ..RendererCase {
                                    name: name.clone(),
                                    ..case.after.clone_layers()
                                }
                            });
                            if w != c || *c != cpu {
                                failed.push(format!(
                                    "{name}: warm vs cold {}; cold vs CPU {}",
                                    diff_report(c, w),
                                    diff_report(&cpu, c)
                                ));
                            } else {
                                println!("{name} (enforced): warm = cold = CPU");
                            }
                        }
                        _ if want.is_some()
                            && refusal(&warm).as_deref() == *want
                            && refusal(&cold).as_deref() == *want =>
                        {
                            println!(
                                "{name} (enforced): refused warm and cold: {}",
                                want.unwrap()
                            );
                        }
                        _ => failed.push(format!(
                            "{name}: expected {want:?}, warm {:?}, cold {:?}",
                            warm.as_ref().map(|_| "tile").map_err(|e| e.to_string()),
                            cold.as_ref().map(|_| "tile").map_err(|e| e.to_string())
                        )),
                    }
                }
            }
        }
        Err(err) => failed.push(format!("cold/warm: {err}")),
    }
    assert!(
        failed.is_empty(),
        "GPU and CPU pixels differ:\n{}",
        failed.join("\n")
    );
}
