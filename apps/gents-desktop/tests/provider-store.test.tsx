import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DesktopClientSnapshot,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";

import { useAccounts } from "../src/ui/hooks/useProviders";
import { publishSnapshot, testApp, withApp } from "./app-fixture";

const account = (credentialId: string) => ({ credentialId }) as ProviderAccountView;

/* two panels showing one agent's accounts, as the inference panel and a
   backend sheet open beside it do */
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

  it("reads an agent's accounts once when the client changes, however many panels show them", async () => {
    const listProviderAccounts = vi.fn().mockResolvedValue([account("a")]);
    const { app, second } = twoPanels(listProviderAccounts);
    await waitFor(() => expect(second.result.current.accounts).toHaveLength(1));
    const before = listProviderAccounts.mock.calls.length;

    act(() =>
      publishSnapshot(app, {
        bootstrap: { changed: true },
        client: null,
      } as unknown as DesktopClientSnapshot),
    );

    await waitFor(() =>
      expect(listProviderAccounts.mock.calls.length).toBe(before + 1),
    );
    await act(async () => {});
    expect(listProviderAccounts.mock.calls.length).toBe(before + 1);
  });
});
