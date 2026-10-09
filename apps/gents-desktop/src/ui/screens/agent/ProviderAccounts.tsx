/* The provider accounts a subscription backend runs on: which account a
   backend references, what is wrong with it, and the rows and dialogs that
   manage it. */
import type { NodeView } from "../../../hooks/fleetStore";
import { useState } from "react";
import { toast } from "sonner";
import type {
  InferenceBackendView,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { setupErrorMessage } from "../../../lib/setupErrors";
import { FactRow } from "./editors";
import { ConfirmDelete } from "./ListDetail";
import { Row } from "./rows";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { RenameDialog } from "../AgentsScreen";
import { useApp } from "@/app/AppContext";
import { SUBSCRIPTION } from "./inferenceKinds";

/* the stored account a subscription backend runs on: the one with the
   backend's reference; no reference is the provider's original account */
function referencedAccount(
  accounts: readonly ProviderAccountView[],
  provider: string,
  accountRef: string | null | undefined,
) {
  return accounts.find(
    (a) =>
      a.provider === provider &&
      !a.pendingSave &&
      (a.accountRef ?? null) === (accountRef ?? null),
  );
}

/* the account a subscription backend runs on, if it is stored here */
export function backendAccount(
  accounts: readonly ProviderAccountView[],
  b: InferenceBackendView,
) {
  const sub = SUBSCRIPTION[b.providerKind ?? ""];
  return sub ? referencedAccount(accounts, sub.provider, b.accountRef) : undefined;
}

/* a backend as a profile sees it: a subscription backend serves only on its
   enabled account and is named "<label> · <provider>"; others always serve */
export function profileBackend(
  accounts: readonly ProviderAccountView[],
  b: InferenceBackendView,
) {
  const sub = SUBSCRIPTION[b.providerKind ?? ""];
  const account = backendAccount(accounts, b);
  return {
    provider: sub?.provider,
    account,
    usable: !sub || Boolean(account?.enabled),
    label: sub && account ? `${account.label} · ${sub.title}` : (b.name ?? b.backendId),
  };
}

/* what an account action leaves behind: the profiles that fail their next
   turn, in the CLI's words, and for remove which backends go with it (an
   added account's backends that no profile uses, as the CLI's remove) */
export function accountWarnings(
  deployment: NodeView,
  accounts: readonly ProviderAccountView[],
  account: ProviderAccountView,
) {
  const backends = deployment.inferenceBackends.filter(
    (b) => backendAccount(accounts, b)?.credentialId === account.credentialId,
  );
  const usedBy = (b: InferenceBackendView) =>
    deployment.inferenceProfiles.filter((p) => p.backend_id === b.backendId);
  const profiles = backends.flatMap(usedBy).map((p) => p.display_name ?? p.profile_id);
  const names = (list: InferenceBackendView[]) =>
    list.map((b) => b.name ?? b.backendId).join(", ");
  const gone = account.accountRef ? backends.filter((b) => !usedBy(b).length) : [];
  const kept = backends.filter((b) => !gone.includes(b));
  const disconnect = profiles.length
    ? `These profiles use this account and fail their next turn until moved to another backend: ${profiles.join(", ")}.`
    : "";
  const remove = [
    disconnect,
    gone.length ? `Deletes its unused backends: ${names(gone)}.` : "",
    kept.length
      ? `${account.accountRef ? "Keeps the backends a profile uses" : "Keeps its backends"}: ${names(kept)}.`
      : "",
  ]
    .filter(Boolean)
    .join(" ");
  return { disconnect, remove };
}

export type AccountAction = {
  action: "rename" | "disconnect" | "remove";
  account: ProviderAccountView;
};

/* rename, disconnect and remove for an account, opened from its row's menu;
   none of them edits a profile or a backend */
export function AccountDialogs({
  deployment,
  accounts,
  acting,
  onClose,
}: {
  deployment: NodeView;
  accounts: readonly ProviderAccountView[];
  acting: AccountAction | null;
  onClose: () => void;
}) {
  const { actions } = useApp();
  const [busy, setBusy] = useState(false);
  const account = acting?.account;
  const warnings = account && accountWarnings(deployment, accounts, account);
  const rename = async (label: string) => {
    if (!account) return;
    if (
      accounts.some(
        (a) =>
          a.provider === account.provider &&
          !a.pendingSave &&
          a.credentialId !== account.credentialId &&
          a.label === label,
      )
    )
      throw new Error(
        `Another account is already labelled “${label}”. Choose another label.`,
      );
    await actions.renameProviderAccount(
      deployment.agentDid,
      account.credentialId,
      label,
    );
    toast("Renamed");
  };
  const disconnect = async () => {
    if (!account) return;
    setBusy(true);
    try {
      await actions.disconnectProviderAccount(
        deployment.agentDid,
        account.credentialId,
      );
      toast("Disconnected");
      onClose();
    } catch (error) {
      toast(`Disconnect failed: ${setupErrorMessage(error)}`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <RenameDialog
        key={account?.credentialId ?? "none"}
        title="Rename account"
        description="The label this account goes by on this agent."
        value={acting?.action === "rename" ? account!.label : null}
        onSave={rename}
        onClose={onClose}
      />
      <Dialog
        open={acting?.action === "disconnect"}
        onOpenChange={(open) => !open && !busy && onClose()}
      >
        <DialogContent aria-modal="true">
          <DialogHeader>
            <DialogTitle>Disconnect {account?.label}?</DialogTitle>
            <DialogDescription>
              Signing in to it again reconnects it. {warnings?.disconnect}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={onClose}>
              Keep connected
            </Button>
            <Button variant="destructive" disabled={busy} onClick={disconnect}>
              {busy ? "Disconnecting…" : "Disconnect now"}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {account && (
        <ConfirmDelete
          label={account.label}
          noun="account"
          open={acting?.action === "remove"}
          onOpenChange={(open) => !open && onClose()}
          onDelete={() =>
            actions.removeProviderAccount(deployment.agentDid, account.credentialId)
          }
          warning={warnings?.remove}
        />
      )}
    </>
  );
}

/* the account card for a subscription backend */
export function AccountRows({
  deployment,
  kind,
  accountRef,
  accounts,
}: {
  deployment: NodeView;
  kind: string;
  accountRef: string | null;
  accounts: readonly ProviderAccountView[];
}) {
  const { actions } = useApp();
  const sub = SUBSCRIPTION[kind]!;
  const stored = referencedAccount(accounts, sub.provider, accountRef);
  const account = stored?.enabled ? stored : undefined;
  /* signing in here would add a new account, never this one */
  const elsewhere = accountRef !== null && !stored;
  const unsaved = accounts.some((a) => a.provider === sub.provider && a.pendingSave);
  const [busy, setBusy] = useState(false);
  const [confirmingDisconnect, setConfirmingDisconnect] = useState(false);
  const signIn = async () => {
    setBusy(true);
    try {
      await actions.signInToProvider(deployment.agentDid, sub.login);
      toast("Signed in");
    } catch (error) {
      toast(`Sign in failed: ${setupErrorMessage(error)}`);
    } finally {
      setBusy(false);
    }
  };
  const retrySave = async () => {
    setBusy(true);
    try {
      await actions.retrySaveProviderAccount(deployment.agentDid, sub.provider);
      toast("Signed in");
    } catch (error) {
      toast(`Save failed: ${setupErrorMessage(error)}`);
    } finally {
      setBusy(false);
    }
  };
  const disconnect = async () => {
    if (!account) return;
    setBusy(true);
    try {
      await actions.disconnectProviderAccount(
        deployment.agentDid,
        account.credentialId,
      );
      toast("Disconnected");
      setConfirmingDisconnect(false);
    } catch (error) {
      toast(
        `Disconnect failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    } finally {
      setBusy(false);
    }
  };
  if (elsewhere)
    return (
      <Row label="Account" description={sub.note}>
        <span className="text-sm text-muted-foreground">account not on this node</span>
      </Row>
    );
  return (
    <>
      <Row label="Account" description={sub.note}>
        <span className="flex items-center gap-2">
          {stored && (
            <Badge variant="secondary">{account ? "Connected" : "Disabled"}</Badge>
          )}
          {account && !confirmingDisconnect && (
            <Button
              size="sm"
              variant="quiet"
              disabled={busy}
              onClick={() => setConfirmingDisconnect(true)}
            >
              Disconnect
            </Button>
          )}
          {account && confirmingDisconnect && (
            <>
              <span className="text-xs text-muted-foreground">
                Disconnect {sub.title}?
              </span>
              <Button
                size="sm"
                variant="quiet"
                disabled={busy}
                onClick={() => setConfirmingDisconnect(false)}
              >
                Keep connected
              </Button>
              <Button
                size="sm"
                variant="destructive"
                disabled={busy}
                onClick={disconnect}
              >
                {busy ? "Disconnecting…" : "Disconnect now"}
              </Button>
            </>
          )}
          {unsaved && actions.canRetryProviderSave ? (
            <Button size="sm" variant="brand" disabled={busy} onClick={retrySave}>
              Retry save
            </Button>
          ) : null}
          <Button
            size="sm"
            variant={stored || unsaved ? "outline" : "brand"}
            disabled={busy}
            onClick={signIn}
          >
            {busy ? "Signing in…" : stored ? "Reconnect" : "Connect"}
          </Button>
        </span>
      </Row>
      {stored && (
        <>
          <FactRow label="Signed in as">
            {stored.accountId ?? "Account identity unavailable — reconnect to refresh"}
            {stored.planType ? ` · ${stored.planType}` : ""}
          </FactRow>
          <FactRow label="Label">{stored.label}</FactRow>
        </>
      )}
    </>
  );
}
