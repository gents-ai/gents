import { nextSendDeadline } from "./chatDeliveryView";
import type { ChatStore } from "./chatStore";

/** The one clock of saved send deadlines: when the earliest deadline still
 * ahead passes, it records the time in `sendDeadlineTick`, so every view and
 * the landed-send sweep re-derive send status then and a pending message
 * shows failed. */
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

/** The time a send view derives its status at, advanced by the deadline clock. */
export function useSendDeadlineClock(chat: ChatStore): number {
  const tick = chat.use.sendDeadlineTick();
  // A draft saved after the last tick derives against the current time.
  return Math.max(tick, Date.now());
}
