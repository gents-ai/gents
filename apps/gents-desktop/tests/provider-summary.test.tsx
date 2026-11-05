import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { BackendEditor } from "../src/ui/screens/agent/BackendEditor";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const cardOf = (label: string) =>
  screen.getAllByText(label).find((el) => el.closest(".group\\/summary"))!
    .parentElement!;

describe("a provider's page", () => {
  it("opens on its name, kind and what uses it, its key a fact", () => {
    const backend = { ...deployment.inferenceBackends[0]!, models: ["m1", "m2"] };
    renderIn(
      testApp({ deployments: [deployment] }),
      <BackendEditor deployment={deployment} backend={backend} accounts={[]} />,
    );
    expect(screen.getByRole("heading", { name: "Backend A" })).toBeVisible();
    expect(cardOf("Used by")).toHaveTextContent(/1 profile · \d+ agents?/);
    expect(cardOf("Models")).toHaveTextContent("2 models");
    expect(cardOf("Key")).toHaveTextContent("No key");
    expect(screen.queryByText("Not signed in")).toBeNull();
  });

  it("says so when a subscription has no account to sign in with", () => {
    const backend = {
      ...deployment.inferenceBackends[0]!,
      providerKind: "ClaudeCliSubscription",
      accountRef: null,
    };
    renderIn(
      testApp({ deployments: [deployment] }),
      <BackendEditor deployment={deployment} backend={backend} accounts={[]} />,
    );
    expect(cardOf("Account")).toHaveTextContent("Not signed in");
    /* the header carries the problem too */
    expect(screen.getAllByText("Not signed in")).toHaveLength(2);
  });

  it("describes a private credential without claiming its secret type", () => {
    const backend = {
      ...deployment.inferenceBackends[0]!,
      authKind: "credential",
      apiKeyConfigured: true,
    };
    renderIn(
      testApp({ deployments: [deployment] }),
      <BackendEditor deployment={deployment} backend={backend} accounts={[]} />,
    );
    expect(cardOf("Key")).toHaveTextContent("Stored credential");
  });
});
