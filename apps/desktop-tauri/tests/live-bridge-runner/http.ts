const DEFAULT_HTTP_REQUEST_TIMEOUT_MS = 15_000;

export class JsonHttpClient {
  constructor(
    private readonly baseUrl: string,
    private readonly timeoutMs = DEFAULT_HTTP_REQUEST_TIMEOUT_MS,
  ) {}

  async getJson<T>(path: string) {
    const response = await this.fetchWithTimeout(`${this.baseUrl}${path}`, {});
    return this.decodeJson<T>(response);
  }

  async postJson<T = unknown>(path: string, body: unknown) {
    const response = await this.fetchWithTimeout(`${this.baseUrl}${path}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
      },
      body: JSON.stringify(body),
    });
    return this.decodeJson<T>(response);
  }

  async fetchWithTimeout(input: string, init: RequestInit) {
    let timeoutId: ReturnType<typeof setTimeout> | null = null;
    try {
      return await Promise.race([
        fetch(input, init),
        new Promise<Response>((_, reject) => {
          timeoutId = setTimeout(() => {
            reject(
              new Error(`timed out after ${this.timeoutMs}ms waiting for ${input}`),
            );
          }, this.timeoutMs);
        }),
      ]);
    } finally {
      if (timeoutId) {
        clearTimeout(timeoutId);
      }
    }
  }

  async decodeJson<T>(response: Response) {
    if (!response.ok) {
      throw new Error(await response.text());
    }
    return (await response.json()) as T;
  }
}
