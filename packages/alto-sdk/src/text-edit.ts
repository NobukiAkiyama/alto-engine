import type { StylePatch, TextContent, TextSpan } from "./types.js";

/** Replaces a UTF-16 range; spans shift (typing at a span's end extends it), edits inside tate-chu-yoko dissolve it. */
export function replaceText(content: TextContent, start: number, end: number, insert: string): TextContent {
  const c = structuredClone(content);
  if (start > end) [start, end] = [end, start];
  const added = insert.length, removed = end - start;
  // Text inserted at a boundary joins the span ending there, so typing continues the preceding style.
  const shift = (p: number) => (p < start ? p : p >= end ? p - removed + added : start + added);
  c.text = c.text.slice(0, start) + insert + c.text.slice(end);
  // Typing over a selection keeps the style of the replaced text.
  c.spans = (c.spans || []).map((s) => ({ ...s, start: s.start === start && removed > 0 ? s.start : shift(s.start), end: s.end === start ? s.end + added : shift(s.end) })).filter((s) => s.start < s.end);
  c.tcy = (c.tcy || [])
    .filter((r) => !(start < r.end && end > r.start) && !(start === end && start > r.start && start < r.end))
    .map((r) => ({ start: shift(r.start), end: shift(r.end) }));
  return c;
}
/** Applies a style patch to a UTF-16 range, splitting and merging spans. */
export function applyTextStyle(content: TextContent, start: number, end: number, patch: StylePatch): TextContent {
  const c = structuredClone(content);
  if (start > end) [start, end] = [end, start];
  const spans = c.spans || [];
  const cuts = [...new Set([0, c.text.length, start, end, ...spans.flatMap((s) => [s.start, s.end])])].filter((x) => x >= 0 && x <= c.text.length).sort((a, b) => a - b);
  const out: TextSpan[] = [];
  for (let i = 0; i + 1 < cuts.length; i++) {
    const [a, b] = [cuts[i], cuts[i + 1]];
    let style: StylePatch = { ...(spans.find((s) => s.start <= a && b <= s.end)?.style || {}) };
    if (a >= start && b <= end) style = { ...style, ...patch };
    if (!Object.keys(style).length) continue;
    const last = out.at(-1);
    if (last && last.end === a && JSON.stringify(last.style) === JSON.stringify(style)) last.end = b;
    else out.push({ start: a, end: b, style });
  }
  c.spans = out;
  return c;
}
