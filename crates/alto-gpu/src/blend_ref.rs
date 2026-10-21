//! CPU reference for `shaders/blend.wgsl` and the two callers that must agree.
//!
//! `blend()` takes straight source rgb. `composite.wgsl` unpremultiplies the
//! group accumulator before the call. `effects.wgsl` info.x==1 does the same
//! to a premultiplied texture sample. The previous effects function
//! unpremultiplied inside the blend and composited premultiplied source; those
//! pixels match this calling convention. Clip blend (info.x==9) keeps base
//! alpha and only special-cases multiply and screen, so it calls `mix_color`
//! for modes 0..=2 and leaves every other mode as normal.

fn select3(f: [f32; 3], t: [f32; 3], cond: [bool; 3]) -> [f32; 3] {
    [
        if cond[0] { t[0] } else { f[0] },
        if cond[1] { t[1] } else { f[1] },
        if cond[2] { t[2] } else { f[2] },
    ]
}
fn lum3(c: [f32; 3]) -> f32 {
    c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2]]
}
fn scale(a: [f32; 3], s: f32) -> [f32; 3] {
    [a[0] * s, a[1] * s, a[2] * s]
}
fn splat(v: f32) -> [f32; 3] {
    [v, v, v]
}
fn clamp01(c: [f32; 3]) -> [f32; 3] {
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
    ]
}
fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum3(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut o = c;
    if n < 0.0 {
        o = add(splat(l), scale(sub(c, splat(l)), l / (l - n).max(0.000001)));
    }
    if x > 1.0 {
        o = add(
            splat(l),
            scale(sub(o, splat(l)), (1.0 - l) / (x - l).max(0.000001)),
        );
    }
    clamp01(o)
}
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    clip_color(add(c, splat(l - lum3(c))))
}
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mn = c[0].min(c[1]).min(c[2]);
    let mx = c[0].max(c[1]).max(c[2]);
    if mx <= mn {
        return [0.0; 3];
    }
    scale(sub(c, splat(mn)), s / (mx - mn).max(0.000001))
}
fn soft_channel(ss: f32, dd: f32) -> f32 {
    let d2 = if dd <= 0.25 {
        ((16.0 * dd - 12.0) * dd + 4.0) * dd
    } else {
        dd.sqrt()
    };
    if ss <= 0.5 {
        dd - (1.0 - 2.0 * ss) * dd * (1.0 - dd)
    } else {
        dd + (2.0 * ss - 1.0) * (d2 - dd)
    }
}
fn dodge(ss: f32, dd: f32) -> f32 {
    if ss >= 1.0 {
        1.0
    } else {
        (dd / (1.0 - ss).max(0.000001)).min(1.0)
    }
}
fn burn(ss: f32, dd: f32) -> f32 {
    if ss <= 0.0 {
        0.0
    } else {
        1.0 - ((1.0 - dd) / ss.max(0.000001)).min(1.0)
    }
}
fn mix_color(s: [f32; 3], d: [f32; 3], mode: u32) -> [f32; 3] {
    match mode {
        1 => mul(d, s),
        2 => sub(splat(1.0), mul(sub(splat(1.0), d), sub(splat(1.0), s))),
        3 => select3(
            sub(
                splat(1.0),
                scale(mul(sub(splat(1.0), s), sub(splat(1.0), d)), 2.0),
            ),
            scale(mul(s, d), 2.0),
            [d[0] < 0.5, d[1] < 0.5, d[2] < 0.5],
        ),
        4 => [s[0].min(d[0]), s[1].min(d[1]), s[2].min(d[2])],
        5 => [s[0].max(d[0]), s[1].max(d[1]), s[2].max(d[2])],
        6 => [dodge(s[0], d[0]), dodge(s[1], d[1]), dodge(s[2], d[2])],
        7 => [burn(s[0], d[0]), burn(s[1], d[1]), burn(s[2], d[2])],
        8 => select3(
            sub(
                splat(1.0),
                scale(mul(sub(splat(1.0), d), sub(splat(1.0), s)), 2.0),
            ),
            scale(mul(d, s), 2.0),
            [s[0] < 0.5, s[1] < 0.5, s[2] < 0.5],
        ),
        9 => [
            soft_channel(s[0], d[0]),
            soft_channel(s[1], d[1]),
            soft_channel(s[2], d[2]),
        ],
        10 => [
            (d[0] - s[0]).abs(),
            (d[1] - s[1]).abs(),
            (d[2] - s[2]).abs(),
        ],
        11 => sub(add(d, s), scale(mul(d, s), 2.0)),
        12 => set_lum(
            set_sat(s, d[0].max(d[1]).max(d[2]) - d[0].min(d[1]).min(d[2])),
            lum3(d),
        ),
        13 => set_lum(
            set_sat(d, s[0].max(s[1]).max(s[2]) - s[0].min(s[1]).min(s[2])),
            lum3(d),
        ),
        14 => set_lum(s, lum3(d)),
        15 => set_lum(d, lum3(s)),
        16 => [
            (s[0] + d[0]).min(1.0),
            (s[1] + d[1]).min(1.0),
            (s[2] + d[2]).min(1.0),
        ],
        _ => s,
    }
}

