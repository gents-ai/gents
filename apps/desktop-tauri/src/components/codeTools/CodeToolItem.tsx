import type { CodeToolView } from "./codeTools";

// Renders a code tool call as a diff (file edits) or a terminal block (bash),
// instead of the generic args/result disclosure. Foundation-aligned: the data
// is already on the persisted tool call; this is a projection, not new state.
export function CodeToolItem({ view }: { view: CodeToolView }) {
  if (view.kind === "fileEdit") {
    return (
      <details className="tool-item code-tool" data-testid="code-file-edit" open>
        <summary className="tool-item-summary">
          <span className="tool-item-summary-left">
            <span aria-hidden="true" className="tool-item-dot tool-item-dot-success" />
            <span className="code-tool-kind">
              {view.created ? "created" : "edited"}
            </span>
            <span className="code-tool-path mono">{view.path}</span>
            {view.replacementsApplied > 1 ? (
              <span className="code-tool-kind" data-testid="code-replacements">
                ×{view.replacementsApplied}
              </span>
            ) : null}
          </span>
          <span className="tool-item-action">diff</span>
        </summary>
        <pre className="code-diff" data-testid="code-diff">
          {view.diff.map((line, index) => (
            <div
              className={`code-diff-line code-diff-${line.kind}`}
              key={`${line.kind}-${index}`}
            >
              <span aria-hidden="true" className="code-diff-gutter">
                {line.kind === "add" ? "+" : "-"}
              </span>
              <span className="code-diff-text">{line.text}</span>
            </div>
          ))}
        </pre>
      </details>
    );
  }

  return (
    <details className="tool-item code-tool" data-testid="code-command" open>
      <summary className="tool-item-summary">
        <span className="tool-item-summary-left">
          <span
            aria-hidden="true"
            className={`tool-item-dot ${
              view.failed ? "tool-item-dot-error" : "tool-item-dot-success"
            }`}
          />
          <span aria-hidden="true" className="code-tool-prompt mono">
            $
          </span>
          <span className="code-tool-command mono">{view.command}</span>
        </span>
        <span
          className={`code-exit ${view.failed ? "code-exit-fail" : "code-exit-ok"}`}
          data-testid="code-exit"
        >
          {view.timedOut
            ? "timed out"
            : view.exitCode != null
              ? `exit ${view.exitCode}`
              : view.failed
                ? "failed"
                : "ok"}
        </span>
      </summary>
      <div className="tool-item-body">
        {view.executionMode || view.networkMode ? (
          <div className="code-command-meta muted small">
            {view.executionMode ? <span>sandbox: {view.executionMode}</span> : null}
            {view.networkMode ? <span>network: {view.networkMode}</span> : null}
          </div>
        ) : null}
        {view.stdout ? (
          <pre className="code-terminal" data-testid="code-terminal">
            {view.stdout}
          </pre>
        ) : null}
        {view.stderr ? (
          <div className="code-stderr" data-testid="code-stderr">
            <div className="tool-detail-label">stderr</div>
            <pre className="code-terminal">{view.stderr}</pre>
          </div>
        ) : null}
      </div>
    </details>
  );
}
