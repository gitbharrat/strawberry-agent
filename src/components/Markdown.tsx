// Renders model output.
//
// Two performance layers keep long streams smooth:
//
//  1. `splitStable` finds the last point in a partially received document that
//     can no longer change, so the parser mostly re-parses only the tail.
//  2. The stable prefix and the live tail render into SEPARATE hosts. The
//     stable host's DOM is rewritten only when a new block settles; per-token
//     updates touch just the tail's few nodes. Before this split, every delta
//     re-created the entire message's DOM — code highlights, tables and all —
//     sixty times a second, which is exactly the lag you feel on long answers.

import { createEffect, createMemo, onCleanup } from "solid-js";
import { needsMath, needsMermaid, renderMarkdown, splitStable } from "~/lib/markdown";
import { enhanceDesignBlocks, type DesignMutator } from "~/lib/designDom";
import { loadKatex, loadMermaid, retheme } from "~/lib/lazy";
import { store, toast } from "~/lib/store";
import { openPath } from "~/lib/actions";

interface Props {
  text: string;
  streaming?: boolean;
  class?: string;
  /** Present → design blocks in this content become editable in place. */
  onDesignEdit?: (mutate: DesignMutator) => void;
  /** Present → design blocks offer "sketch" (opens the scratchpad seeded). */
  onDesignSketch?: (designJson: string) => void;
}

let diagramSeq = 0;

export function Markdown(props: Props) {
  let stableEl!: HTMLDivElement;
  let tailEl!: HTMLDivElement;

  // While streaming, the settled prefix only changes when a new block closes,
  // so this memo recomputes rarely even though `text` changes constantly.
  const parts = createMemo(() =>
    props.streaming ? splitStable(props.text) : { stable: props.text, tail: "" },
  );
  const stableHtml = createMemo(() => renderMarkdown(parts().stable));
  const tailHtml = createMemo(() => (parts().tail ? renderMarkdown(parts().tail) : ""));

  const enhance = (host: HTMLElement) => {
    // Editing mid-stream would race the writer; view-only until settled.
    if (props.streaming) return;
    enhanceDesignBlocks(host, { onEdit: props.onDesignEdit, onSketch: props.onDesignSketch });
  };

  createEffect(() => {
    stableEl.innerHTML = stableHtml();
    upgrade(stableEl, parts().stable);
    enhance(stableEl);
  });
  createEffect(() => {
    tailEl.innerHTML = tailHtml();
    upgrade(tailEl, parts().tail);
    enhance(tailEl);
  });

  const onClick = (e: MouseEvent) => {
    const target = e.target as HTMLElement;

    const copy = target.closest<HTMLButtonElement>("[data-copy]");
    if (copy) {
      const code = copy.closest("figure")?.querySelector("code")?.textContent ?? "";
      void navigator.clipboard.writeText(code).then(() => {
        copy.textContent = "Copied";
        setTimeout(() => (copy.textContent = "Copy"), 1200);
      });
      return;
    }

    const path = target.closest<HTMLElement>(".md-path");
    if (path?.dataset.path) {
      e.preventDefault();
      void openPath(path.dataset.path, path.dataset.line ? Number(path.dataset.line) : undefined);
      return;
    }

    const link = target.closest<HTMLAnchorElement>("a[data-external]");
    if (link) {
      // Never navigate the app frame away; hand external URLs to the OS.
      e.preventDefault();
      void import("@tauri-apps/plugin-opener")
        .then((m) => m.openUrl(link.href))
        .catch(() => toast("error", "Could not open that link"));
    }
  };

  onCleanup(() => {
    stableEl.innerHTML = "";
    tailEl.innerHTML = "";
  });

  return (
    // One delegated listener for the whole rendered tree: the interactive
    // children are real buttons and links produced by the renderer, and each
    // is independently keyboard reachable.
    // biome-ignore lint/a11y/useKeyWithClickEvents: delegation target, children carry their own semantics
    // biome-ignore lint/a11y/noStaticElementInteractions: as above
    <div class={`md ${props.class ?? ""}`} onClick={onClick}>
      <div class="md-part" ref={stableEl} />
      <div class="md-part" ref={tailEl} />
    </div>
  );
}

/** Replaces mermaid and maths placeholders with rendered output. */
function upgrade(host: HTMLElement, source: string) {
  if (needsMermaid(source)) void upgradeMermaid(host);
  if (needsMath(source)) void upgradeMath(host);
}

async function upgradeMermaid(host: HTMLElement) {
  const nodes = host.querySelectorAll<HTMLElement>(".md-mermaid:not([data-done])");
  if (!nodes.length) return;
  const dark = document.documentElement.dataset.theme === "dark";
  const mermaid = await loadMermaid(dark);

  for (const node of nodes) {
    const src = node.dataset.src ?? "";
    node.dataset.done = "1";
    try {
      const { svg } = await mermaid.render(`sa-diagram-${++diagramSeq}`, src);
      node.innerHTML = svg;
      node.classList.add("md-mermaid-ok");
    } catch (err) {
      // A diagram that will not parse is still worth showing as source.
      node.classList.add("md-mermaid-error");
      const message = err instanceof Error ? err.message : String(err);
      node.innerHTML =
        `<div class="md-mermaid-msg">Diagram could not be rendered — ${escapeText(message)}</div>` +
        `<pre class="md-mermaid-fallback">${escapeText(src)}</pre>`;
    }
  }
}

async function upgradeMath(host: HTMLElement) {
  const nodes = host.querySelectorAll<HTMLElement>(".md-math:not([data-done])");
  if (!nodes.length) return;
  const katex = await loadKatex();

  for (const node of nodes) {
    node.dataset.done = "1";
    const tex = node.dataset.tex ?? "";
    try {
      katex.render(tex, node, {
        displayMode: node.dataset.display === "1",
        throwOnError: false,
        output: "html",
        strict: false,
      });
    } catch {
      node.textContent = tex;
      node.classList.add("md-math-error");
    }
  }
}

function escapeText(s: string): string {
  const d = document.createElement("div");
  d.textContent = s;
  return d.innerHTML;
}

/** Keeps rendered diagrams in step with a theme change. */
export function rethemeDiagrams() {
  document.querySelectorAll<HTMLElement>(".md-mermaid[data-done]").forEach((n) => {
    delete n.dataset.done;
    n.innerHTML = `<pre class="md-mermaid-fallback">${escapeText(n.dataset.src ?? "")}</pre>`;
  });
  void retheme(document.documentElement.dataset.theme === "dark");
  document.querySelectorAll<HTMLElement>(".md").forEach((el) => {
    const src = el.textContent ?? "";
    if (needsMermaid(src)) void upgradeMermaid(el);
  });
  void store;
}
