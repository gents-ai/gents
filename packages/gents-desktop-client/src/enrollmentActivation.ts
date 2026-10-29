import type { EnrollmentRequestView } from "./generated/EnrollmentRequestView.js";

export function enrollmentActivationMessage(
  request: Pick<EnrollmentRequestView, "state" | "activationError">,
): string | null {
  if (request.state !== "approved") return null;
  switch (request.activationError) {
    case "authentication":
      return "Could not authenticate the server. Check its address and network connection. Gents will retry.";
    case "route":
      return "Could not finish the secure connection. Check that the server is available. Gents will retry.";
    default:
      return null;
  }
}
