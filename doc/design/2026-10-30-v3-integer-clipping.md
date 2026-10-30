# v3 integer clipping

Render version 3 clipping uses Q14 premultiplied targets in the M5 renderer. The
v1/v2 clipping stages remain unchanged. The v3 shader reads both inputs through
integer ROI maps; no clip-base texture or floating-point pixel arithmetic is
needed.

## Clipped layers (mode 27)

`A` is the current clipping base and `B` is the clipped layer. The host passes
the layer opacity as Q14, except when a mask or clip path has already consumed
that opacity in mode 21. For each pixel, `sa = mul_q(B.a, opacity)`; the base and
source are straightened with the existing `straight_q` rule. The selected blend
mode produces `mixed = mix_q(base_color, source_color, blend_mode)`. Each output
color is `mul_q(div_q(base_color * (Q-sa) + mixed * sa, Q), A.a)`, and output
alpha is exactly `A.a`. Empty source or base leaves the base unchanged. This
retains translucent base coverage across every layer in a clipping chain.

The clipped result is then composited over the stored backdrop using the base
node's own opacity and blend mode. This is the existing group ordering.

## Clipped adjustments (mode 28)

`A` is the clipping base and `B` is the adjusted copy after its effect, mask,
and opacity mix. The output color is `mul_q(straight_q(B), A.a)` and its alpha is
`A.a`. This prevents blur and other adjustments from extending beyond the base
coverage while preserving the adjusted color within it.

The Metal tests cover clipped layer opacity, a chain over a translucent base,
and a clipped adjustment. The existing v2 tests still exercise the floating
point stages. Vector strokes remain a separate v3 gap, so new documents remain
v2 until that gap is closed.
