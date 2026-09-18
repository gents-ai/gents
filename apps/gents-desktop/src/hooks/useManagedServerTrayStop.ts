import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

/** The native tray targets main; global event listeners would match in every view. */
export function useManagedServerTrayStop(api: DesktopApiAdapter) {
  useEffect(() => {
    if (!api.stopManagedServer || !("__TAURI_INTERNALS__" in window)) return;
    const view = getCurrentWindow();
    if (view.label !== "main") return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void view
      .listen("desktop://managed-server-tray-stop", () => {
        void api.stopManagedServer?.(true);
      })
      .then((cleanup) => {
        if (disposed) cleanup();
        else unlisten = cleanup;
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [api]);
}
