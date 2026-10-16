import { screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { BackendEditor } from "../src/ui/screens/agent/BackendEditor";
import { someNames } from "../src/ui/screens/agent/behaviorDraft";
import { accountWarnings } from "../src/ui/screens/agent/ProviderAccounts";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const profile = (n: number) => ({
  ...deployment.inferenceProfiles[0]!,
  profile_id: `profile-${n}`,
  display_name: `A profile with a name long enough to truncate ${n}`,
  backend_id: deployment.inferenceBackends[0]!.backendId,
});

describe("lists of what uses a document", () => {
  it("names a few, then says how many more", () => {
    expect(someNames(["a", "b"])).toBe("a, b");
    expect(someNames(["a", "b", "c", "d"])).toBe("a, b, c, d");
    expect(someNames(["a", "b", "c", "d", "e"])).toBe("a, b, c and 2 more");
  });

  it("links a backend's profiles, not agents, the first few by name", () => {
    const profiles = [1, 2, 3, 4, 5].map(profile);
    const view = { ...deployment, inferenceProfiles: profiles };
    const backend = deployment.inferenceBackends[0]!;
    renderIn(
      testApp({ deployments: [view] }),
      <BackendEditor deployment={view} backend={backend} accounts={[]} />,
    );
    const row = screen
      .getByText("Used by")
      .closest("[data-slot=field-row]") as HTMLElement;
    expect(row).toHaveTextContent("Delete is blocked while a profile uses it.");
    const links = within(row).getAllByRole("link");
    expect(links).toHaveLength(3);
    expect(links[0]).toHaveAttribute("title", profiles[0]!.display_name);
    expect(links[0]!.getAttribute("href")).toContain("profile-1");
    expect(row).toHaveTextContent("and 2 more");
  });

  it("keeps an account's warning to a few profile names", () => {
    const profiles = [1, 2, 3, 4, 5].map(profile);
    const backend = {
      ...deployment.inferenceBackends[0]!,
      providerKind: "ClaudeCliSubscription",
    };
    const account = {
      credentialId: "cred",
      nodeDid: deployment.nodeDid,
      provider: "claude-subscription",
      accountId: "acct",
      planType: null,
      accessTokenExpiresAt: null,
      lastRefresh: null,
      enabled: true,
      accountRef: null,
      label: "Claude",
    };
    const warnings = accountWarnings(
      {
        ...deployment,
        inferenceProfiles: profiles,
        inferenceBackends: [backend],
      } as never,
      [account] as never,
      account as never,
    );
    expect(warnings.disconnect).toContain("and 2 more");
    expect(warnings.disconnect).not.toContain(profiles[4]!.display_name);
  });
});
