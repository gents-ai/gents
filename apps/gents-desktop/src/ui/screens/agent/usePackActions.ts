import { useState } from "react";
import { toast } from "sonner";
import type { PackEditedChoice } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { refreshRuntimePackUxPlugins } from "@/contrib/runtime-door";
import { errorText } from "../../../lib/errorText";
import type { PackReviewRequest } from "@/lib/packReview";
import type { NodeView } from "../../../hooks/fleetStore";
import { forgetSetAsideReview } from "../packs/preview";
import { shortName } from "../packs/model";
import { refusalOf } from "../packs/review";
import {
  editedQuestion,
  failedWithDetails,
  packUses,
  refusedRemoval,
  removalQuestion,
  stillSelected,
  takenOut,
  type PackUse,
} from "../packs/removal";

/** What the person chose in an install's review. */
export type PackInstallChoice = {
  /** the digest the review showed: a pack that differs is refused */
  digest: string;
  /** the pack's coordinate, for the pages it brings */
  pack: string;
  edited: PackEditedChoice;
  grantAuthority: boolean;
  /** the folder access the review showed, as `PACK=AGENT` rows: only these
      agents are let into the folder */
  rootAgents: string[];
  /** the `NAME=PROFILE` model bindings the review previewed */
  inferenceSlots: string[];
};

/** Install, update and remove a pack on the desktop's own node. An install
    opens a review first (`review`), and only the review's Install writes;
    an update asks where it would replace someone's edits, and a removal
    before it takes what the pack installed with it. `busy` names the pack
    (or other work) under way; the pack's pages reload in every window once
    one finishes. */
export function usePackActions(refresh: () => Promise<void>) {
  const { actions } = useApp();
  const [busy, setBusy] = useState<string | null>(null);
  const [review, setReview] = useState<PackReviewRequest | null>(null);
  /* each review opened anew, so its choices start from nothing or from
     where it was set aside */
  const [opened, setOpened] = useState(0);
  const open = (request: PackReviewRequest) => {
    setOpened((n) => n + 1);
    setReview(request);
  };

  /* after the list: a reload waits behind any scan still running, and the
     list must not wait with it */
  async function refreshPages(pack: string) {
    try {
      await refreshRuntimePackUxPlugins(pack);
    } catch (error) {
      toast.error(`Couldn't refresh pack pages: ${errorText(error)}`);
    }
  }

  /** opens the review for a registry pack */
  function install(pack: string) {
    open({ package: pack, source: "registry" });
  }

  /** Installs what the review showed, pinned to its digest. A refusal is
      thrown for the review to show; nothing else closes it. */
  async function installReviewed(
    request: PackReviewRequest,
    choice: PackInstallChoice,
  ): Promise<void> {
    setBusy(choice.pack);
    try {
      await actions.installPack({
        package: request.package,
        edited: choice.edited,
        grantAuthority: choice.grantAuthority,
        rootAgents: choice.rootAgents,
        expectDigest: choice.digest,
        inferenceSlots: choice.inferenceSlots,
      });
      toast.success(`Installed ${choice.pack}`);
      forgetSetAsideReview(request);
      setReview(null);
      await refresh();
      void refreshPages(choice.pack);
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
      /* an update that would replace documents someone edited stops and
         names them; the person chooses keep or overwrite */
      if (refusalOf(error) === "edited") {
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

  /** The agents on `node` whose tools select the pack's plugins. Unknown
      when the plugins can't be listed: the removal then goes ahead as it
      did, and a refusal names who uses them. */
  async function usesOf(pack: string, node: NodeView | null): Promise<PackUse[]> {
    if (!node) return [];
    try {
      return packUses(node, await actions.listPackWireSources(), pack);
    } catch {
      return [];
    }
  }

  /** Takes each of `uses`' plugins out of its agent's tools, recording in
      `out` each one taken out; the one that couldn't be, or null. */
  async function takeOut(uses: readonly PackUse[], out: PackUse[]) {
    for (const use of uses) {
      for (const plugin of use.plugins) {
        try {
          await actions.wirePack({
            plugin,
            agent: use.agentDid,
            targetsFrom: [],
            remove: true,
            preview: false,
          });
        } catch (error) {
          return { use, plugin, error: errorText(error) };
        }
        const taken = out.find((u) => u.agentDid === use.agentDid);
        if (taken) taken.plugins.push(plugin);
        else out.push({ ...use, plugins: [plugin] });
      }
    }
    return null;
  }

  /** Whether the pack was removed. Agents whose tools select its plugins
      are named before it's removed. The runtime is asked first, so a pack
      another requires, or whose own edited documents the person keeps,
      stops before any agent's tools change; only its refusal to retire a
      plugin those tools select takes the plugins out, then asks again. A
      stop after that says what was taken out. */
  async function remove(pack: string, node: NodeView | null): Promise<boolean> {
    setBusy(pack);
    const out: PackUse[] = [];
    const saying = (message: string) =>
      [message, takenOut(out)].filter(Boolean).join(" ");
    const name = shortName(pack);
    try {
      const uses = await usesOf(pack, node);
      if (!window.confirm(removalQuestion(pack, uses))) return false;
      let edited: PackEditedChoice = "refuse";
      let unwired = false;
      for (;;) {
        try {
          await actions.removePack(pack, edited);
          break;
        } catch (error) {
          const text = errorText(error);
          if (edited === "refuse" && refusalOf(error) === "edited") {
            if (!window.confirm(editedQuestion(pack, text))) {
              if (out.length > 0) toast.error(saying(`${name} wasn't removed.`));
              return false;
            }
            edited = "overwrite";
          } else if (!unwired && uses.length > 0 && stillSelected(text)) {
            unwired = true;
            const failed = await takeOut(uses, out);
            if (failed) {
              failedWithDetails(
                saying(
                  `Couldn't take ${shortName(failed.plugin)} out of ${failed.use.name}'s tools, so ${name} wasn't removed.`,
                ),
                failed.error,
              );
              return false;
            }
          } else throw error;
        }
      }
      toast.success(`Removed ${pack}`);
      await refresh();
      void refreshPages(pack);
      return true;
    } catch (error) {
      const text = errorText(error);
      const refused = refusedRemoval(pack, text, node);
      if (refused || out.length > 0)
        failedWithDetails(saying(refused ?? `Couldn't remove ${name}.`), text);
      else toast.error(text);
      return false;
    } finally {
      setBusy(null);
    }
  }

  return {
    busy,
    setBusy,
    install,
    update,
    remove,
    review,
    /** which opening of a review this is */
    opened,
    /** opens the review for a file, a link or a registry pack */
    reviewInstall: open,
    /** closes the review and forgets it */
    closeReview: () => {
      if (review) forgetSetAsideReview(review);
      setReview(null);
    },
    /** closes the review, which stays set aside to take back to */
    setReviewAside: () => setReview(null),
    installReviewed,
  };
}
