# v3 integer vector strokes

This extends the v3 fill design of 2026-10-28. Its earlier stroke fallback is
superseded here. v1/v2 continue to use Lyon and their existing MSAA path.

For v3, the renderer paints each path's fill before its stroke. It reads the
stored path, width, cap, join and miter limit as IEEE bits and quantizes them
with integer half-away rounding. The local coordinate precision is increased
by powers of two until a local unit projects to at most one screen Q8 unit at
the requested level. Curves use the fill rasterizer's integer subdivision
rule, without implicitly closing open stroke subpaths.

Each centerline segment becomes a local rectangle of half-width radius. Butt
caps add nothing; square caps extend the first and last rectangles by the
radius; round caps are half-disks approximated by integer recursive arc
subdivision. Joins add bevel triangles, miter triangles limited by the stored
miter ratio, or round wedges. All primitives are mapped once through the v3
fixed world matrix to screen Q8 and share one nonzero coverage raster. Their
winding is normalized so overlaps form a union. The existing Q14 vector paint
stage consumes the resulting R8 coverage bytes.

Curve subdivision and outline command budgets are fixed per path and level,
independent of tile window. An over-budget path is skipped on every tile.
Metal tests cover all cap and join styles, nonuniform transforms, fill then
stroke order, and full-image parity with the integer reference. Rust tests
cover high zoom and tile seam equality.
