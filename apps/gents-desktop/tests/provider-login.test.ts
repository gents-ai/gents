import { afterEach, expect, it, vi } from "vitest";
import { watchProviderLoginUrl } from "../src/ui/lib/providerLogin";

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
