// Error → readable text.
//
// Backend commands reject with a structured `{ kind, message }` object, not an
// Error instance, so `String(e)` renders the infamous "[object Object]" — in a
// status chip, that hides exactly the thing the user needed to read. Every
// catch in the app formats through here instead.

export function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object") {
    const m = (e as { message?: unknown }).message;
    if (typeof m === "string" && m.length > 0) return m;
    try {
      return JSON.stringify(e);
    } catch {
      /* circular — fall through */
    }
  }
  return String(e);
}
