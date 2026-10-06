/* The UI harness stands in for the bridge in browser tests and Bombadil. A
   backend it projects must have the bridge's shape: InferenceBackendView.tags
   is a Vec<String> on the bridge, so it is always a list. Bombadil found the
   harness adding a backend with no tags, and the backend editor's Tags row
   then crashed on the Providers page. */
import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { renderIn, testApp } from "./app-fixture";
import type { InferenceBackend } from "@source-inc/gents-desktop-client";

vi.mock("@/lib/router", () => ({ href: () => "#", navigate: vi.fn() }));

import { createDesktopUiHarness } from "./ui-harness/desktopHarness";
import { InferencePanel } from "../src/ui/screens/agent/InferencePanel";

const backendDocument = (backend_id: string): InferenceBackend =>
  ({
    agent_did: "did:key:z6MkBombadilAgent",
    backend_id,
    name: "Added backend",
    provider_kind: "OpenAiCompatible",
    endpoint: "http://127.0.0.1:9000/v1",
    auth: { kind: "unauthenticated" },
    enabled: true,
  }) as InferenceBackend;

describe("harness backends keep the bridge's contract", () => {
  it("projects tags as a list for seeded, saved, applied and patched backends", async () => {
    const harness = createDesktopUiHarness();
    const api = harness.adapter;
    await api.saveBackendConfig({ document: backendDocument("backend-saved") });
    await api.applyConfigComponents({
      document: {
        agent_principal: { agent_did: "did:key:z6MkBombadilAgent" },
        inference_backends: [backendDocument("backend-applied")],
      },
    });
    await api.patchConfigComponents({
      agentDid: "did:key:z6MkBombadilAgent",
      patches: [
        {
          collection: "InferenceBackend",
          id: "backend-saved",
          changes: { tags: null },
        },
      ],
    });
    const snapshot = await api.fetchDesktopSnapshot();
    const backends = snapshot.client!.deployments[0]!.inferenceBackends;
    expect(backends.map((b) => b.backendId)).toEqual(
      expect.arrayContaining(["backend-saved", "backend-applied"]),
    );
    for (const backend of backends) {
      expect(Array.isArray(backend.tags), backend.backendId).toBe(true);
      expect(Array.isArray(backend.advertisedModels), backend.backendId).toBe(true);
    }
  });

  it("opens a backend added after onboarding in its editor", async () => {
    const harness = createDesktopUiHarness();
    const api = harness.adapter;
    await api.saveBackendConfig({ document: backendDocument("backend-saved") });
    const deployment = (await api.fetchDesktopSnapshot()).client!.deployments[0]!;
    renderIn(
      testApp({ api }),
      <InferencePanel deployment={deployment} item="backend-saved" />,
    );
    expect(screen.getByRole("textbox", { name: "Tags" })).toBeInTheDocument();
  });

  it("shows an Anthropic key backend as an API-key backend", async () => {
    const harness = createDesktopUiHarness();
    const api = harness.adapter;
    /* applied, not saved: the harness save path reports any env var as a
       stored key, while its apply path matches the bridge */
    await api.applyConfigComponents({
      document: {
        agent_principal: { agent_did: "did:key:z6MkBombadilAgent" },
        inference_backends: [
          {
            ...backendDocument("anthropic-key"),
            provider_kind: "AnthropicApiKey",
            endpoint: "https://api.anthropic.com/v1",
            auth: { kind: "environment", variable: "ANTHROPIC_API_KEY" },
          } as InferenceBackend,
        ],
      },
    });
    const deployment = (await api.fetchDesktopSnapshot()).client!.deployments[0]!;
    renderIn(testApp({ api }), <InferencePanel deployment={deployment} />);
    const meta = screen.getByText(/Anthropic API key · key from ANTHROPIC_API_KEY/);
    expect(meta.textContent).not.toMatch(/signed in/);
  });
});
