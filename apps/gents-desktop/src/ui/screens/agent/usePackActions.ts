import { useState } from "react";
import { toast } from "sonner";
import type { PackEditedChoice } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { refreshRuntimePackUxPlugins } from "@/contrib/runtime-door";
import { errorText } from "../../../lib/errorText";
import type { PackReviewRequest } from "@/lib/packReview";
import { forgetSetAsideReview } from "../packs/preview";

/** An install or update that would replace documents someone edited stops and
    names them; the person chooses keep or overwrite. Read from the error's
    words, since the install reports no typed refusal: replace with typed
    errors from core. */
function editedChoice(error: unknown): boolean {
  return errorText(error).includes("--overwrite");
}

/** What the person chose in an install's review. */
export type PackInstallChoice = {
  /** the digest the review showed: a pack that differs is refused */
  digest: string;
  /** the pack's coordinate, for the pages it brings */
  pack: string;
  edited: PackEditedChoice;
  grantAuthority: boolean;
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
