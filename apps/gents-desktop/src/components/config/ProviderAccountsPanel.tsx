import { useCallback, useEffect, useMemo, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import type {
  DeploymentView,
  DesktopApiAdapter,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";

type Provider = "chatgpt-codex" | "xai-oauth";

type TauriEventWindow = Window & {
  __TAURI_INTERNALS__?: {
    transformCallback?: unknown;
  };
};

function hasTauriEventBridge(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof (window as TauriEventWindow).__TAURI_INTERNALS__?.transformCallback ===
      "function"
  );
}

const PROVIDERS: Array<{ id: Provider; title: string; description: string }> = [
  {
    id: "chatgpt-codex",
    title: "ChatGPT / Codex",
    description: "Use an eligible ChatGPT subscription for Codex inference.",
  },
  {
    id: "xai-oauth",
    title: "Grok / xAI",
    description: "Use SuperGrok or an eligible X Premium+ subscription.",
  },
];

export function ProviderAccountsPanel({
  api,
  deployment,
}: {
  api: DesktopApiAdapter;
  deployment: DeploymentView;
}) {
  const [accounts, setAccounts] = useState<ProviderAccountView[]>([]);
  const [loading, setLoading] = useState(true);
  const [busyProvider, setBusyProvider] = useState<Provider | null>(null);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [disconnecting, setDisconnecting] = useState<ProviderAccountView | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      if (!api.listProviderAccounts) {
        throw new Error("Provider accounts are not supported by this build");
      }
      setAccounts(await api.listProviderAccounts(deployment.agentDid));
      setError(null);
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setLoading(false);
    }
  }, [api, deployment.agentDid]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!hasTauriEventBridge()) return;
    let unlisten: (() => void) | undefined;
    void listen("desktop://client-updated", () => void refresh()).then(
      (stop) => (unlisten = stop),
    );
    return () => unlisten?.();
  }, [refresh]);

  async function connect(provider: Provider) {
    setBusyProvider(provider);
    setAuthUrl(null);
    setError(null);
    let unlisten: (() => void) | undefined;
    try {
      const eventName =
        provider === "chatgpt-codex"
          ? "desktop://codex-login-url"
          : "desktop://grok-login-url";
      if (hasTauriEventBridge()) {
        unlisten = await listen<{ url?: string | null }>(eventName, (event) =>
          setAuthUrl(event.payload?.url ?? null),
        );
      }
      if (provider === "chatgpt-codex") {
        await api.codexLogin(deployment.agentDid);
      } else {
        await api.grokLogin(deployment.agentDid);
      }
      await refresh();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      unlisten?.();
      setAuthUrl(null);
      setBusyProvider(null);
    }
  }

  async function cancelLogin() {
    if (busyProvider === "chatgpt-codex") await api.cancelCodexLogin();
    if (busyProvider === "xai-oauth") await api.cancelGrokLogin();
  }

  async function disconnect() {
    const account = disconnecting;
    setDisconnecting(null);
    if (!account) return;
    setBusyProvider(account.provider as Provider);
    try {
      if (!api.disconnectProviderAccount) {
        throw new Error("Provider accounts are not supported by this build");
      }
      await api.disconnectProviderAccount(deployment.agentDid, account.credentialId);
      await refresh();
    } catch (caught) {
      setError(caught instanceof Error ? caught.message : String(caught));
    } finally {
      setBusyProvider(null);
    }
  }

  const accountByProvider = useMemo(
    () => new Map(accounts.map((account) => [account.provider, account])),
    [accounts],
  );

  return (
    <section className="panel config-editor provider-accounts-panel">
      <header className="panel-header">
        <div>
          <p className="eyebrow">Inference</p>
          <h3>Provider Accounts</h3>
          <p className="muted">
            Connect subscription accounts separately from backend configuration. One
            account per provider is supported for this agent.
          </p>
        </div>
      </header>
      {error ? <p className="field-hint visible">{error}</p> : null}
      <div className="provider-account-grid" aria-busy={loading}>
        {PROVIDERS.map((provider) => {
          const account = accountByProvider.get(provider.id);
          const connected = Boolean(account?.enabled);
          const expired = account
            ? Date.parse(account.accessTokenExpiresAt) <= Date.now()
            : false;
          const backendCount = deployment.inferenceBackends.filter((backend) =>
            provider.id === "chatgpt-codex"
              ? backend.providerKind === "ChatGptCodex"
              : backend.providerKind === "XaiGrokOAuth",
          ).length;
          const busy = busyProvider === provider.id;
          return (
            <article className="provider-account-card" key={provider.id}>
              <div className="provider-account-card-heading">
                <div>
                  <h3>{provider.title}</h3>
                  <p className="muted">{provider.description}</p>
                </div>
                {connected ? (
                  <span className={`status-pill ${expired ? "yellow" : "green"}`}>
                    {expired ? "Expired" : "Connected"}
                  </span>
                ) : null}
              </div>
              {account ? (
                <dl className="provider-account-details">
                  {account.accountId ? (
                    <>
                      <dt>Account</dt>
                      <dd>{account.accountId}</dd>
                    </>
                  ) : null}
                  {account.planType ? (
                    <>
                      <dt>Plan</dt>
                      <dd>{account.planType}</dd>
                    </>
                  ) : null}
                  <dt>Credential</dt>
                  <dd className="mono">{account.credentialId}</dd>
                  <dt>Backends</dt>
                  <dd>{backendCount}</dd>
                  <dt>Expires</dt>
                  <dd>{new Date(account.accessTokenExpiresAt).toLocaleString()}</dd>
                </dl>
              ) : (
                <p className="muted small">No subscription account connected.</p>
              )}
              {busy && authUrl ? (
                <p className="muted small provider-account-auth">
                  Complete sign-in at <span className="mono">{authUrl}</span>
                </p>
              ) : null}
              <div className="config-actions">
                {busy ? (
                  <button
                    className="ghost-button"
                    onClick={() => void cancelLogin()}
                    type="button"
                  >
                    Cancel sign-in
                  </button>
                ) : null}
                {connected ? (
                  <button
                    className="ghost-button danger-button"
                    onClick={() => setDisconnecting(account!)}
                    type="button"
                  >
                    Disconnect
                  </button>
                ) : null}
                <button
                  className="primary-button"
                  data-testid={`provider-account-connect-${provider.id}`}
                  disabled={Boolean(busyProvider)}
                  onClick={() => void connect(provider.id)}
                  type="button"
                >
                  {busy ? "Waiting for sign-in…" : connected ? "Reconnect" : "Connect"}
                </button>
              </div>
            </article>
          );
        })}
      </div>
      <ConfirmDialog
        open={Boolean(disconnecting)}
        title="Disconnect provider account"
        message={`Disconnect ${disconnecting?.provider ?? "this account"}? Subscription backends using it will be unavailable until you reconnect.`}
        confirmLabel="Disconnect"
        danger
        onConfirm={() => void disconnect()}
        onCancel={() => setDisconnecting(null)}
      />
    </section>
  );
}
