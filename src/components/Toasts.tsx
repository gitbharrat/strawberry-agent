// Transient notices.
//
// Bottom-right, stacked, never blocking. Errors linger; confirmations do not.

import { For, Show } from "solid-js";
import { Check, Close, Info, Warning } from "./Icons";
import { dismissToast, store } from "~/lib/store";

export function Toasts() {
  return (
    <div class="toasts">
      <For each={store.toasts()}>
        {(t) => (
          <div class="toast" data-level={t.level}>
            <span class="toast-icon">
              <Show
                when={t.level === "error" || t.level === "warn"}
                fallback={t.level === "ok" ? <Check size={13} /> : <Info size={13} />}
              >
                <Warning size={13} />
              </Show>
            </span>
            <span class="toast-text selectable">{t.text}</span>
            <Show when={t.action}>
              {(a) => (
                <button
                  type="button"
                  class="btn"
                  data-size="sm"
                  onClick={() => {
                    a().run();
                    dismissToast(t.id);
                  }}
                >
                  {a().label}
                </button>
              )}
            </Show>
            <button
              type="button"
              class="btn"
              data-size="icon-sm"
              onClick={() => dismissToast(t.id)}
              title="Dismiss"
            >
              <Close size={11} />
            </button>
          </div>
        )}
      </For>
    </div>
  );
}
