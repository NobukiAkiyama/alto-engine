/** @internal The fields a transact request carries. */
export function intentRecord(intent) {
    return typeof intent === "string" ? { intent } : { intent: intent.id, label: intent.label };
}
