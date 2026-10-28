// Integer tile compositor main. Appended after blend-q14.wgsl.
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
