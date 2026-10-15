/* The sandbox document's entry: wait for the shell's connect message with
   the port, then hand the port to the runtime. The shell is the only
   sender this document trusts: the message must come from the parent
   window, and the port it carries is the channel from then on. */
import "../../App.css";

import { createSandboxRuntime } from "./runtime";

const root = document.getElementById("root");
if (!root) throw new Error("the sandbox document has no root");

let connected = false;
window.addEventListener("message", (event: MessageEvent<{ t?: string }>) => {
  if (connected || event.source !== window.parent) return;
  const port = event.ports[0];
  if (event.data?.t !== "connect" || !port) return;
  connected = true;
  const runtime = createSandboxRuntime(
    {
      post: (message) => port.postMessage(message),
      onMessage: (handler) => {
        port.onmessage = (e) => handler(e.data);
      },
    },
    { mount: root, document },
  );
  port.start();
  window.addEventListener("unload", () => runtime.dispose());
});
