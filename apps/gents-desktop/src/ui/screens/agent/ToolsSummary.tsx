import { Pencil } from "lucide-react";
import type { Tools } from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../../hooks/fleetStore";
import { Button } from "@gents/ui/components/button";
import { shortPath } from "@/lib/paths";
import { counted } from "../NodeFacts";
import { Summary } from "./AgentCard";
import { usersOf } from "./behaviorDraft";

type Files = "Off" | "ReadOnly" | "ReadWrite";
type Bash = "Off" | "ReadOnly" | "Unrestricted";

const FILES: Record<Files, string> = {
  ReadWrite: "read and edit files",
  ReadOnly: "read files",
  Off: "not touch files",
};

const BASH: Record<Bash, string> = {
  Unrestricted: "run any command",
  ReadOnly: "run read-only commands",
  Off: "run no commands",
};

/** What these tools let an agent do, in a sentence. */
export function toolsSentence(
  tools: Tools,
  draft: { files: Files; bash: Bash; root: string },
) {
  if (draft.files === "Off" && draft.bash === "Off")
    return "Can't touch files or run commands.";
  const root = draft.root.trim();
  const where = root ? `in ${shortPath(root, { max: 40 })}` : "in its workspace";
  const reaches = tools.host?.bash?.network_mode === "enabled";
  const commands = `${BASH[draft.bash]}${reaches ? " and reach the network" : ", with no network"}`;
  if (draft.files === "Off") return `Can ${commands}, but not touch files.`;
  if (draft.bash === "Off")
    return `Can ${FILES[draft.files]} ${where}, but run no commands.`;
  return `Can ${FILES[draft.files]} ${where}, ${commands}.`;
}

/** The tool groups beyond files and commands these tools turn on. */
function alsoOf(tools: Tools): string[] {
  return [
    tools.remote && "remote tools",
    tools.agents && "other agents",
    tools.datastore && "the database",
    tools.integrations && "integrations",
    tools.self_config && "its own configuration",
  ].filter((part): part is string => Boolean(part));
}

export function ToolsSummary({
  deployment,
  tools,
  draft,
  edit,
}: {
  deployment: NodeView;
  tools: Tools;
  /** what the header says, as the form has it */
  draft: { name: string; files: Files; bash: Bash; root: string };
  edit: { open: boolean; onToggle: () => void; controls: string };
}) {
  const contexts = deployment.contexts.filter((c) => c.tools_id === tools.tools_id);
  const agents = contexts.flatMap((c) => usersOf(deployment, c.context_id));
  const also = alsoOf(tools);
  return (
    <div className="@container mb-8 grid gap-4">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="truncate font-heading text-lg font-medium text-heading">
            {draft.name.trim() || tools.tools_id}
          </h2>
          <Button
            size="xs"
            variant="quiet"
            aria-expanded={edit.open}
            aria-controls={edit.controls}
            onClick={edit.onToggle}
          >
            {edit.open ? (
              "Done"
            ) : (
              <>
                <Pencil /> Edit
              </>
            )}
          </Button>
        </div>
        <p className="mt-0.5 text-sm text-foreground/80">
          {toolsSentence(tools, draft)}
        </p>
      </div>
      <div className="grid gap-2 @xl:grid-cols-2">
        <Summary
          label="Used by"
          to={{ name: "agent", nodeDid: deployment.nodeDid, section: "agents" }}
        >
          <span className="truncate">
            {agents.length
              ? `${counted(agents.length, "agent")}${
                  agents.length <= 2
                    ? `: ${agents.map((a) => a.displayName).join(", ")}`
                    : ""
                }`
              : "No agent yet"}
          </span>
        </Summary>
        <Summary label="Also">
          <span className="truncate">
            {also.length
              ? also.join(", ").replace(/^./, (c) => c.toUpperCase())
              : "Nothing else"}
          </span>
        </Summary>
      </div>
    </div>
  );
}
