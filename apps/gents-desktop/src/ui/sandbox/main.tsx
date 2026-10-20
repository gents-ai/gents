/* The sandbox runtime's entry. The frame's inline bootstrap
   (bootstrap.js) has already taken the shell's connect message — checked
   that it came from the parent window — kept its port, and loaded this
   script from an in-frame blob. The port is the channel from here. */
import "../../App.css";

import { createSandboxRuntime } from "./runtime";

const root = document.getElementById("root");
if (!root) throw new Error("the sandbox document has no root");

const handoff = (window as { __gentsFrame?: { port?: MessagePort } }).__gentsFrame;
const port = handoff?.port;
if (!handoff || !port)
  throw new Error("the sandbox runtime started without the shell's port");
delete handoff.port;

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
