import { afterEach, expect, it, vi } from "vitest";
import {
  createDesktopApiAdapter,
  tauriTransport,
} from "@source-inc/gents-desktop-client";
import { providerSignInState } from "../src/ui/screens/setup/inferenceSetupForm";
import { ceilingFromInit } from "../src/ui/screens/setup/OnboardingWizard";

const { listen, openExternalUrl } = vi.hoisted(() => ({
  listen: vi.fn(),
  openExternalUrl: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen }));
vi.mock("../src/lib/externalLinks", () => ({ openExternalUrl }));
afterEach(() => {
  Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
  vi.clearAllMocks();
});

it.each([
  ["openai", "desktop://codex-login-url"],
  ["anthropic", "desktop://claude-login-url"],
  ["grok", "desktop://grok-login-url"],
] as const)(
  "%s leaves automatic browser opening to native",
  async (provider, event) => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {},
      configurable: true,
    });
    const unlisten = vi.fn();
    listen.mockResolvedValue(unlisten);
    const onUrl = vi.fn();
    const api = createDesktopApiAdapter(tauriTransport());
    expect(await api.watchProviderLoginUrl?.(provider, onUrl)).toBe(unlisten);
    const [name, handler] = listen.mock.calls[0]!;
    expect(name).toBe(event);
    handler({ payload: { url: "https://example.test/sign-in" } });
    expect(onUrl).toHaveBeenCalledWith("https://example.test/sign-in");
    expect(openExternalUrl).not.toHaveBeenCalled();
  },
);

it("maps stored init.json ceilings onto reviewed setup authority", () => {
  expect(ceilingFromInit("Readwrite")).toBe("readwrite");
  expect(ceilingFromInit("readonly")).toBe("readonly");
  expect(ceilingFromInit("meta-only")).toBe("meta-only");
  expect(ceilingFromInit(null)).toBe("readwrite");
});

it("replaces stale provider sign-ins from the latest account snapshot", () => {
  const account = (provider: string, credentialId: string, enabled = true) => ({
    provider,
    credentialId,
    enabled,
    agentDid: "did:test:agent",
    accountId: null,
    planType: null,
    accessTokenExpiresAt: "2026-09-15T00:00:00Z",
    lastRefresh: null,
    pendingSave: false,
    accountRef: null,
    label: "Personal",
  });
  expect(
    providerSignInState([
      account("chatgpt-codex", "codex-current"),
      account("claude-subscription", "claude-disabled", false),
    ]),
  ).toEqual({ openai: "codex-current" });
  expect(providerSignInState([])).toEqual({});
});
