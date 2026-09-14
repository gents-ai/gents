import { afterEach, expect, it, vi } from "vitest";
import { watchProviderLoginUrl } from "../src/ui/lib/providerLogin";
import { providerSignInState } from "../src/ui/screens/setup/SetupScreen";

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

it.each(["openai", "anthropic", "grok"] as const)(
  "%s leaves automatic browser opening to native",
  async (provider) => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {},
      configurable: true,
    });
    const unlisten = vi.fn();
    listen.mockResolvedValue(unlisten);
    const onUrl = vi.fn();
    expect(await watchProviderLoginUrl(provider, onUrl)).toBe(unlisten);
    const handler = listen.mock.calls[0]![1];
    handler({ payload: { url: "https://example.test/sign-in" } });
    expect(onUrl).toHaveBeenCalledWith("https://example.test/sign-in");
    expect(openExternalUrl).not.toHaveBeenCalled();
  },
);

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
  });
  expect(
    providerSignInState([
      account("chatgpt-codex", "codex-current"),
      account("claude-subscription", "claude-disabled", false),
    ]),
  ).toEqual({ openai: "codex-current" });
  expect(providerSignInState([])).toEqual({});
});
