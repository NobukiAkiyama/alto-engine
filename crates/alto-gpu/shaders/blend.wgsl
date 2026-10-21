// Shared by composite.wgsl and effects.wgsl. Both shaders prepend this file.
// blend() expects src.rgb straight (unpremultiplied) and src.a separate. dst is
// premultiplied. composite.wgsl unpremultiplies before the call; effects info.x==1
// does the same, so the two paths sample the same pixels. mix_color is the shared
// mode math. Clip blend (effects info.x==9) uses it only for multiply and screen
// and keeps the base alpha; folding that path into blend() would change coverage.
fn lum3(c: vec3f) -> f32 { return dot(c, vec3f(0.2126, 0.7152, 0.0722)); }
fn clip_color(c: vec3f) -> vec3f { let l = lum3(c); let n = min(c.r, min(c.g, c.b)); let x = max(c.r, max(c.g, c.b)); var o = c; if n < 0.0 { o = vec3f(l) + (c - vec3f(l)) * l / max(l - n, 0.000001); } if x > 1.0 { o = vec3f(l) + (o - vec3f(l)) * (1.0 - l) / max(x - l, 0.000001); } return clamp(o, vec3f(0.0), vec3f(1.0)); }
fn set_lum(c: vec3f, l: f32) -> vec3f { return clip_color(c + (vec3f(l) - vec3f(lum3(c)))); }
fn set_sat(c: vec3f, s: f32) -> vec3f { let mn = min(c.r, min(c.g, c.b)); let mx = max(c.r, max(c.g, c.b)); if mx <= mn { return vec3f(0.0); } return (c - vec3f(mn)) * s / max(mx - mn, 0.000001); }
fn soft_channel(ss: f32, dd: f32) -> f32 { let d2 = select(sqrt(dd), ((16.0 * dd - 12.0) * dd + 4.0) * dd, dd <= 0.25); if ss <= 0.5 { return dd - (1.0 - 2.0 * ss) * dd * (1.0 - dd); } return dd + (2.0 * ss - 1.0) * (d2 - dd); }
fn dodge(ss: f32, dd: f32) -> f32 { if ss >= 1.0 { return 1.0; } return min(1.0, dd / max(1.0 - ss, 0.000001)); }
fn burn(ss: f32, dd: f32) -> f32 { if ss <= 0.0 { return 0.0; } return 1.0 - min(1.0, (1.0 - dd) / max(ss, 0.000001)); }
fn mix_color(s: vec3f, d: vec3f, mode: u32) -> vec3f {
  var b = s;
  if mode == 1u { b = d * s; }
  else if mode == 2u { b = 1.0 - (1.0 - d) * (1.0 - s); }
  else if mode == 3u { b = select(1.0 - 2.0 * (1.0 - s) * (1.0 - d), 2.0 * s * d, d < vec3f(0.5)); }
  else if mode == 4u { b = min(s, d); }
  else if mode == 5u { b = max(s, d); }
  else if mode == 6u { b = vec3f(dodge(s.r, d.r), dodge(s.g, d.g), dodge(s.b, d.b)); }
  else if mode == 7u { b = vec3f(burn(s.r, d.r), burn(s.g, d.g), burn(s.b, d.b)); }
  else if mode == 8u { b = select(1.0 - 2.0 * (1.0 - d) * (1.0 - s), 2.0 * d * s, s < vec3f(0.5)); }
  else if mode == 9u { b = vec3f(soft_channel(s.r, d.r), soft_channel(s.g, d.g), soft_channel(s.b, d.b)); }
  else if mode == 10u { b = abs(d - s); }
  else if mode == 11u { b = d + s - 2.0 * d * s; }
  else if mode == 12u { b = set_lum(set_sat(s, max(d.r, max(d.g, d.b)) - min(d.r, min(d.g, d.b))), lum3(d)); }
  else if mode == 13u { b = set_lum(set_sat(d, max(s.r, max(s.g, s.b)) - min(s.r, min(s.g, s.b))), lum3(d)); }
  else if mode == 14u { b = set_lum(s, lum3(d)); }
  else if mode == 15u { b = set_lum(d, lum3(s)); }
  else if mode == 16u { b = min(vec3f(1.0), s + d); }
  return b;
}
fn blend(dst: vec4f, src: vec4f, mode: u32) -> vec4f {
  let d = select(vec3f(0.0), dst.rgb / max(dst.a, 0.000001), dst.a > 0.0);
  let b = mix_color(src.rgb, d, mode);
  let rgb = (1.0-src.a)*dst.rgb + (1.0-dst.a)*src.rgb*src.a + dst.a*src.a*b;
  return vec4f(rgb, src.a + dst.a*(1.0-src.a));
}