/// `src.rgb` is straight. `dst` is premultiplied. Mirrors `fn blend` in blend.wgsl.
fn blend(dst: [f32; 4], src: [f32; 4], mode: u32) -> [f32; 4] {
    let d = if dst[3] > 0.0 {
        [
            dst[0] / dst[3].max(0.000001),
            dst[1] / dst[3].max(0.000001),
            dst[2] / dst[3].max(0.000001),
        ]
    } else {
        [0.0; 3]
    };
    let b = mix_color([src[0], src[1], src[2]], d, mode);
    let rgb = [
        (1.0 - src[3]) * dst[0] + (1.0 - dst[3]) * src[0] * src[3] + dst[3] * src[3] * b[0],
        (1.0 - src[3]) * dst[1] + (1.0 - dst[3]) * src[1] * src[3] + dst[3] * src[3] * b[1],
        (1.0 - src[3]) * dst[2] + (1.0 - dst[3]) * src[2] * src[3] + dst[3] * src[3] * b[2],
    ];
    [rgb[0], rgb[1], rgb[2], src[3] + dst[3] * (1.0 - src[3])]
}

fn straight_rgb(c: [f32; 4]) -> [f32; 4] {
    if c[3] > 0.0 {
        [
            c[0] / c[3].max(0.000001),
            c[1] / c[3].max(0.000001),
            c[2] / c[3].max(0.000001),
            c[3],
        ]
    } else {
        [0.0, 0.0, 0.0, c[3]]
    }
}

/// effects.wgsl info.x==1: clip the premultiplied sample, then unpremultiply.
fn effects_blend(dst: [f32; 4], src_premul: [f32; 4], mode: u32) -> [f32; 4] {
    blend(dst, straight_rgb(src_premul), mode)
}

/// composite.wgsl group pop: unpremultiply, then scale alpha by the layer opacity.
fn composite_blend(dst: [f32; 4], accum_premul: [f32; 4], opacity: f32, mode: u32) -> [f32; 4] {
    let mut src = straight_rgb(accum_premul);
    src[3] *= opacity;
    blend(dst, src, mode)
}

/// The effects blend that lived in effects.wgsl before it called the shared function.
fn previous_effects_blend(dst: [f32; 4], src: [f32; 4], mode: u32) -> [f32; 4] {
    let s = if src[3] > 0.0 {
        [
            src[0] / src[3].max(0.000001),
            src[1] / src[3].max(0.000001),
            src[2] / src[3].max(0.000001),
        ]
    } else {
        [0.0; 3]
    };
    let d = if dst[3] > 0.0 {
        [
            dst[0] / dst[3].max(0.000001),
            dst[1] / dst[3].max(0.000001),
            dst[2] / dst[3].max(0.000001),
        ]
    } else {
        [0.0; 3]
    };
    let mixed = mix_color(s, d, mode);
    let rgb = [
        (1.0 - src[3]) * dst[0] + (1.0 - dst[3]) * src[0] + dst[3] * src[3] * mixed[0],
        (1.0 - src[3]) * dst[1] + (1.0 - dst[3]) * src[1] + dst[3] * src[3] * mixed[1],
        (1.0 - src[3]) * dst[2] + (1.0 - dst[3]) * src[2] + dst[3] * src[3] * mixed[2],
    ];
    [rgb[0], rgb[1], rgb[2], src[3] + dst[3] * (1.0 - src[3])]
}

