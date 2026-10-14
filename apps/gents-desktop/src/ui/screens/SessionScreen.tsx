/* One session: start a new one, or read and continue an existing one.
   Built from the kit's conversation patterns over the bridge's session
   projection: the timeline items are its own RenderedTimelineItem,
   rendered as they arrive. */
import { useApp } from "@/app/AppContext";
import { NewSession } from "./session/NewSession";
import { OpenSession } from "./session/OpenSession";

export function SessionScreen() {
  const selectedSessionId = useApp().stores.selection.use.sessionId();
  return selectedSessionId ? <OpenSession /> : <NewSession />;
}
