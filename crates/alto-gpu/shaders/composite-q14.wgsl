// Appended to the existing fixed-point sampler and effects Q14 helpers.
const Q: i32 = 16384;
fn mul_q(a: i32, b: i32) -> i32 { return div_q(a*b,Q); }
fn wide_div_q28(n: Wide) -> i32 {
  let neg = wide_negative(n);
  var mag = n; if neg { mag=wide_neg(n); }
  let adj=wide_add(mag,Wide(134217728u,0u));
  var q=i32((adj.lo>>28u)|(adj.hi<<4u));
  if neg {q=-q;} return q;
}
fn triple(a: i32,b: i32,c: i32) -> Wide { return wide_mul_i32(a*b,c); }
fn lum_q(c: vec3i) -> i32 { return div_q(c.r*3483+c.g*11718+c.b*1183,Q); }
fn sat_q(c: vec3i) -> i32 {return max(max(c.x,c.y),c.z)-min(min(c.x,c.y),c.z);}
fn clip_q(c: vec3i) -> vec3i {
  let l=lum_q(c); let n=min(min(c.x,c.y),c.z);let x=max(max(c.x,c.y),c.z);
  var o=c;
  if n<0 {if l-n<=0 {o=vec3i(l);} else {for(var i=0u;i<3u;i++){o[i]=l+div_q((c[i]-l)*l,l-n);}}}
  if x>Q {if x-l<=0 {o=vec3i(l);} else {for(var i=0u;i<3u;i++){o[i]=l+div_q((o[i]-l)*(Q-l),x-l);}}}
  return clamp(o,vec3i(0),vec3i(Q));
}
fn set_lum_q(c:vec3i,l:i32)->vec3i {return clip_q(c+vec3i(l-lum_q(c)));}
fn set_sat_q(c:vec3i,s:i32)->vec3i {
  let n=min(min(c.x,c.y),c.z);let x=max(max(c.x,c.y),c.z);
  if x==n {return vec3i(0);}
  var o=vec3i(0);for(var i=0u;i<3u;i++){o[i]=div_q((c[i]-n)*s,x-n);}return o;
}
fn floor_isqrt(n:u32)->u32 {
  var lo=0u;var hi=16384u;
  loop {if lo>=hi {break;}let mid=(lo+hi+1u)/2u;if mid*mid<=n {lo=mid;} else {hi=mid-1u;}}
  return lo;
}
fn overlay_q(s:i32,d:i32)->i32 {
  if d<8192 {return div_q(2*s*d,Q);}return Q-div_q(2*(Q-s)*(Q-d),Q);
}
fn channel_q(d:i32,s:i32,mode:u32)->i32 {
  var b=0;
  switch mode {
    case 0u:{b=s;}
    case 1u:{b=mul_q(d,s);}
    case 2u:{b=Q-mul_q(Q-d,Q-s);}
    case 3u:{b=overlay_q(s,d);}
    case 4u:{b=min(d,s);}
    case 5u:{b=max(d,s);}
    case 6u:{b=Q;if s<Q {b=min(Q,div_q(d*Q,Q-s));}}
    case 7u:{if s!=0 {b=Q-min(Q,div_q((Q-d)*Q,s));}}
    case 8u:{b=overlay_q(d,s);}
    case 9u:{
      if s<=8192 {b=d-wide_div_q28(triple(Q-2*s,d,Q-d));} else {
        var d2=0;
        if d<=4096 {
          let cubic=triple(16*d,d,d);
          let square=triple(12*d,d,Q);
          let linear=triple(4*d,Q,Q);
          d2=wide_div_q28(wide_add(wide_sub(cubic,square),linear));
        } else {
          let n=u32(d*Q);let r=floor_isqrt(n);d2=i32(r+select(0u,1u,n-r*r>=r+1u));
        }
        b=d+wide_div_q14(wide_mul_i32(2*s-Q,d2-d));
      }
    }
    case 10u:{b=abs(d-s);}
    case 11u:{b=d+s-div_q(2*d*s,Q);}
    case 16u:{b=min(Q,d+s);}
    default:{}
  }
  return clamp(b,0,Q);
}
fn mix_q(d:vec3i,s:vec3i,mode:u32)->vec3i {
  switch mode {
    case 12u:{return set_lum_q(set_sat_q(s,sat_q(d)),lum_q(d));}
    case 13u:{return set_lum_q(set_sat_q(d,sat_q(s)),lum_q(d));}
    case 14u:{return set_lum_q(s,lum_q(d));}
    case 15u:{return set_lum_q(d,lum_q(s));}
    default:{return vec3i(channel_q(d.r,s.r,mode),channel_q(d.g,s.g,mode),channel_q(d.b,s.b,mode));}
  }
}
fn straight_q(p:vec4i)->vec3i {
  if p.a==0 {return vec3i(0);}
  return clamp(vec3i(div_q(p.r*Q,p.a),div_q(p.g*Q,p.a),div_q(p.b*Q,p.a)),vec3i(0),vec3i(Q));
}
fn blend_q(dst:vec4i,s:vec3i,pre:vec3i,sa:i32,mode:u32)->vec4i {
  let da=dst.a;var out=vec4i(0);out.a=div_q(sa*Q+da*(Q-sa),Q);
  if mode==0u {for(var i=0u;i<3u;i++){out[i]=div_q(pre[i]*Q+dst[i]*(Q-sa),Q);}return out;}
  let b=mix_q(straight_q(dst),s,mode);
  for(var i=0u;i<3u;i++){
    let num=wide_add(wide_add(triple(Q-sa,dst[i],Q),triple(Q-da,s[i],sa)),triple(da,sa,b[i]));
    out[i]=wide_div_q28(num);
  }
  // dp<=da and B, s in [0,Q] imply num <= out.a*Q^2 before rounding.
  // Half-away division is monotone, so RGB <= alpha without a corrective clamp.
  return out;
}
@compute @workgroup_size(16,16)
fn main(@builtin(global_invocation_id) id:vec3u) {
  if any(id.xy>=vec2u(256u)){return;}
  let p=config.origin_scale.xy+(vec2f(id.xy)+0.5)*config.origin_scale.z;
  var stack:array<vec4i,32>;var depth=0u;var accum=vec4i(0);
  if p.x>=0.0 && p.y>=0.0 && p.x<config.origin_scale.w && p.y<f32(config.info.z) {
    for(var i=0u;i<config.info.x;i++) {
      let c=commands[i];let op=i32(c.opacity_q);let mode=c.mode;
      if c.info.x==1u {stack[depth]=accum;depth++;accum=vec4i(0);}
      else if c.info.x==2u {
        let s=straight_q(accum);let sa=mul_q(accum.a,op);
        let pre=vec3i(mul_q(s.r,sa),mul_q(s.g,sa),mul_q(s.b,sa));
        depth--;accum=blend_q(stack[depth],s,pre,sa,mode);
      } else {
        let bytes=fetch_bytes(c,p,vec2i(id.xy));
        let s=vec3i(i32(byte_q14(bytes.r)),i32(byte_q14(bytes.g)),i32(byte_q14(bytes.b)));
        let sa=mul_q(i32(byte_q14(bytes.a)),op);
        let pre=vec3i(mul_q(i32(premul_byte_q14(bytes.r,bytes.a)),op),mul_q(i32(premul_byte_q14(bytes.g,bytes.a)),op),mul_q(i32(premul_byte_q14(bytes.b,bytes.a)),op));
        accum=blend_q(accum,s,pre,sa,mode);
      }
    }
  }
  let b=vec4u(clamp(vec4i(div_q(accum.r*255,Q),div_q(accum.g*255,Q),div_q(accum.b*255,Q),div_q(accum.a*255,Q)),vec4i(0),vec4i(255)));
  output[id.y*256u+id.x]=b.r|(b.g<<8u)|(b.b<<16u)|(b.a<<24u);
}
