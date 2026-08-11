import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

import { ProviderAccountsPanel } from "../src/components/config";
import { deployment } from "./config-panel-wiring/fixtures";

describe("ProviderAccountsPanel", () => {
  const listProviderAccounts = vi.fn();
  const codexLogin = vi.fn();
  const grokLogin = vi.fn();
  const disconnectProviderAccount = vi.fn();
  const api = {
    listProviderAccounts,
    disconnectProviderAccount,
    codexLogin,
    grokLogin,
    cancelCodexLogin: vi.fn(),
    cancelGrokLogin: vi.fn(),
  } as unknown as DesktopApiAdapter;

  beforeEach(() => {
    vi.clearAllMocks();
    listProviderAccounts.mockResolvedValue([]);
    codexLogin.mockResolvedValue({ credentialId: "chatgpt-codex:agent" });
    grokLogin.mockResolvedValue({ credentialId: "xai-oauth:agent" });
    disconnectProviderAccount.mockResolvedValue(undefined);
  });

  it("shows disconnected providers and connects Codex", async () => {
    render(<ProviderAccountsPanel api={api} deployment={deployment} />);
    expect(
      await screen.findAllByText("No subscription account connected."),
    ).toHaveLength(2);
    expect(screen.queryByText("Not connected")).not.toBeInTheDocument();

    fireEvent.click(screen.getByTestId("provider-account-connect-chatgpt-codex"));
    await waitFor(() => expect(codexLogin).toHaveBeenCalledWith(deployment.agentDid));
    expect(listProviderAccounts).toHaveBeenCalledTimes(2);
  });

  it("renders redacted account metadata and disconnects after confirmation", async () => {
    listProviderAccounts
      .mockResolvedValueOnce([
        {
          credentialId: "chatgpt-codex:agent",
          agentDid: deployment.agentDid,
          provider: "chatgpt-codex",
          accountId: "acct-1",
          planType: "plus",
          accessTokenExpiresAt: "2099-01-01T00:00:00Z",
          lastRefresh: null,
          enabled: true,
        },
      ])
      .mockResolvedValueOnce([]);
    render(<ProviderAccountsPanel api={api} deployment={deployment} />);

    expect(await screen.findByText("acct-1")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Disconnect" }));
    fireEvent.click(screen.getByTestId("confirm-dialog-confirm"));
    await waitFor(() =>
      expect(disconnectProviderAccount).toHaveBeenCalledWith(
        deployment.agentDid,
        "chatgpt-codex:agent",
      ),
    );
  });
});
