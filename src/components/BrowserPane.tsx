// In-app browser.
//
// A sandboxed iframe with a URL bar and history. Many sites refuse to be
// framed (`X-Frame-Options`, `frame-ancestors`) — that is the site's decision,
// not something the app can override, so a blocked page offers to open in the
// real browser instead of failing silently.
//
// A full embedded engine will replace this when the dedicated browser surface
// lands; the interface here is deliberately the one it will keep.

import { createSignal, Show } from "solid-js";
import { Chevron, Close, Globe, Refresh } from "./Icons";
import { store, toast } from "~/lib/store";

function normalise(input: string): string {
  const raw = input.trim();
  if (!raw) return "";
  if (/^https?:\/\//i.test(raw)) return raw;
  // A bare token with no dot is a search, not a hostname.
  if (!raw.includes(".") || raw.includes(" ")) {
    return `https://duckduckgo.com/?q=${encodeURIComponent(raw)}`;
  }
  return `https://${raw}`;
}

export function BrowserPane(props: { url: string; tabId: string }) {
  const [history, setHistory] = createSignal<string[]>([props.url]);
  const [cursor, setCursor] = createSignal(0);
  const [draft, setDraft] = createSignal(props.url);
  const [nonce, setNonce] = createSignal(0);

  const current = () => history()[cursor()] ?? props.url;

  function go(to: string) {
    const url = normalise(to);
    if (!url) return;
    setHistory((h) => [...h.slice(0, cursor() + 1), url]);
    setCursor((c) => c + 1);
    setDraft(url);
  }

  function step(delta: number) {
    const next = cursor() + delta;
    if (next < 0 || next >= history().length) return;
    setCursor(next);
    setDraft(history()[next] ?? "");
  }

  function openExternally() {
    void import("@tauri-apps/plugin-opener")
      .then((m) => m.openUrl(current()))
      .catch(() => toast("error", "Could not open that link"));
  }

  return (
    <div class="viewer">
      <div class="code-head browser-bar">
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Back"
          disabled={cursor() === 0}
          onClick={() => step(-1)}
        >
          <span style={{ transform: "rotate(180deg)", display: "flex" }}>
            <Chevron size={12} />
          </span>
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Forward"
          disabled={cursor() >= history().length - 1}
          onClick={() => step(1)}
        >
          <Chevron size={12} />
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Reload"
          onClick={() => setNonce((n) => n + 1)}
        >
          <Refresh size={12} />
        </button>
        <div class="browser-url">
          <Globe size={12} />
          <input
            value={draft()}
            spellcheck={false}
            placeholder="Search or enter a URL"
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") go(draft());
            }}
            onFocus={(e) => e.currentTarget.select()}
          />
        </div>
        <button
          type="button"
          class="btn"
          data-size="sm"
          title="Open in your default browser"
          onClick={openExternally}
        >
          Open externally
        </button>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Close"
          onClick={() => store.closeTab(props.tabId)}
        >
          <Close size={12} />
        </button>
      </div>

      <Show
        when={current()}
        fallback={
          <div class="empty">
            <p>Enter a URL to begin.</p>
          </div>
        }
      >
        <iframe
          class="viewer-frame"
          title="Browser"
          src={`${current()}${current().includes("#") ? "" : `#${nonce()}`}`}
          sandbox="allow-scripts allow-forms allow-popups allow-same-origin"
          referrerpolicy="no-referrer"
        />
      </Show>

      <div class="browser-note muted">
        Some sites refuse to be embedded. Use <em>Open externally</em> if the page stays blank.
      </div>
    </div>
  );
}
