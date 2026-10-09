/** The bridge's error code on a failure, when it carries one. */
export function bridgeErrorCode(cause: unknown): string | null {
  if (!cause || typeof cause !== "object") return null;
  const code = (cause as { code?: unknown }).code;
  return typeof code === "string" ? code : null;
}

const INTERNAL_DETAIL = /https?:\/\/|graphql|\/api\/v\d/i;

/**
 * User-facing text for a setup failure. Bridge setup commands already return
 * user-facing messages; anything that still carries an endpoint URL or an
 * internal GraphQL error is replaced, and the detail goes to the console log.
 */
export function setupErrorMessage(
  cause: unknown,
  fallback = "Something went wrong. Try again.",
): string {
  const message = cause instanceof Error ? cause.message : String(cause ?? "");
  if (!message.trim()) return fallback;
  if (INTERNAL_DETAIL.test(message)) {
    console.warn("setup error detail", cause);
    return bridgeErrorCode(cause) === "endpointUnreachable"
      ? "The node is not reachable. Make sure it is running, then try again."
      : fallback;
  }
  return message;
}
