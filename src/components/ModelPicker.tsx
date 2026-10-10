// Model selector.
//
// OpenRouter exposes several hundred models, so this is a filtered list rather
// than a dropdown: type to narrow, and capability badges make the trade-offs
// visible without leaving the composer.

import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { ChevronDown, Refresh, Search } from "./Icons";
import { store } from "~/lib/store";
import { setChatModel } from "~/lib/actions";
import type { ModelInfo } from "~/lib/types";

function pricePerMillion(raw: string | undefined): number | null {
  if (!raw) return null;
  const n = Number(raw);
  return Number.isFinite(n) ? n * 1_000_000 : null;
}

function priceLabel(m: ModelInfo): string {
  const inp = pricePerMillion(m.pricing?.prompt);
  const out = pricePerMillion(m.pricing?.completion);
  if (inp === null || out === null) return "";
  if (inp === 0 && out === 0) return "free";
  return `$${inp.toFixed(2)} / $${out.toFixed(2)}`;
}

function contextLabel(m: ModelInfo): string {
  const n = m.contextLength ?? m.topProvider?.contextLength;
  if (!n) return "";
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M` : `${Math.round(n / 1000)}k`;
}

/**
 * Two modes: bound to a chat (`chatId`, persists via `setChatModel`), or
 * controlled (`value` + `onChange`) for pickers that live outside a chat —
 * the analysis pane chooses its annotation model this way.
 */
export function ModelPicker(props: {
  chatId?: string;
  value?: string;
  onChange?: (id: string) => void;
  /** Menu direction: "up" (composer, the default) or "down" (headers). */
  drop?: "up" | "down";
}) {
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal("");
  const [toolsOnly, setToolsOnly] = createSignal(true);
  let root!: HTMLDivElement;
  let search!: HTMLInputElement;

  const chat = () => store.chats().find((c) => c.id === props.chatId);
  const current = () => props.value ?? chat()?.model ?? store.settings()?.defaultModel ?? "";
  const currentInfo = () => store.models().find((m) => m.id === current());

  const shortName = () => {
    const id = current();
    const info = currentInfo();
    if (info?.name) return info.name.replace(/\s*\(.*\)$/, "");
    return id.split("/").pop() ?? id;
  };

  const results = createMemo(() => {
    const q = query().trim().toLowerCase();
    const all = store.models();
    const filtered = all.filter((m) => {
      if (toolsOnly() && !m.supportedParameters.includes("tools")) return false;
      if (!q) return true;
      return m.id.toLowerCase().includes(q) || m.name.toLowerCase().includes(q);
    });
    // Keep the active model pinned so it is never filtered out of view.
    const active = all.find((m) => m.id === current());
    if (active && !filtered.includes(active)) filtered.unshift(active);
    return filtered.slice(0, 120);
  });

  const onDocClick = (e: MouseEvent) => {
    if (open() && !root.contains(e.target as Node)) setOpen(false);
  };
  onMount(() => document.addEventListener("mousedown", onDocClick));
  onCleanup(() => document.removeEventListener("mousedown", onDocClick));

  function choose(id: string) {
    if (props.onChange) props.onChange(id);
    else if (props.chatId) void setChatModel(props.chatId, id);
    setOpen(false);
    setQuery("");
  }

  return (
    <div class="model-picker" data-drop={props.drop ?? "up"} ref={root}>
      <button
        type="button"
        class="btn model-trigger"
        data-size="sm"
        data-on={open()}
        onClick={() => {
          // `open()` already reflects the new value after the setter, so the
          // focus check has to be made against the value we are moving to.
          const next = !open();
          setOpen(next);
          if (next) queueMicrotask(() => search?.focus());
        }}
        title={current()}
      >
        <span class="truncate">{shortName()}</span>
        <ChevronDown size={12} />
      </button>

      <Show when={open()}>
        <div class="model-menu">
          <div class="model-search">
            <Search size={13} />
            <input
              ref={search}
              class="model-search-input"
              placeholder="Filter models…"
              value={query()}
              onInput={(e) => setQuery(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape") setOpen(false);
                if (e.key === "Enter" && results()[0]) choose(results()[0]!.id);
              }}
            />
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              title="Refresh catalogue"
              onClick={() => void store.refreshModels(true)}
            >
              <Refresh size={12} />
            </button>
          </div>

          <div class="model-filters">
            <label class="model-toggle">
              <input
                type="checkbox"
                checked={toolsOnly()}
                onChange={(e) => setToolsOnly(e.currentTarget.checked)}
              />
              Tool-capable only
            </label>
            <span class="muted tabular">{results().length}</span>
          </div>

          <div class="model-list">
            <Show when={query().includes("/") && !results().some((m) => m.id === query().trim())}>
              <button type="button" class="model-row" onClick={() => choose(query().trim())}>
                <span class="model-name truncate">Use “{query().trim()}”</span>
                <span class="model-id truncate muted">not in the catalogue — send it anyway</span>
              </button>
            </Show>
            <For
              each={results()}
              fallback={
                <div class="model-empty muted">
                  <Show when={store.models().length === 0} fallback={<>No models match that filter.</>}>
                    <Show
                      when={store.modelsLoading()}
                      fallback={
                        <>
                          {store.modelsError() ?? "The model catalogue has not loaded."}
                          <br />
                          <button
                            type="button"
                            class="btn"
                            data-size="sm"
                            data-variant="outline"
                            onClick={() => void store.refreshModels(true)}
                          >
                            Retry
                          </button>
                        </>
                      }
                    >
                      Loading the catalogue…
                    </Show>
                  </Show>
                </div>
              }
            >
              {(m) => (
                <button
                  type="button"
                  class="model-row"
                  data-active={m.id === current()}
                  onClick={() => choose(m.id)}
                >
                  <span class="model-name truncate">{m.name || m.id}</span>
                  <span class="model-id truncate muted">{m.id}</span>
                  <span class="model-badges">
                    <Show when={contextLabel(m)}>{(c) => <span class="chip">{c()}</span>}</Show>
                    <Show when={m.supportedParameters.includes("tools")}>
                      <span class="chip" data-tone="accent" title="Supports tool calling">
                        tools
                      </span>
                    </Show>
                    <Show when={m.architecture?.inputModalities.includes("image")}>
                      <span class="chip" title="Accepts images">
                        vision
                      </span>
                    </Show>
                    <Show when={m.reasoning || m.supportedParameters.includes("reasoning")}>
                      <span
                        class="chip"
                        title={
                          m.reasoning?.supportedEfforts.length
                            ? `Reasoning efforts: ${m.reasoning.supportedEfforts.join(", ")}`
                            : "Supports extended reasoning"
                        }
                      >
                        think
                        <Show when={m.reasoning?.supportedEfforts.length}>
                          {(n) => <span class="model-effort-count">{n()}</span>}
                        </Show>
                      </span>
                    </Show>
                    <Show when={priceLabel(m)}>
                      {(p) => (
                        <span class="chip tabular" title="USD per million tokens, in / out">
                          {p()}
                        </span>
                      )}
                    </Show>
                  </span>
                </button>
              )}
            </For>
          </div>

          <Show when={store.modelsStale()}>
            <div class="model-foot muted">Showing a cached catalogue — refresh when you are back online.</div>
          </Show>
        </div>
      </Show>
    </div>
  );
}
