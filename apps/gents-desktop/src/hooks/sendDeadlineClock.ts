import { nextSendDeadline } from "./chatDeliveryView";
import type { ChatStore } from "./chatStore";

export function keepSendDeadlineClock(chat: ChatStore, now: () => number = Date.now) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let due: number | null = null;
  function schedule() {
    const next = nextSendDeadline(chat.getState().chatDrafts, now());
    if (next === due) return;
    clearTimeout(timer);
    due = next;
    timer = next === null ? undefined : setTimeout(tick, Math.max(0, next - now()));
  }
  function tick() {
    due = null;
    timer = undefined;
    chat.setState({ sendDeadlineTick: now() });
    schedule();
  }
  const stop = chat.subscribe((state, previous) => {
    if (state.chatDrafts !== previous.chatDrafts) schedule();
  });
  schedule();
  return () => {
    stop();
    clearTimeout(timer);
  };
}

export function useSendDeadlineClock(chat: ChatStore): number {
  const tick = chat.use.sendDeadlineTick();
  return Math.max(tick, Date.now());
}
