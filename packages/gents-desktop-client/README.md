# @source-inc/gents-desktop-client

Transport interface, default Tauri transport, shared store/refresh coordinator, and generated view-model bindings for Gents desktop packages.

```ts
import {
  createDesktopClient,
  createDesktopStore,
  tauriTransport,
} from "@source-inc/gents-desktop-client";
import { createMemoryTransport } from "@source-inc/gents-desktop-client/testing";

const client = createDesktopClient(); // tauri by default
const store = createDesktopStore(client);
await store.start();
```

Tests inject a memory transport:

```ts
const transport = createMemoryTransport({
  handlers: {
    desktop_client_snapshot: () => ({ bootstrap: {}, client: null }),
  },
});
const client = createDesktopClient(transport);
```

Constructor injection applies to `DesktopClient` and `DesktopStore`. The
compatibility API adapter exports remain for the existing Gents Desktop harness
and prop-less domain component actions; those actions are not yet bound to a
specific `DesktopClient` instance. Direct Tauri API access is still centralized
in `transport.ts`.

The shared store coalesces aggregate snapshot refreshes; it does not yet contain
Gents Desktop's active-session polling, restart/backoff, or P2P auto-restart
coordinator. A downstream that needs Gents-equivalent streaming recovery must
extract or implement that coordinator explicitly.
