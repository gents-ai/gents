import { useShallow } from "zustand/react/shallow";
import { Button } from "@gents/ui/components/button";
import { useFleet } from "@/hooks/useFleet";
import { fleetNodes } from "@/lib/scope";
import { preferences, useHiddenAgents, useHiddenNodes, useOpenOn } from "@/preferences";
import { ChoiceRow } from "../agent/editors";
import { Group, Row } from "../agent/rows";

/** Which node the app opens on, and what this device leaves out of the
    menus that offer nodes and agents, each with a way to show it again. */
export function NodesGroup() {
  const nodes = useFleet(useShallow(fleetNodes));
  const openOn = useOpenOn();
  const hiddenNodes = useHiddenNodes();
  const hiddenAgents = useHiddenAgents();
  const nameOf = (nodeDid: string) => {
    const n = nodes.find((x) => x.nodeDid === nodeDid);
    /* a node no longer listed has only its DID's tail, as the nav shows it */
    return n ? (n.node.displayName ?? n.label) : `Node …${nodeDid.slice(-8)}`;
  };
  const choices = nodes.map((n) => ({ value: n.nodeDid, label: nameOf(n.nodeDid) }));
  /* a node chosen before it went away stays named, so the choice is visible */
  if (openOn && !nodes.some((n) => n.nodeDid === openOn))
    choices.push({ value: openOn, label: `${nameOf(openOn)} (not listed)` });
  const agents = hiddenAgents.map((key) => {
    const at = key.indexOf(" ");
    const nodeDid = key.slice(0, at);
    const agentId = key.slice(at + 1);
    const agent = nodes
      .find((n) => n.nodeDid === nodeDid)
      ?.agents.find((a) => a.agentId === agentId);
    return { key, nodeDid, agentId, name: agent?.displayName ?? agentId };
  });
  return (
    <Group title="Nodes">
      <ChoiceRow
        id="settings-open-on"
        label="Open on"
        description="The node the app opens on. Last used opens on the node you were on."
        value={openOn ?? ""}
        none="Last used"
        onChange={(v) => preferences.setOpenOn(v || null)}
        items={choices}
      />
      {hiddenNodes.length + agents.length === 0 ? (
        <Row
          label="Hidden"
          description="Nothing is hidden. Hide a node from its menu on the Nodes page, or an agent from its menu in its node's Agents list."
        />
      ) : (
        <>
          {hiddenNodes.map((nodeDid) => (
            <Row
              key={nodeDid}
              label={nameOf(nodeDid)}
              description="Node, hidden from the node switcher, the nav and search"
            >
              <Button
                variant="outline"
                size="sm"
                aria-label={`Show ${nameOf(nodeDid)}`}
                onClick={() => preferences.setNodeHidden(nodeDid, false)}
              >
                Show
              </Button>
            </Row>
          ))}
          {agents.map((a) => (
            <Row
              key={a.key}
              label={a.name}
              description={`Agent on ${nameOf(a.nodeDid)}, hidden from the agent picker and search`}
            >
              <Button
                variant="outline"
                size="sm"
                aria-label={`Show ${a.name}`}
                onClick={() => preferences.setAgentHidden(a.nodeDid, a.agentId, false)}
              >
                Show
              </Button>
            </Row>
          ))}
        </>
      )}
    </Group>
  );
}
