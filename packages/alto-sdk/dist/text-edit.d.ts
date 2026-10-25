import type { StylePatch, TextContent } from "./types.js";
/** Replaces a UTF-16 range; spans shift (typing at a span's end extends it), edits inside tate-chu-yoko dissolve it. */
export declare function replaceText(content: TextContent, start: number, end: number, insert: string): TextContent;
/** Applies a style patch to a UTF-16 range, splitting and merging spans. */
export declare function applyTextStyle(content: TextContent, start: number, end: number, patch: StylePatch): TextContent;
