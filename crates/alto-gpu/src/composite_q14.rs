//! Integer compositing reference. Every product and sum is checked, including tests.
use super::{Q14_LUMA, byte_q14, div_q_i64, premul_byte_q14};
pub(super) const Q: i64 = 16384;
fn m(a: i64, b: i64) -> i64 {
    a.checked_mul(b).expect("Q14 product overflow")
}
fn a(x: i64, y: i64) -> i64 {
    x.checked_add(y).expect("Q14 sum overflow")
}
fn d(n: i64, den: i64) -> i64 {
    div_q_i64(n, den).expect("Q14 division overflow")
}
fn clamp(x: i64) -> i64 {
    x.clamp(0, Q)
}
fn mul(x: i64, y: i64) -> i64 {
    d(m(x, y), Q)
}
pub(super) fn opacity(v: f32) -> i64 {
    i64::from(crate::composite_opacity_q14(v))
}

pub(super) fn straight(p: [i64; 4]) -> [i64; 3] {
    std::array::from_fn(|i| {
        if p[3] == 0 {
            0
        } else {
            clamp(d(m(p[i], Q), p[3]))
        }
    })
}
fn lum(c: [i64; 3]) -> i64 {
    d(
        c.into_iter()
            .zip(Q14_LUMA)
            .fold(0, |s, (c, w)| a(s, m(c, i64::from(w)))),
        Q,
    )
}
fn sat(c: [i64; 3]) -> i64 {
    c.into_iter().max().unwrap() - c.into_iter().min().unwrap()
}
fn set_sat(c: [i64; 3], s: i64) -> [i64; 3] {
    let n = *c.iter().min().unwrap();
    let x = *c.iter().max().unwrap();
    c.map(|v| if x == n { 0 } else { d(m(v - n, s), x - n) })
}
fn clip(c: [i64; 3]) -> [i64; 3] {
    let l = lum(c);
    let n = *c.iter().min().unwrap();
    let x = *c.iter().max().unwrap();
    let mut o = c;
    if n < 0 {
        o = if l - n <= 0 {
            [l; 3]
        } else {
            c.map(|v| a(l, d(m(v - l, l), l - n)))
        };
    }
    if x > Q {
        o = if x - l <= 0 {
            [l; 3]
        } else {
            o.map(|v| a(l, d(m(v - l, Q - l), x - l)))
        };
    }
    o.map(clamp)
}
fn set_lum(c: [i64; 3], l: i64) -> [i64; 3] {
    let delta = l - lum(c);
    clip(c.map(|v| a(v, delta)))
}
// Keep the memo's exact binary-search procedure in all three languages.
#[allow(clippy::manual_div_ceil)]
fn floor_isqrt(n: u32) -> u32 {
    let (mut lo, mut hi) = (0, 16384);
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        if mid * mid <= n {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}
fn polynomial(v: i64) -> i64 {
    d(
        a(
            a(m(m(m(16, v), v), v), -m(m(m(12, v), v), Q)),
            m(m(4, v), m(Q, Q)),
        ),
        m(Q, Q),
    )
}
#[allow(clippy::int_plus_one)] // Match the specified rem >= r + 1 test.
fn channel(dst: i64, src: i64, mode: u32) -> i64 {
    let overlay = |s: i64, t: i64| {
        if t < 8192 {
            d(m(m(2, s), t), Q)
        } else {
            Q - d(m(m(2, Q - s), Q - t), Q)
        }
    };
    clamp(match mode {
        0 => src,
        1 => mul(dst, src),
        2 => Q - mul(Q - dst, Q - src),
        3 => overlay(src, dst),
        4 => dst.min(src),
        5 => dst.max(src),
        6 => {
            if src >= Q {
                Q
            } else {
                Q.min(d(m(dst, Q), Q - src))
            }
        }
        7 => {
            if src == 0 {
                0
            } else {
                Q - Q.min(d(m(Q - dst, Q), src))
            }
        }
        8 => overlay(dst, src),
        9 => {
            if src <= 8192 {
                dst - d(m(m(Q - 2 * src, dst), Q - dst), m(Q, Q))
            } else {
                let d2 = if dst <= 4096 {
                    polynomial(dst)
                } else {
                    let n = m(dst, Q) as u32;
                    let r = floor_isqrt(n);
                    i64::from(r + u32::from(n - r * r >= r + 1))
                };
                a(dst, d(m(2 * src - Q, d2 - dst), Q))
            }
        }
        10 => (dst - src).abs(),
        11 => a(dst, src) - d(m(m(2, dst), src), Q),
        16 => Q.min(a(dst, src)),
        _ => unreachable!(),
    })
}
fn mix(dst: [i64; 3], src: [i64; 3], mode: u32) -> [i64; 3] {
    match mode {
        12 => set_lum(set_sat(src, sat(dst)), lum(dst)),
        13 => set_lum(set_sat(dst, sat(src)), lum(dst)),
        14 => set_lum(src, lum(dst)),
        15 => set_lum(dst, lum(src)),
        _ => std::array::from_fn(|i| channel(dst[i], src[i], mode)),
    }
}
// For 0 <= dp <= da <= Q and s, B in [0,Q], the common-denominator
// alpha numerator minus this RGB numerator is at least
// sa*(Q-s)*(Q-da) + (da-dp)*Q*(Q-sa) >= 0. Half-away division on
// nonnegative values is monotone, so rounding preserves RGB <= alpha.
// Normal likewise compares src_pre <= sa and dst_pre <= da with the same
// denominator. Neither path needs min(RGB, alpha).
fn numerator(dp: i64, da: i64, ss: i64, sa: i64, b: i64) -> i64 {
    a(
        a(m(m(Q - sa, dp), Q), m(m(Q - da, ss), sa)),
        m(m(da, sa), b),
    )
}
pub(super) fn blend(dst: [i64; 4], src: [i64; 3], pre: [i64; 3], sa: i64, mode: u32) -> [i64; 4] {
    let da = dst[3];
    let mut out = [0; 4];
    out[3] = d(a(m(sa, Q), m(da, Q - sa)), Q);
    let b = if mode == 0 {
        src
    } else {
        mix(straight(dst), src, mode)
    };
    for i in 0..3 {
        out[i] = if mode == 0 {
            d(a(m(pre[i], Q), m(dst[i], Q - sa)), Q)
        } else {
            d(numerator(dst[i], da, src[i], sa, clamp(b[i])), m(Q, Q))
        };
    }
    assert!(out[..3].iter().all(|v| *v <= out[3]));
    out
}
pub(super) fn draw(dst: [i64; 4], bytes: [u8; 4], op: i64, mode: u32) -> [i64; 4] {
    let src = std::array::from_fn(|i| i64::from(byte_q14(u32::from(bytes[i]))));
    let pre = std::array::from_fn(|i| {
        mul(
            i64::from(premul_byte_q14(u32::from(bytes[i]), u32::from(bytes[3]))),
            op,
        )
    });
    blend(
        dst,
        src,
        pre,
        mul(i64::from(byte_q14(u32::from(bytes[3]))), op),
        mode,
    )
}
pub(super) fn pop(dst: [i64; 4], child: [i64; 4], op: i64, mode: u32) -> [i64; 4] {
    let src = straight(child);
    let sa = mul(child[3], op);
    blend(dst, src, src.map(|v| mul(v, sa)), sa, mode)
}
pub(super) fn bytes(p: [i64; 4]) -> [u8; 4] {
    p.map(|v| d(m(v, 255), Q).clamp(0, 255) as u8)
}

#[test]
fn composite_q14_blend_does_not_overflow() {
    const E: [i64; 10] = [0, 1, 4095, 4096, 4097, 8191, 8192, 8193, 16383, 16384];
    for dp in E {
        for da in E {
            for ss in E {
                for sa in E {
                    for b in E {
                        let n = numerator(dp, da, ss, sa, b);
                        assert!((0..=4_398_046_511_104).contains(&n));
                        if dp <= da {
                            assert!(d(n, m(Q, Q)) <= d(a(m(sa, Q), m(da, Q - sa)), Q));
                        }
                    }
                }
            }
        }
    }
    for da in E {
        for sa in E {
            for dp in E {
                for sp in E {
                    if dp <= da && sp <= sa {
                        let rgb = d(a(m(sp, Q), m(dp, Q - sa)), Q);
                        let alpha = d(a(m(sa, Q), m(da, Q - sa)), Q);
                        assert!(rgb <= alpha);
                    }
                }
            }
        }
    }
    for dst in E {
        for src in E {
            for mode in 0..=16 {
                if !(12..=15).contains(&mode) {
                    assert!((0..=Q).contains(&channel(dst, src, mode)));
                }
            }
        }
    }
    // Each RGB triple, each pair of colors and both alpha endpoints: 100 million points.
    let colors: Vec<_> = E
        .into_iter()
        .flat_map(|r| E.into_iter().flat_map(move |g| E.map(|b| [r, g, b])))
        .collect();
    for dst in &colors {
        for src in &colors {
            for mode in 12..=15 {
                let b = mix(*dst, *src, mode);
                assert!(b.iter().all(|v| (0..=Q).contains(v)));
                for da in E {
                    for sa in E {
                        for i in 0..3 {
                            let dp = mul(dst[i], da);
                            let n = numerator(dp, da, src[i], sa, b[i]);
                            assert!(n <= 4_398_046_511_104);
                            assert!(d(n, m(Q, Q)) <= d(a(m(sa, Q), m(da, Q - sa)), Q));
                        }
                    }
                }
            }
        }
    }
    for v in 0..=4096 {
        assert!(m(m(m(16, v), v), v) <= 1_099_511_627_776);
        assert!((0..=Q).contains(&polynomial(v)));
    }
    let ce = [-16384, -1, 0, 1, 8192, 16384, 16385, 32768];
    for r in ce {
        for g in ce {
            for b in ce {
                let c = [r, g, b];
                let l = lum(c);
                for v in c {
                    assert!(m(v - l, l).abs() <= 1_610_612_736);
                }
                assert!(clip(c).iter().all(|v| (0..=Q).contains(v)));
            }
        }
    }
}
