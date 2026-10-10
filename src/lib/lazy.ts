// Lazy loaders for the heavy optional renderers.
//
// mermaid and katex together are several times the size of the entire rest of
// the app. Neither is loaded until a message actually contains a diagram or a
// formula, and each is loaded exactly once.

let mermaidPromise: Promise<typeof import("mermaid").default> | null = null;
type Katex = typeof import("katex")["default"];
let katexPromise: Promise<Katex> | null = null;

export function loadMermaid(dark: boolean) {
  if (!mermaidPromise) {
    mermaidPromise = import("mermaid").then((m) => {
      m.default.initialize({
        startOnLoad: false,
        securityLevel: "strict",
        theme: dark ? "dark" : "neutral",
        fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-ui") || "sans-serif",
        flowchart: { curve: "basis", htmlLabels: false },
      });
      return m.default;
    });
  }
  return mermaidPromise;
}

/** Re-themes an already-loaded mermaid when the user flips light/dark. */
export async function retheme(dark: boolean) {
  if (!mermaidPromise) return;
  const m = await mermaidPromise;
  m.initialize({ startOnLoad: false, securityLevel: "strict", theme: dark ? "dark" : "neutral" });
}

export function loadKatex(): Promise<Katex> {
  if (!katexPromise) {
    katexPromise = (async () => {
      const [mod] = await Promise.all([import("katex"), import("katex/dist/katex.min.css")]);
      return mod.default;
    })();
  }
  return katexPromise;
}
