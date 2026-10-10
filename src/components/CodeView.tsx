// Code viewer and editor.
//
// A transparent textarea sits exactly on top of a highlighted `<pre>`. That is
// the whole trick: real native text editing — caret, selection, IME, undo,
// spell-check off — with syntax colour behind it, for a few hundred lines of
// code instead of a 300 KB editor framework.
//
// Repainting the highlight layer is debounced to the next frame, so typing
// stays at input latency even in a large file.

import { createEffect, createSignal, on, onCleanup, onMount, Show } from "solid-js";
import { Check, Close, Code, Refresh, Warning } from "./Icons";
import { Markdown } from "./Markdown";
import { highlight, languageLabel } from "~/lib/highlight";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { formatBytes } from "~/lib/actions";
import type { FileContents } from "~/lib/types";
import { errText } from "~/lib/err";

export function CodeView(props: { path: string; line?: number; tabId: string }) {
  const [file, setFile] = createSignal<FileContents | null>(null);
  const [text, setText] = createSignal("");
  const [dirty, setDirty] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // Markdown opens rendered; one click flips to the raw source and back.
  const isMd = /\.(md|markdown)$/i.test(props.path);
  const [previewMd, setPreviewMd] = createSignal(isMd);

  let area!: HTMLTextAreaElement;
  let pre!: HTMLPreElement;
  let gutter!: HTMLDivElement;
  let raf = 0;

  async function load() {
    setError(null);
    try {
      const f = await api.fsRead(props.path);
      setFile(f);
      setText(f.content);
      setDirty(false);
      queueMicrotask(() => {
        paint();
        if (props.line) scrollToLine(props.line);
      });
    } catch (e) {
      setError(errText(e));
    }
  }

  onMount(load);
  createEffect(on(() => props.path, load, { defer: true }));
  onCleanup(() => cancelAnimationFrame(raf));

  /** Repaints the highlight layer at most once per frame. */
  function paint() {
    cancelAnimationFrame(raf);
    raf = requestAnimationFrame(() => {
      const lang = file()?.language ?? "text";
      // The trailing newline keeps the last line's box height stable while the
      // caret sits on it.
      pre.innerHTML = `${highlight(text(), lang)}\n`;
      const count = text().split("\n").length;
      gutter.textContent = Array.from({ length: count }, (_, i) => i + 1).join("\n");
    });
  }

  function syncScroll() {
    pre.scrollTop = area.scrollTop;
    pre.scrollLeft = area.scrollLeft;
    gutter.scrollTop = area.scrollTop;
  }

  function scrollToLine(n: number) {
    const lineHeight = parseFloat(getComputedStyle(area).lineHeight) || 20;
    area.scrollTop = Math.max(0, (n - 4) * lineHeight);
    syncScroll();
  }

  async function save() {
    const f = file();
    if (!f || saving()) return;
    setSaving(true);
    try {
      const updated = await api.fsWrite({
        path: f.path,
        content: text(),
        expectedHash: f.hash,
        chatId: store.activeChatId() ?? undefined,
      });
      setFile(updated);
      setDirty(false);
      toast("ok", `Saved ${updated.name}`);
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setSaving(false);
    }
  }

  const onKeyDown = (e: KeyboardEvent) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      void save();
      return;
    }
    // Tab inserts an indent rather than leaving the field, which is what
    // anyone typing code expects.
    if (e.key === "Tab") {
      e.preventDefault();
      const start = area.selectionStart;
      const end = area.selectionEnd;
      const next = `${text().slice(0, start)}  ${text().slice(end)}`;
      setText(next);
      paint();
      queueMicrotask(() => {
        area.selectionStart = area.selectionEnd = start + 2;
      });
    }
  };

  return (
    <div class="code">
      <div class="code-head">
        <span class="truncate" title={props.path}>
          {file()?.name ?? props.path.split("/").pop()}
        </span>
        <Show when={dirty()}>
          <span class="code-dot" title="Unsaved changes" />
        </Show>
        <div class="spacer" />
        <Show when={file()}>
          {(f) => (
            <>
              <span class="chip">{languageLabel(f().language)}</span>
              <span class="muted tabular">
                {f().lines} lines · {formatBytes(f().size)}
              </span>
            </>
          )}
        </Show>
        <Show when={isMd}>
          <button
            type="button"
            class="btn"
            data-size="sm"
            data-on={!previewMd()}
            title={previewMd() ? "View the raw markdown" : "View rendered"}
            onClick={() => setPreviewMd(!previewMd())}
          >
            <Code size={12} /> {previewMd() ? "Code" : "Preview"}
          </button>
        </Show>
        <button
          type="button"
          class="btn"
          data-size="icon-sm"
          title="Reload from disk"
          onClick={() => void load()}
        >
          <Refresh size={12} />
        </button>
        <Show when={!file()?.readOnly}>
          <button
            type="button"
            class="btn"
            data-size="sm"
            data-variant="primary"
            disabled={!dirty() || saving()}
            onClick={() => void save()}
          >
            <Check size={12} /> Save
          </button>
        </Show>
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

      <Show when={error()}>
        {(e) => (
          <div class="code-error">
            <Warning size={14} /> <span class="selectable">{e()}</span>
          </div>
        )}
      </Show>

      <Show when={file()?.binary}>
        <div class="empty">
          <p>This is a binary file.</p>
        </div>
      </Show>

      <Show when={file() && !file()!.binary && isMd && previewMd()}>
        <div class="code-mdwrap">
          <Markdown text={text()} class="selectable" />
        </div>
      </Show>

      <Show when={file() && !file()!.binary && !(isMd && previewMd())}>
        <div class="code-body">
          <div class="code-gutter" ref={gutter} aria-hidden="true" />
          <div class="code-scroll">
            <pre class="code-layer" ref={pre} aria-hidden="true" />
            <textarea
              ref={area}
              class="code-input"
              spellcheck={false}
              autocapitalize="off"
              autocomplete="off"
              readOnly={file()?.readOnly}
              value={text()}
              onInput={(e) => {
                setText(e.currentTarget.value);
                setDirty(true);
                paint();
              }}
              onScroll={syncScroll}
              onKeyDown={onKeyDown}
            />
          </div>
        </div>
      </Show>

      <Show when={file()?.truncated}>
        <div class="code-foot warn">Showing the first 4 MB — this file is too large to edit here.</div>
      </Show>
    </div>
  );
}
