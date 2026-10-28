import type { createDesktopInvoker } from "../src/api/invoke.js";
import type { ManagedServerStatus } from "../src/generated/ManagedServerStatus.js";

declare const invoke: ReturnType<typeof createDesktopInvoker>;

const status: Promise<ManagedServerStatus> = invoke(
  "desktop_managed_server_status",
);
void status;
invoke("desktop_open_external_url", { url: "https://example.com" });
invoke("desktop_pack_install", {
  request: { package: "example/demo", registry: null },
});
invoke("desktop_pack_plugin_bind", {
  request: { plugin: "example/model", profile: null },
});

// @ts-expect-error The native inventory has no such command.
invoke("desktop_missing_command");
// @ts-expect-error A required native argument cannot be omitted.
invoke("desktop_open_external_url");
// @ts-expect-error Argument types come from the native signature.
invoke("desktop_open_external_url", { url: 42 });
// @ts-expect-error Tauri serializes command arguments in camelCase.
invoke("desktop_peer_remove", { peer_id: "peer" });
// @ts-expect-error A command with no arguments accepts no extra keys.
invoke("desktop_managed_server_status", { enabled: true });
// @ts-expect-error Native request fields remain required.
invoke("desktop_pack_install", { request: { edited: "keep" } });
invoke("desktop_pack_install", {
  // @ts-expect-error Drift handling uses the native enum.
  request: { package: "example/demo", edited: "replace" },
});
// @ts-expect-error The native result cannot be asserted as another type.
const wrongResult: Promise<string> = invoke("desktop_managed_server_status");
void wrongResult;
// @ts-expect-error The invoker's generic parameter names a native command.
invoke<string>("desktop_managed_server_status");

invoke("desktop_session_archive", {
  request: {
    nodeDid: "node",
    sessionId: "session",
    archived: true,
    expectedRevision: "9007199254740993",
  },
});
invoke("desktop_session_history", {
  request: {
    nodeDid: "node",
    visibility: "archived",
    query: null,
    cursor: null,
    limit: 30,
  },
});
invoke("desktop_session_archive", {
  request: {
    nodeDid: "node",
    sessionId: "session",
    archived: true,
    // @ts-expect-error Browser numbers cannot preserve native archive revisions.
    expectedRevision: 1,
  },
});

invoke("desktop_mailbox_observe", {
  request: { clientDid: "reader", itemId: "question" },
});
// @ts-expect-error The exact reader identity is required for a mailbox observation.
invoke("desktop_mailbox_observe", { request: { itemId: "question" } });
