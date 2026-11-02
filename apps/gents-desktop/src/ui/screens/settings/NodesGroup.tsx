import { useShallow } from "zustand/react/shallow";
import { Button } from "@gents/ui/components/button";
import { useHomeDid } from "@/hooks/useClient";
import { useFleet } from "@/hooks/useFleet";
import { isLocalNode } from "@/lib/firstRun";
import { fleetNodes } from "@/lib/scope";
import { preferences, useHiddenAgents, useHiddenNodes, useOpenOn } from "@/preferences";
import { ChoiceRow } from "../agent/editors";
import { Group, StackedRow } from "../agent/rows";
import { NodeAvatar, nodeAvatarOf } from "../AgentAvatar";

/** The node the app starts on, and what this device leaves out of the
    menus that offer nodes and agents, each with a way to show it again. */
export function NodesGroup() {
  const nodes = useFleet(useShallow(fleetNodes));
  const openOn = useOpenOn();
  const hiddenNodes = useHiddenNodes();
  const hiddenAgents = useHiddenAgents();
  const nodeOf = (nodeDid: string) => nodes.find((x) => x.nodeDid === nodeDid);
  const nameOf = (nodeDid: string) => {
    const n = nodeOf(nodeDid);
    /* a node no longer listed has only its DID's tail, as the nav shows it */
    return n ? (n.node.displayName ?? n.label) : `Node …${nodeDid.slice(-8)}`;
  };
  const homeDid = useHomeDid();
  const choice = (n: (typeof nodes)[number]) => ({
    value: n.nodeDid,
    label: nameOf(n.nodeDid),
  });
  /* as the node switcher has them: this machine's first, then the remote ones */
  const groups = [
    { label: "Local", items: nodes.filter((n) => isLocalNode(n, homeDid)).map(choice) },
    {
      label: "Remote",
      items: nodes.filter((n) => !isLocalNode(n, homeDid)).map(choice),
    },
  ];
  /* a node chosen before it went away stays named, so the choice is visible */
  const gone =
    openOn && !nodes.some((n) => n.nodeDid === openOn)
      ? [{ value: openOn, label: `${nameOf(openOn)} (not listed)` }]
      : [];
  const agents = hiddenAgents.map((key) => {
    const at = key.indexOf(" ");
    const nodeDid = key.slice(0, at);
    const agentId = key.slice(at + 1);
    const agent = nodes
      .find((n) => n.nodeDid === nodeDid)
      ?.agents.find((a) => a.agentId === agentId);
    return { key, nodeDid, agentId, name: agent?.displayName ?? agentId };
  });
  /* agents draw their node's portrait, since an agent has none of its own */
  const items = [
    ...hiddenNodes.map((nodeDid) => ({
      key: nodeDid,
      nodeDid,
      node: nodeOf(nodeDid),
      name: nameOf(nodeDid),
      kind: "Node",
      show: () => preferences.setNodeHidden(nodeDid, false),
    })),
    ...agents.map((a) => ({
      key: a.key,
      nodeDid: a.nodeDid,
      node: nodeOf(a.nodeDid),
      name: a.name,
      kind: `Agent on ${nameOf(a.nodeDid)}`,
      show: () => preferences.setAgentHidden(a.nodeDid, a.agentId, false),
    })),
  ];
  return (
    <Group title="Nodes">
      <ChoiceRow
        id="settings-open-on"
        label="Startup node"
        description="Which node the app shows when it starts or opens a new window. Last used picks up where you left off."
        value={openOn ?? ""}
        none="Last used"
        onChange={(v) => preferences.setOpenOn(v || null)}
        groups={groups}
        items={gone}
      />
      <StackedRow
        label="Hidden"
        description={
          items.length === 0
            ? "Nothing is hidden. Hide a node from its menu on the Nodes page, or an agent from its menu in its node's Agents list."
            : "Left out of the node switcher, the sidebar and search on this device."
        }
      >
        {items.length > 0 && (
          <ul
            aria-label="Hidden"
            className="divide-y divide-border/60 rounded-xl border border-border/60"
          >
            {items.map((item) => (
              <li
                key={item.key}
                className="flex min-w-0 items-center gap-2.5 px-3 py-2"
              >
                <NodeAvatar
                  name={nameOf(item.nodeDid)}
                  {...(item.node ? nodeAvatarOf(item.node) : {})}
                  className="size-5"
                />
                <span className="min-w-0 truncate text-sm">{item.name}</span>
                <span className="min-w-0 truncate text-xs text-muted-foreground">
                  {item.kind}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  className="ml-auto shrink-0"
                  aria-label={`Show ${item.name}`}
                  onClick={item.show}
                >
                  Show
                </Button>
              </li>
            ))}
          </ul>
        )}
      </StackedRow>
    </Group>
  );
}
