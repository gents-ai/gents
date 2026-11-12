import { useCallback, useEffect } from "react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import {
  BridgeInvokeError,
  type DefaultPack,
  type DefaultPackApproval,
  type DefaultPackFailure,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { useFleet } from "@/hooks/useFleet";
import { useHomeDid } from "@/hooks/useClient";
import { isLocalNode } from "@/lib/firstRun";
import { nodeOrFirst } from "../../../hooks/fleetStore";
import { errorText } from "../../../lib/errorText";
import { supportsLocalManagedServer } from "../../../lib/shellPlatform";
import { readInstalledPacks } from "./model";

/* The packs the Engineer starts with on the desktop's own node, read once per
   node and shared by the card over the shell and the Packs page, so an
   approval in one shows in the other. */
const starter = createStore<{
  /** the local node the list was read for */
  nodeDid: string | null;
  packs: DefaultPack[] | null;
  loading: boolean;
  error: string | null;
  busy: boolean;
  failed: DefaultPackFailure[];
}>(() => ({
  nodeDid: null,
  packs: null,
  loading: false,
  error: null,
  busy: false,
  failed: [],
}));

export function resetDefaultPacks() {
  starter.setState(starter.getInitialState(), true);
}

/** a starter pack that is offered: not installed, and its archive resolved */
export const isPending = (pack: DefaultPack) =>
  pack.state === "needs_approval" || pack.state === "ready";

export type DefaultPacksReview = ReturnType<typeof useDefaultPacks>;

/** The starter packs of the selected node when it is the desktop's own and
    this shell can host one; for any other node nothing is read and the list
    is empty. An approval restarts the local node around the install. */
export function useDefaultPacks() {
  const { actions, stores } = useApp();
  const homeDid = useHomeDid();
  const selected = stores.selection.use.nodeDid();
  const nodeDid = useFleet((state) => {
    const node = nodeOrFirst(state, selected);
    return node && isLocalNode(node, homeDid) && supportsLocalManagedServer()
      ? node.nodeDid
      : null;
  });
  const state = useStore(starter);

  const read = useCallback(
    async (nodeDid: string) => {
      starter.setState({
        nodeDid,
        packs: null,
        loading: true,
        error: null,
        failed: [],
      });
      const current = () => starter.getState().nodeDid === nodeDid;
      try {
        const { packs } = await actions.listDefaultPacks();
        if (current()) starter.setState({ packs, loading: false });
      } catch (error) {
        if (!current()) return;
        /* a shell with no local home has no starter packs to offer */
        const none = error instanceof BridgeInvokeError && error.code === "unsupported";
        starter.setState({
          packs: [],
          loading: false,
          error: none ? null : errorText(error),
        });
      }
    },
    [actions],
  );

  useEffect(() => {
    if (nodeDid !== null && starter.getState().nodeDid !== nodeDid) void read(nodeDid);
  }, [nodeDid, read]);

  const approve = useCallback(
    async (packs: DefaultPackApproval[]) => {
      if (nodeDid === null || starter.getState().busy || packs.length === 0) return;
      starter.setState({ busy: true, error: null, failed: [] });
      try {
        const result = await actions.approveDefaultPacks(packs);
        starter.setState({ packs: result.packs, failed: result.failed });
      } catch (error) {
        starter.setState({ error: errorText(error) });
      } finally {
        starter.setState({ busy: false });
        /* the node came back with what was installed; an install that threw
           may still have restarted it */
        void readInstalledPacks(actions);
        void actions.refreshSnapshot();
      }
    },
    [actions, nodeDid],
  );

  const retry = useCallback(async () => {
    if (nodeDid !== null && !starter.getState().busy) await read(nodeDid);
  }, [nodeDid, read]);

  const mine = nodeDid !== null && state.nodeDid === nodeDid;
  return {
    /** the local node the list is for, or null where none is offered */
    nodeDid,
    packs: mine ? (state.packs ?? []) : [],
    loading: mine && state.loading,
    error: mine ? state.error : null,
    busy: mine && state.busy,
    /** the packs the last approval could not install */
    failed: mine ? state.failed : [],
    approve,
    retry,
  };
}
