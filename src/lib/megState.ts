// Shared state for the Model Execution Graph: the stored graphs and each
// chat's binding. Lives outside the pane so the composer's graph chip and the
// send path can read it without mounting the canvas.

import { createSignal } from "solid-js";
import { api } from "./ipc";
import type { GraphDoc } from "./meg";

const [graphs, setGraphs] = createSignal<GraphDoc[]>([]);
const [loaded, setLoaded] = createSignal(false);
const [bindings, setBindings] = createSignal<Record<string, string | null>>({});
const pending = new Set<string>();

export const megState = {
  graphs,
  loaded,

  async load(): Promise<void> {
    setGraphs(await api.graphList());
    setLoaded(true);
  },

  /** The chat's bound graph id. `undefined` means "still fetching" — the
   * fetch is kicked off on first read and the signal updates when it lands. */
  binding(chatId: string): string | null | undefined {
    const b = bindings();
    if (chatId in b) return b[chatId];
    if (!pending.has(chatId)) {
      pending.add(chatId);
      void api
        .graphBinding(chatId)
        .then((g) => setBindings((m) => ({ ...m, [chatId]: g })))
        .catch(() => setBindings((m) => ({ ...m, [chatId]: null })))
        .finally(() => pending.delete(chatId));
    }
    return undefined;
  },

  async bind(chatId: string, graphId: string | null): Promise<void> {
    await api.graphBind(chatId, graphId);
    setBindings((m) => ({ ...m, [chatId]: graphId }));
  },

  upsert(doc: GraphDoc): void {
    setGraphs((gs) => {
      const i = gs.findIndex((g) => g.id === doc.id);
      if (i === -1) return [doc, ...gs];
      const next = gs.slice();
      next[i] = doc;
      return next;
    });
  },

  remove(id: string): void {
    setGraphs((gs) => gs.filter((g) => g.id !== id));
    setBindings((m) => {
      const next = { ...m };
      for (const k of Object.keys(next)) if (next[k] === id) next[k] = null;
      return next;
    });
  },
};
