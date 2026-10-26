/**
 * What an edit was for. Undo history and version commits keep this identifier, so a
 * host shows its own words for it. The engine and SDK use these; a host's own edits
 * use an identifier starting with `app:`.
 */
export type BuiltInIntent =
  | "layer.add"
  | "layer.remove"
  | "layer.move"
  | "layer.props"
  | "layer.transform"
  | "layer.fill"
  | "layer.gradient"
  | "layer.stamp"
  | "layer.smudge"
  | "layer.rasterize"
  | "layer.merge"
  | "brush.stroke"
  | "selection.set"
  | "mask.set"
  | "effects.set"
  | "text.edit"
  | "text.input"
  | "text.style"
  | "text.tate-chu-yoko"
  | "vector.edit"
  | "vector.mask"
  | "vector.clip"
  | "image.import"
  | "image.crop"
  | "image.rotate"
  | "image.flip"
  | "image.resize"
  | "version.restore"
  | "render.upgrade"
  | "recovered.edit";

export type IntentId = BuiltInIntent | `app:${string}`;

/** An identifier, or an identifier with a label the host wants kept beside it. */
export type Intent = IntentId | { id: IntentId; label?: string };

/** @internal The fields a transact request carries. */
export function intentRecord(intent: Intent): { intent: string; label?: string } {
  return typeof intent === "string" ? { intent } : { intent: intent.id, label: intent.label };
}
