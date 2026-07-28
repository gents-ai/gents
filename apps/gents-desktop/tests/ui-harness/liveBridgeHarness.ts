import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { DesktopClientUpdatedListenerFactory } from "@source-inc/gents-desktop-client";
import { createBridgeHttpAdapter } from "../live-bridge-runner/adapter";
import {
  createVersionPollingListenerFactory,
  VERSION_POLL_MS,
} from "../live-bridge-runner/listener";
import type { VersionResponse } from "../live-bridge-runner/types";

const LIVE_BRIDGE_REQUIRED =
  "Live desktop harness backend requires bridgeUrl when backend=live.";
const DEFAULT_REQUEST_TIMEOUT_MS = 15_000;

export type LiveDesktopUiHarnessOptions = {
  bridgeUrl?: string | null;
};

export type LiveDesktopUiHarness = {
  adapter: DesktopApiAdapter;
  listenerFactory: DesktopClientUpdatedListenerFactory;
  bridgeUrl: string | null;
};

export function createLiveDesktopUiHarness({
  bridgeUrl,
}: LiveDesktopUiHarnessOptions): LiveDesktopUiHarness {
  const baseUrl = normalizeLiveBridgeBaseUrl(bridgeUrl);
  if (!baseUrl) {
    return {
      adapter: unavailableAdapter(LIVE_BRIDGE_REQUIRED),
      listenerFactory: noopListenerFactory,
      bridgeUrl: null,
    };
  }

  const client = new BrowserBridgeHttpClient(baseUrl);
  return {
    adapter: createBridgeHttpAdapter(client),
    listenerFactory: createVersionPollingListenerFactory({
      fetchVersion: async () =>
        (await client.getJson<VersionResponse>("/desktop/version")).version,
      getExitStatus: () => null,
      logError: (message) => console.warn(`[live-bridge-listener] ${message.trim()}`),
      pollMs: VERSION_POLL_MS,
    }),
    bridgeUrl: baseUrl,
  };
}

function normalizeLiveBridgeBaseUrl(raw: string | null | undefined) {
  const trimmed = raw?.trim();
  if (!trimmed) {
    return null;
  }
  const url = new URL(trimmed, window.location.origin);
  url.search = "";
  url.hash = "";
  return url.toString().replace(/\/+$/, "");
}

function unavailableAdapter(message: string): DesktopApiAdapter {
  return new Proxy(
    {},
    {
      get: () => () => Promise.reject(new Error(message)),
    },
  ) as DesktopApiAdapter;
}

const noopListenerFactory: DesktopClientUpdatedListenerFactory = async () => () => {};

class BrowserBridgeHttpClient {
  constructor(
    private readonly baseUrl: string,
    private readonly timeoutMs = DEFAULT_REQUEST_TIMEOUT_MS,
  ) {}

  async getJson<T>(path: string) {
    const response = await this.fetchWithTimeout(this.url(path), {});
    return this.decodeJson<T>(response);
  }

  async postJson<T = unknown>(path: string, body: unknown) {
    const response = await this.fetchWithTimeout(this.url(path), {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    return this.decodeJson<T>(response);
  }

  async fetchWithTimeout(input: string, init: RequestInit) {
    const controller = new AbortController();
    const timeoutId = window.setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      return await fetch(input, {
        ...init,
        signal: init.signal ?? controller.signal,
      });
    } catch (error) {
      if (controller.signal.aborted) {
        throw new Error(`timed out after ${this.timeoutMs}ms waiting for ${input}`);
      }
      throw error;
    } finally {
      window.clearTimeout(timeoutId);
    }
  }

  async decodeJson<T>(response: Response) {
    if (!response.ok) {
      throw new Error(await response.text());
    }
    return (await response.json()) as T;
  }

  private url(path: string) {
    return `${this.baseUrl}${path}`;
  }
}
