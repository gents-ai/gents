import type { CommandDenialView } from "../../lib/commandDenial";
import type { RenderedToolCallView } from "../../lib/types";

/**
 * Inline render for a command-policy denial inside the transcript's
 * .tool-group. Mirrors the panel-286 prototype at
 * docs/ui-prototypes/panel-286-command-denial.html — same class names,
 * same amber treatment, distinct from the red tool-failure path.
 *
 * Mounted by Transcript.tsx::ToolGroups when parseCommandDenial returns
 * non-null on a tool's result text. The component renders the entire
 * <details className="tool-item tool-item-denied"> shell so the
 * surrounding ToolGroups loop just delegates wholesale per item.
 */
export function CommandDenialToolItem({
  tool,
  denial,
}: {
  tool: RenderedToolCallView;
  denial: CommandDenialView;
}) {
  return (
    <details className="tool-item tool-item-denied" data-rule-id={denial.ruleId}>
      <summary className="tool-item-summary">
        <span className="tool-item-summary-left">
          <span aria-hidden="true" className="tool-item-dot tool-item-dot-denied" />
          <span className="denial-summary">
            <span className="denial-summary-line">
              <span className="denial-category-label">{denial.categoryLabel}</span>
              <code className="denial-rule-id">{denial.ruleId}</code>
            </span>
            <span className="denial-summary-line">
              <span className="tool-item-name">{tool.toolName}</span>
            </span>
            <span className="denial-summary-line">
              <span className="denial-reason">{denial.reasonLine}</span>
            </span>
          </span>
        </span>
        <span className="tool-item-action">View</span>
      </summary>
      <div className="tool-item-body">
        <div className="denial-body">
          {denial.deniedCommand ? (
            <div className="denial-detail">
              <span className="tool-detail-key">Denied command</span>
              <div className="denial-attempt">
                <code>{denial.deniedCommand}</code>
                {denial.deniedSubcommand ? (
                  <>
                    {" "}
                    <span className="denied-token">{denial.deniedSubcommand}</span>
                  </>
                ) : null}
                {denial.deniedArgument ? (
                  <>
                    {" "}
                    <span className="denied-token">{denial.deniedArgument}</span>
                  </>
                ) : null}
              </div>
            </div>
          ) : null}

          <div className="denial-detail">
            <span className="tool-detail-key">Diagnostic</span>
            <pre className="tool-block">{denial.diagnostic}</pre>
          </div>
        </div>
      </div>
    </details>
  );
}
