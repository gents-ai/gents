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

Direct Tauri API access is centralized in `transport.ts`.

The shared store coalesces aggregate snapshot refreshes; it does not yet contain
Gents Desktop's active-session polling, restart/backoff, or P2P auto-restart
coordinator. A downstream that needs Gents-equivalent streaming recovery must
extract or implement that coordinator explicitly.

`createDesktopClient(transport)` exposes both the lifecycle/snapshot methods and
a full `client.api` adapter bound to that exact transport. Pass `client.api`
into reusable package providers or required component `api` props. There is no
process-global adapter; every command has an explicit transport owner.
