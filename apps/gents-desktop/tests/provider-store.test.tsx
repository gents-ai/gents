import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopClientSnapshot,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { useAccounts } from "../src/ui/hooks/useProviders";
import { publishSnapshot, testApp, withApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const account = (credentialId: string) => ({ credentialId }) as ProviderAccountView;

function twoPanels(listProviderAccounts: ReturnType<typeof vi.fn>) {
  const app = testApp({
    api: { listProviderAccounts },
    snapshot: { bootstrap: {}, client: null },
  });
  const wrapper = withApp(app);
  const panel = () => renderHook(() => useAccounts("did:key:agent"), { wrapper });
  return { app, first: panel(), second: panel() };
}

describe("provider accounts held once for every panel", () => {
  it("shows a sign-in reloaded from one panel in the other", async () => {
    const listProviderAccounts = vi.fn().mockResolvedValue([account("a")]);
    const { first, second } = twoPanels(listProviderAccounts);
    await waitFor(() => expect(second.result.current.accounts).toHaveLength(1));

    listProviderAccounts.mockResolvedValue([account("a"), account("b")]);
    await act(() => first.result.current.reload());

    expect(second.result.current.accounts).toHaveLength(2);
  });

  it("reads an agent's accounts once when its account backends change, however many panels show them", async () => {
    const listProviderAccounts = vi.fn().mockResolvedValue([account("a")]);
    const { app, second } = twoPanels(listProviderAccounts);
    await waitFor(() => expect(second.result.current.accounts).toHaveLength(1));
    const before = listProviderAccounts.mock.calls.length;
    const withBackend = (name: string) =>
      ({
        bootstrap,
        client: {
          deployments: [
            {
              ...deployment,
              nodeDid: "did:key:agent",
              inferenceBackends: [
                { ...deployment.inferenceBackends[0]!, backendId: "claude", name },
              ],
            },
          ],
        },
      }) as unknown as DesktopClientSnapshot;

    act(() => publishSnapshot(app, withBackend("Claude")));
    await waitFor(() =>
      expect(listProviderAccounts.mock.calls.length).toBe(before + 1),
    );
    await act(async () => {});
    expect(listProviderAccounts.mock.calls.length).toBe(before + 1);

    /* a snapshot that leaves the account backends as they were (a heartbeat,
       a probe) lists nothing again */
    act(() => publishSnapshot(app, withBackend("Claude")));
    await act(async () => {});
    expect(listProviderAccounts.mock.calls.length).toBe(before + 1);
  });
});