fn previous_clip(base: [f32; 4], src: [f32; 4], mode: u32) -> [f32; 4] {
    let d = [
        base[0] / base[3].max(0.000001),
        base[1] / base[3].max(0.000001),
        base[2] / base[3].max(0.000001),
    ];
    let s = [
        src[0] / src[3].max(0.000001),
        src[1] / src[3].max(0.000001),
        src[2] / src[3].max(0.000001),
    ];
    let mut blended = s;
    if mode == 1 {
        blended = mul(d, s);
    }
    if mode == 2 {
        blended = sub(splat(1.0), mul(sub(splat(1.0), d), sub(splat(1.0), s)));
    }
    let rgb = scale(add(scale(d, 1.0 - src[3]), scale(blended, src[3])), base[3]);
    [rgb[0], rgb[1], rgb[2], base[3]]
}

fn shared_clip(base: [f32; 4], src: [f32; 4], mode: u32) -> [f32; 4] {
    let d = [
        base[0] / base[3].max(0.000001),
        base[1] / base[3].max(0.000001),
        base[2] / base[3].max(0.000001),
    ];
    let s = [
        src[0] / src[3].max(0.000001),
        src[1] / src[3].max(0.000001),
        src[2] / src[3].max(0.000001),
    ];
    let mode = if mode <= 2 { mode } else { 0 };
    let blended = mix_color(s, d, mode);
    let rgb = scale(add(scale(d, 1.0 - src[3]), scale(blended, src[3])), base[3]);
    [rgb[0], rgb[1], rgb[2], base[3]]
}

fn close(a: [f32; 4], b: [f32; 4]) {
    for i in 0..4 {
        assert!((a[i] - b[i]).abs() < 1e-5, "{a:?} != {b:?} at {i}");
    }
}

#[test]
fn effects_blend_matches_composite_and_the_previous_formula() {
    let effects = include_str!("../shaders/effects.wgsl");
    let blend_src = include_str!("../shaders/blend.wgsl");
    assert!(
        effects.contains("let straight=select(vec3f(0.0),src.rgb/max(src.a,0.000001),src.a>0.0);")
    );
    assert!(effects.contains("out=blend(load_a(p),vec4f(straight,src.a),u32(c.settings.y));"));
    assert!(blend_src.contains("fn mix_color"));
    assert!(effects.contains("mix_color(s,d,select(0u,mode,mode<=2u))"));

    let samples = [
        ([0.2, 0.0, 0.0, 0.5], [0.0, 0.3, 0.0, 0.5]),
        ([0.0, 0.0, 0.0, 0.0], [0.4, 0.2, 0.1, 0.8]),
        ([1.0, 0.5, 0.25, 1.0], [0.0, 0.0, 0.0, 0.0]),
        ([0.1, 0.2, 0.3, 0.4], [0.5, 0.4, 0.3, 0.7]),
    ];
    for (dst, src) in samples {
        for mode in 0..=16 {
            let effects_px = effects_blend(dst, src, mode);
            close(effects_px, composite_blend(dst, src, 1.0, mode));
            close(effects_px, previous_effects_blend(dst, src, mode));
            let mut faded = straight_rgb(src);
            faded[3] *= 0.5;
            close(
                blend(dst, faded, mode),
                composite_blend(dst, src, 0.5, mode),
            );
        }
    }
    // Normal, hand-expanded: dst (0.2,0,0,0.5), src premul (0,0.3,0,0.5).
    close(
        effects_blend([0.2, 0.0, 0.0, 0.5], [0.0, 0.3, 0.0, 0.5], 0),
        [0.1, 0.3, 0.0, 0.75],
    );

    let base = [0.2, 0.0, 0.0, 0.5];
    let src = [0.0, 0.3, 0.0, 0.5];
    for mode in [0, 1, 2, 5, 9] {
        close(shared_clip(base, src, mode), previous_clip(base, src, mode));
    }
    let lighten = mix_color([0.0, 0.6, 0.0], [0.4, 0.0, 0.0], 5);
    let normal = mix_color([0.0, 0.6, 0.0], [0.4, 0.0, 0.0], 0);
    assert_ne!(lighten, normal);
}
