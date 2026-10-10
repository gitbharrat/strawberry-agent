// The chat column: transcript, run bar, composer. The tab strip above the
// workspace carries the title and every pane action.

import { createEffect } from "solid-js";
import { Transcript } from "./Transcript";
import { Composer } from "./Composer";
import { RunBar } from "./RunBar";
import { getSession } from "~/lib/session";

export function ChatView(props: { chatId: string }) {
  const session = () => getSession(props.chatId);

  // Loads once per chat, then stays cached for the life of the session.
  createEffect(() => {
    const s = session();
    if (!s.loaded()) void s.load();
  });

  return (
    <div class="chat">
      <Transcript session={session()} />
      <RunBar session={session()} />
      <Composer session={session()} chatId={props.chatId} />
    </div>
  );
}
