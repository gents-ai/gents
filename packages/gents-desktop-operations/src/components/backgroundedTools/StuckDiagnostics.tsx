import type { StuckWorkDiagnosticView } from "@source-inc/gents-desktop-client";
import { shortId } from "../../shortId.js";

export type StuckDiagnosticsProps = {
  diagnostics: StuckWorkDiagnosticView[];
  onResendRequest?: (requestId: string) => void;
};

export function StuckDiagnostics({
  diagnostics,
  onResendRequest,
}: StuckDiagnosticsProps) {
  if (diagnostics.length === 0) return null;

  return (
    <div
      className="stuck-diagnostics"
      data-testid="stuck-diagnostics"
      role="alert"
    >
      {diagnostics.map((diagnostic, index) => (
        <div
          className={`stuck-diagnostic is-${diagnostic.severity}`}
          key={`${diagnostic.requestId}-${diagnostic.toolCallId ?? index}`}
        >
          <span aria-hidden="true" className="stuck-diagnostic-dot" />
          {diagnosticSentence(diagnostic)}
          {onResendRequest && diagnostic.requestId ? (
            <button
              className="ghost-button stuck-resend"
              data-testid={`stuck-resend-${diagnostic.requestId}`}
              onClick={() => onResendRequest(diagnostic.requestId)}
              title="Resubmit this request as a fresh one"
              type="button"
            >
              Resend
            </button>
          ) : null}
        </div>
      ))}
    </div>
  );
}

/** The bridge's diagnosis, in operator language. */
function diagnosticSentence(diagnostic: StuckWorkDiagnosticView): string {
  const tool = diagnostic.toolName ?? "a tool";
  switch (diagnostic.reason) {
    case "expiredProcessing":
      return `Request ${shortId(diagnostic.requestId)} ran past its deadline`;
    case "expiredTool":
      return `${tool} on ${shortId(diagnostic.requestId)} ran past its deadline`;
    case "stuckTool":
      return `${tool} on ${shortId(diagnostic.requestId)} has stopped making progress`;
    case "pendingRemoteCancelAck":
      return `Waiting on a remote node to acknowledge cancelling ${shortId(diagnostic.requestId)}`;
    default:
      return `${shortId(diagnostic.requestId)} needs attention`;
  }
}
