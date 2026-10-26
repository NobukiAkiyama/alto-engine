// Appended to vector.wgsl. Each pipeline enables writes to exactly one target.
// The other attachments keep their existing coverage, including self-overlaps.
struct DenseCoverage {
 @location(0) a:f32,
 @location(1) b:f32,
 @location(2) c:f32,
 @location(3) d:f32,
 @location(4) e:f32,
 @location(5) f:f32,
 @location(6) g:f32,
 @location(7) h:f32,
};
@fragment fn dense_coverage()->DenseCoverage {
 return DenseCoverage(1.0,1.0,1.0,1.0,1.0,1.0,1.0,1.0);
}
