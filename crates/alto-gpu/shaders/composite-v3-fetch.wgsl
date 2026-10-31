fn sample_byte(c: Command, x: i32, y: i32) -> vec4u {
  let key = vec2i(x >> 8, y >> 8);
  var lo = c.info.y; var hi = lo+c.info.z;
  loop {
    if lo >= hi { break; }
    let mid = (lo+hi)/2u; let t = tiles[mid];
    if t.xy.x < key.x || (t.xy.x == key.x && t.xy.y < key.y) { lo=mid+1u; } else { hi=mid; }
  }
  if lo >= c.info.y+c.info.z { return vec4u(0u); }
  let t=tiles[lo];
  if any(t.xy != key) { return vec4u(0u); }
  let local = clamp(vec2i(x, y)-key*256, vec2i(0), vec2i(255));
  if t.slot == 0xffffffffu {
    let on = ((local.x/12+local.y/12)%2)==0;
    return vec4u(255u, select(0u, 64u, on), 51u, 255u);
  }
  let s = textureLoad(atlas, local, i32(t.slot), 0);
  return vec4u(unorm_byte(s.r),unorm_byte(s.g),unorm_byte(s.b),unorm_byte(s.a));
}
fn channel_byte(a: u32, b: u32, d: u32, e: u32, fx: i32, fy: i32) -> u32 {
  let ab = i32(a) * (256 - fx) + i32(b) * fx;
  let de = i32(d) * (256 - fx) + i32(e) * fx;
  let acc = ab * (256 - fy) + de * fy;
  return u32((acc + 32768) >> 16);
}

fn fetch_integer(c: Command, ix: vec2i) -> vec4u {
  let scale = config.origin_scale.z;
  let px = pixel_q8(config.origin_scale.x, ix.x, scale);
  let py = pixel_q8(config.origin_scale.y, ix.y, scale);
  if px.ok == 0u || py.ok == 0u { return vec4u(0u); }
  let m = c.inv0;
  let tx = Wide(c.trans.x, c.trans.y);
  let ty = Wide(c.trans.z, c.trans.w);
  let qx = coord_q8(m.x, m.z, tx, px.v, py.v);
  let qy = coord_q8(m.y, m.w, ty, px.v, py.v);
  if qx.ok == 0u || qy.ok == 0u { return vec4u(0u); }
  let x = qx.v >> 8;
  let y = qy.v >> 8;
  let fx = select(qx.v - (x << 8), 0, c.info.w == 2u);
  let fy = select(qy.v - (y << 8), 0, c.info.w == 2u);
  let a = sample_byte(c, x, y);
  let b = sample_byte(c, x + 1, y);
  let d = sample_byte(c, x, y + 1);
  let e = sample_byte(c, x + 1, y + 1);
  let r = channel_byte(a.x, b.x, d.x, e.x, fx, fy);
  let g = channel_byte(a.y, b.y, d.y, e.y, fx, fy);
  let bl = channel_byte(a.z, b.z, d.z, e.z, fx, fy);
  let al = channel_byte(a.w, b.w, d.w, e.w, fx, fy);
  return vec4u(r,g,bl,al);
}


// The integer 4-tap, or SAMPLE_NEAREST (2) as the 4-tap with zero weights.
// A pixel whose Q8 position leaves i32 is transparent; the host makes sure
// such a pixel reaches no tile, or refuses the tile (renderer::v3_reach). A draw without an integer sampler (SAMPLE_F32, which the
// host refuses in version 3) reads nothing rather than a float fetch. Keep this
// branch: without it SwiftShader (Subzero) takes about 10x longer to compile
// the pipeline, and a second copy of the Q8 position code about 70x.
fn fetch_bytes(c: Command,ix:vec2i)->vec4u {
  if c.info.w!=0u {return fetch_integer(c,ix);}
  return vec4u(0u);
}
