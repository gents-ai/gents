import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { BackendEditor } from "../src/ui/screens/agent/BackendEditor";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const claude = (fields: object = {}) => ({
  ...deployment.inferenceBackends[0]!,
  backendId: "claude",
  name: "Claude",
  providerKind: "ClaudeCliSubscription" as const,
  endpoint: "claude-cli://subscription",
  authKind: "node_oauth",
  accountRef: null,
  allowOverage: null,
  ...fields,
});
const account = (credentialId: string, enabled = true) => ({
  credentialId,
  nodeDid: deployment.nodeDid,
  provider: "claude-subscription",
  accountId: null,
  planType: null,
  accessTokenExpiresAt: "2099-01-01T00:00:00Z",
  lastRefresh: null,
  enabled,
  pendingSave: false,
  accountRef: credentialId === "first" ? null : credentialId,
  label: `label-${credentialId}`,
});

function render(backend: ReturnType<typeof claude>, accounts = [account("first")]) {
  const api = {
    setBackendRotation: vi.fn().mockResolvedValue({}),
    patchConfigComponents: vi.fn().mockResolvedValue({}),
    listProviderAccounts: vi.fn().mockResolvedValue(accounts),
  };
  const node = { ...deployment, inferenceBackends: [backend] };
  renderIn(
    testApp({ api, deployments: [node] }),
    <BackendEditor deployment={node} backend={backend} accounts={accounts} />,
  );
  return { api, user: userEvent.setup() };
}

describe("a Claude backend's account rotation", () => {
  it("offers the pool and sends the choice as the operator command", async () => {
    const { api, user } = render(claude());
    await user.click(screen.getByRole("combobox", { name: "Accounts" }));
    await user.click(await screen.findByRole("option", { name: "Account pool" }));
    await waitFor(() => expect(api.setBackendRotation).toHaveBeenCalledTimes(1));
    expect(api.setBackendRotation).toHaveBeenCalledWith({
      nodeDid: deployment.nodeDid,
      backendId: "claude",
      rotation: { kind: "pool" },
    });
    expect(api.patchConfigComponents).not.toHaveBeenCalled();
  });

  it("lists a pool's enabled accounts in order and toggles paid extra usage", async () => {
    const accounts = [account("first"), account("off", false), account("second")];
    const { api, user } = render(
      claude({ authKind: "node_oauth_pool", allowOverage: false }),
      accounts,
    );
    const list = screen.getByRole("list", { name: "Pool accounts" });
    expect(
      within(list)
        .getAllByRole("listitem")
        .map((item) => item.textContent),
    ).toEqual(["1. label-first", "2. label-second"]);
    const overage = screen.getByRole("switch", { name: "Use paid extra usage" });
    expect(overage).not.toBeChecked();
    await user.click(overage);
    await waitFor(() => expect(api.setBackendRotation).toHaveBeenCalledTimes(1));
    expect(api.setBackendRotation).toHaveBeenCalledWith({
      nodeDid: deployment.nodeDid,
      backendId: "claude",
      rotation: { kind: "pool", allow_overage: true },
    });
  });

  it("goes back to one account", async () => {
    const { api, user } = render(
      claude({ authKind: "node_oauth_pool", allowOverage: true }),
    );
    expect(screen.getByRole("switch", { name: "Use paid extra usage" })).toBeChecked();
    await user.click(screen.getByRole("combobox", { name: "Accounts" }));
    await user.click(await screen.findByRole("option", { name: "One account" }));
    await waitFor(() =>
      expect(api.setBackendRotation).toHaveBeenCalledWith({
        nodeDid: deployment.nodeDid,
        backendId: "claude",
        rotation: { kind: "single" },
      }),
    );
  });

  it("keeps a pool when another field is saved", async () => {
    const { api, user } = render(
      claude({ authKind: "node_oauth_pool", allowOverage: true }),
    );
    const name = screen.getByLabelText("Name");
    await user.clear(name);
    await user.type(name, "Claude pool");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(api.patchConfigComponents).toHaveBeenCalledTimes(1));
    const [{ patches }] = api.patchConfigComponents.mock.calls[0]!;
    expect(patches[0].changes.name).toBe("Claude pool");
    expect(patches[0].changes).not.toHaveProperty("auth");
  });

  it("offers no pool to other subscription providers", () => {
    render(claude({ providerKind: "ChatGptCodex" }));
    expect(screen.queryByRole("combobox", { name: "Accounts" })).toBeNull();
  });
});
