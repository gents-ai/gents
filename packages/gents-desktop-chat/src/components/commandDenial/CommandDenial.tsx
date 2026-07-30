import type { CommandDenialView } from "@source-inc/gents-desktop-client";
import type { RenderedToolCallView } from "@source-inc/gents-desktop-client";

export function CommandDenialToolItem({
  tool,
  denial,
}: {
  tool: RenderedToolCallView;
  denial: CommandDenialView;
}) {
  return (
    <details
      className="tool-item tool-item-denied"
      data-rule-id={denial.ruleId}
    >
      <summary className="tool-item-summary">
        <span className="tool-item-summary-left">
          <span
            aria-hidden="true"
            className="tool-item-dot tool-item-dot-denied"
          />
          <span className="denial-summary">
            <span className="denial-summary-line">
              <span className="denial-category-label">
                {denial.categoryLabel}
              </span>
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
                    <span className="denied-token">
                      {denial.deniedSubcommand}
                    </span>
                  </>
                ) : null}
                {denial.deniedArgument ? (
                  <>
                    {" "}
                    <span className="denied-token">
                      {denial.deniedArgument}
                    </span>
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
