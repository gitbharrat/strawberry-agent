// A question from the model, blocking its run.
//
// Options render as buttons with the recommended one first and visually
// primary, so the common case is one click. Free text is there for the answer
// the model did not anticipate.

import { createSignal, For, Show } from "solid-js";
import { Send, Sparkle } from "./Icons";

interface Props {
  question: string;
  options: string[];
  recommended: string | null;
  allowFreeText: boolean;
  onAnswer: (text: string) => void;
}

export function QuestionCard(props: Props) {
  const [draft, setDraft] = createSignal("");

  // Recommended first; everything else keeps its order.
  const ordered = () => {
    const rec = props.recommended;
    if (!rec) return props.options;
    return [rec, ...props.options.filter((o) => o !== rec)];
  };

  return (
    <div class="question">
      <div class="question-head">
        <Sparkle size={12} />
        <span>The agent is asking</span>
      </div>
      <div class="question-text selectable">{props.question}</div>

      <Show when={ordered().length > 0}>
        <div class="question-options">
          <For each={ordered()}>
            {(option) => (
              <button
                type="button"
                class="btn"
                data-variant={option === props.recommended ? "primary" : "outline"}
                data-size="sm"
                onClick={() => props.onAnswer(option)}
              >
                {option}
                <Show when={option === props.recommended}>
                  <span class="question-rec">recommended</span>
                </Show>
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={props.allowFreeText}>
        <div class="question-free">
          <input
            class="input"
            placeholder="Or answer in your own words…"
            value={draft()}
            onInput={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && draft().trim()) props.onAnswer(draft().trim());
            }}
          />
          <button
            type="button"
            class="btn"
            data-size="icon"
            disabled={!draft().trim()}
            title="Send answer"
            onClick={() => props.onAnswer(draft().trim())}
          >
            <Send size={13} />
          </button>
        </div>
      </Show>
    </div>
  );
}
