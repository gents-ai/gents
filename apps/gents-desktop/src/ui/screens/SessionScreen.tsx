import { useApp } from "@/app/AppContext";
import { NewSession } from "./session/NewSession";
import { OpenSession } from "./session/OpenSession";

export function SessionScreen() {
  const selectedSessionId = useApp().stores.selection.use.sessionId();
  return selectedSessionId ? <OpenSession /> : <NewSession />;
}
