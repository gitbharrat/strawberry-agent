// First run: get a key in, then get out of the way.

import { createSignal, Show } from "solid-js";
import { Logo } from "./Logo";
import { Check, Key } from "./Icons";
import { api } from "~/lib/ipc";
import { store, toast } from "~/lib/store";
import { newChat } from "~/lib/actions";
import { errText } from "~/lib/err";

export function Welcome() {
  const [key, setKey] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  async function save() {
    const k = key().trim();
    if (!k) return;
    setBusy(true);
    try {
      const status = await api.setApiKey(k);
      store.setKeyStatus(status);
      void store.refreshModels(true);
      if (store.chats().length === 0) await newChat(null);
      toast("ok", "Ready");
    } catch (e) {
      toast("error", errText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="welcome">
      <Logo size={64} />
      <h1>Strawberry Agent</h1>
      <p class="welcome-sub">
        A coding harness for OpenRouter models. Paste a key to begin — it is stored in your system keychain
        and sent only to openrouter.ai.
      </p>

      <div class="welcome-form">
        <div class="welcome-input">
          <Key size={15} />
          <input
            autofocus
            type="password"
            placeholder="sk-or-v1-…"
            value={key()}
            onInput={(e) => setKey(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void save();
            }}
            spellcheck={false}
          />
        </div>
        <button
          type="button"
          class="btn"
          data-variant="primary"
          disabled={!key().trim() || busy()}
          onClick={() => void save()}
        >
          <Check size={14} /> {busy() ? "Checking…" : "Continue"}
        </button>
      </div>

      <p class="welcome-link">
        <a
          href="https://openrouter.ai/keys"
          onClick={(e) => {
            e.preventDefault();
            void import("@tauri-apps/plugin-opener").then((m) => m.openUrl("https://openrouter.ai/keys"));
          }}
        >
          Get a key at openrouter.ai/keys
        </a>
      </p>

      <Show when={store.appInfo()}>
        {(i) => (
          <p class="welcome-foot muted">
            v{i().version} · {i().platform}
          </p>
        )}
      </Show>
    </div>
  );
}
