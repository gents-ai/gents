import type {
  RenderedTimelineItem,
  RenderedToolCallView,
} from "@source-inc/gents-desktop-client";
import { toolSummary } from "@/screens/tool-summary";

function toolLine(tool: RenderedToolCallView): string {
  const { kind, primary, full, secondary, mono } = toolSummary(tool);
  const what = full ?? primary;
  return [
    "-",
    kind || null,
    mono ? `\`${what.replace(/`/g, "'")}\`` : what,
    secondary ? `· ${secondary}` : null,
  ]
    .filter(Boolean)
    .join(" ");
}

/**
 * A session's transcript as Markdown: what was said, in order, under who
 * said it, and each tool call in one line beneath the reply that made it.
 */
export function sessionMarkdown(
  title: string,
  rows: readonly RenderedTimelineItem[],
): string {
  const blocks = [`# ${title}`];
  let tools: string[] = [];
  const said = (who: string, content: string | null | undefined) => {
    const text = content?.trim();
    if (text) blocks.push(`**${who}**`, text);
  };
  for (const row of rows) {
    if (row.kind !== "toolGroup" && tools.length) {
      blocks.push(tools.join("\n"));
      tools = [];
    }
    switch (row.kind) {
      case "userMessage":
      case "pendingUserTurn":
        said("You", row.content);
        break;
      case "automatedInput":
        said("Input", row.content);
        break;
      case "assistantMessage":
      case "liveAssistant":
        said("Agent", row.content);
        break;
      case "toolGroup":
        tools.push(...row.tools.map(toolLine));
        break;
    }
  }
  if (tools.length) blocks.push(tools.join("\n"));
  return `${blocks.join("\n\n")}\n`;
}
