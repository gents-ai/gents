import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";

import type { InviteAcceptance, InvitePreview } from "../../hooks/sharing";

/** the node configuration's section where sharing lives */
export const SHARING_SECTION = "sharing";

/** where the join dialog is: the field for a link or code (with what it
    holds and why it couldn't be read), or the preview of the invite one
    names, once read or still to read */
export type JoinPanel =
  | { step: "enter"; text: string; error: string | null }
  | { step: "preview"; invite: string; preview: InvitePreview | null };

/** a request this desktop sent from an invite, as accepting it answered */
export type SentJoin = {
  invite: string;
  preview: InvitePreview;
  acceptance: InviteAcceptance;
};

/* kept while the app runs, so a request sent stays in view after the person
   leaves the page, and a link opened from outside waits for it */
const joining = createStore<{ panel: JoinPanel | null; sent: SentJoin[] }>(() => ({
  panel: null,
  sent: [],
}));

export const useJoinPanel = () => useStore(joining, (s) => s.panel);
export const useSentJoins = () => useStore(joining, (s) => s.sent);

/** opens the join dialog, on an invite's preview when one is given */
export const startJoin = (invite?: string) =>
  joining.setState({
    panel: invite?.trim()
      ? { step: "preview", invite: invite.trim(), preview: null }
      : { step: "enter", text: "", error: null },
  });

/** shows the preview of an invite already read */
export const showInvite = (invite: string, preview: InvitePreview) =>
  joining.setState({ panel: { step: "preview", invite, preview } });

/** goes back to the field, holding `text`, with why it couldn't be read */
export const enterInvite = (text: string, error: string | null = null) =>
  joining.setState({ panel: { step: "enter", text, error } });

export const closeJoin = () => joining.setState({ panel: null });

/** records a request sent and closes the dialog */
export const joinSent = (join: SentJoin) =>
  joining.setState((s) => ({
    panel: null,
    sent: [
      join,
      ...s.sent.filter((j) => j.acceptance.requestId !== join.acceptance.requestId),
    ],
  }));

export const forgetJoin = (requestId: string) =>
  joining.setState((s) => ({
    sent: s.sent.filter((j) => j.acceptance.requestId !== requestId),
  }));

export const resetJoins = () => joining.setState({ panel: null, sent: [] });
