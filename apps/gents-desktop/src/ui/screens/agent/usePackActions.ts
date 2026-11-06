import { useState } from "react";
import { toast } from "sonner";
import type { PackEditedChoice } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { refreshRuntimePackUxPlugins } from "@/contrib/runtime-door";
import { errorText } from "../../../lib/errorText";

/** An install or update that would replace documents someone edited stops and
 * names them; the person chooses keep or overwrite. */
function editedChoice(error: unknown): boolean {
  return errorText(error).includes("--overwrite");
}

/** Install, update and remove a pack on the desktop's own node, asking first
    where the pack would gain authority, replace someone's edits, or take
    what it installed with it. `busy` names the pack (or other work) under
    way; the pack's pages reload in every window once one finishes. */
export function usePackActions(refresh: () => Promise<void>) {
  const { actions } = useApp();
  const [busy, setBusy] = useState<string | null>(null);

  /* after the list: a reload waits behind any scan still running, and the
     list must not wait with it */
  async function refreshPages(pack: string) {
    try {
      await refreshRuntimePackUxPlugins(pack);
    } catch (error) {
      toast.error(`Couldn't refresh pack pages: ${errorText(error)}`);
    }
  }

  async function install(
    pack: string,
    edited: PackEditedChoice = "refuse",
    grantAuthority = false,
  ): Promise<void> {
    setBusy(pack);
    try {
      await actions.installPack({ package: pack, edited, grantAuthority });
      toast.success(`Installed ${pack}`);
      await refresh();
      void refreshPages(pack);
    } catch (error) {
      const text = errorText(error);
      if (
        text.includes("--grant-authority") &&
        window.confirm(`${text}\n\nAllow it?`)
      ) {
        return install(pack, edited, true);
      }
      if (editedChoice(error)) {
        const overwrite = window.confirm(
          `${text}\n\nOK replaces them, Cancel keeps your edits.`,
        );
        return install(pack, overwrite ? "overwrite" : "keep", grantAuthority);
      }
      toast.error(text);
    } finally {
      setBusy(null);
    }
  }

  async function update(
    pack: string,
    edited: PackEditedChoice = "refuse",
  ): Promise<void> {
    setBusy(pack);
    try {
      await actions.updatePack(pack, edited);
      toast.success(`Updated ${pack}`);
      await refresh();
      void refreshPages(pack);
    } catch (error) {
      if (editedChoice(error)) {
        const overwrite = window.confirm(
          `${errorText(error)}\n\nOK replaces them, Cancel keeps your edits.`,
        );
        return update(pack, overwrite ? "overwrite" : "keep");
      }
      toast.error(errorText(error));
    } finally {
      setBusy(null);
    }
  }

  /** whether the pack was removed */
  async function remove(pack: string): Promise<boolean> {
    if (!window.confirm(`Remove ${pack} and everything it installed?`)) return false;
    setBusy(pack);
    try {
      await actions.removePack(pack);
      toast.success(`Removed ${pack}`);
      await refresh();
      void refreshPages(pack);
      return true;
    } catch (error) {
      toast.error(errorText(error));
      return false;
    } finally {
      setBusy(null);
    }
  }

  return { busy, setBusy, install, update, remove };
}
