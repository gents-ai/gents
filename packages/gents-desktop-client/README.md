# @source-inc/gents-desktop-client

Transport interface, default Tauri transport, shared store/refresh coordinator, and generated view-model bindings for Gents desktop packages.

```ts
import { createDesktopClient, createDesktopStore, tauriTransport } from "@source-inc/gents-desktop-client";
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
