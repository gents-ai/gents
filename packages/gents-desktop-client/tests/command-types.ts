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
