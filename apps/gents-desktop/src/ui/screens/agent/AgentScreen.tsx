import { href, navigate } from "@/lib/router";
import { useNodeConfigReadOnly } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf } from "../../../hooks/fleetStore";
import { SectionedScreen } from "../SectionedScreen";
import { isListed, useAgentSections } from "./sections";

export function AgentScreen({
  nodeDid,
  section,
  item,
}: {
  nodeDid: string;
  section: string;
  item?: string;
}) {
  const sections = useAgentSections();
  const deployment = useFleet((s) => nodeOf(s, nodeDid));
  if (!deployment) {
    return (
      <p className="p-8 text-sm text-muted-foreground">
        No node with that DID on this desktop.
      </p>
    );
  }
  return (
    <NodeSettings
      deployment={deployment}
      sections={sections}
      section={section}
      item={item}
    />
  );
}

function NodeSettings({
  deployment,
  sections,
  section,
  item,
}: {
  deployment: NonNullable<ReturnType<typeof nodeOf>>;
  sections: ReturnType<typeof useAgentSections>;
  section: string;
  item?: string;
}) {
  const nodeDid = deployment.nodeDid;
  const readOnly = useNodeConfigReadOnly(deployment);
  const current = sections.find((s) => s.id === section) ?? null;
  const entry = current && !isListed(current) ? current.under : section;
  const Panel = current?.Panel;
  return (
    <SectionedScreen
      entries={sections.filter(isListed).map((s) => ({
        id: s.id,
        group: s.group,
        label: s.label,
        icon: s.icon,
        count: s.count?.(deployment),
      }))}
      current={entry}
      hrefFor={(id) => href({ name: "agent", nodeDid, section: id })}
      onSelect={(id) => navigate({ name: "agent", nodeDid, section: id })}
      scrollKey={`${nodeDid}/${section}/${item ?? ""}`}
      pageKey={deployment.nodeDid}
      testId="agent-screen"
    >
      {readOnly && (
        <p
          role="status"
          className="mx-6 mt-4 rounded-lg border border-border/60 bg-raised px-3 py-2 text-xs text-muted-foreground"
        >
          {readOnly}
        </p>
      )}
      <fieldset disabled={Boolean(readOnly)} className="m-0 min-w-0 border-0 p-0">
        {Panel && <Panel deployment={deployment} item={item} />}
      </fieldset>
    </SectionedScreen>
  );
}
