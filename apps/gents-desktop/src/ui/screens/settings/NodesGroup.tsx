import { History } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { Button } from "@gents/ui/components/button";
import {
  Combobox,
  ComboboxCollection,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxGroup,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxTrigger,
} from "@gents/ui/components/combobox";
import { cn } from "@gents/ui/lib/utils";
import { useHomeDid } from "@/hooks/useClient";
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { useFleet } from "@/hooks/useFleet";
import type { NodeView } from "../../../hooks/fleetStore";
import { isLocalNode } from "@/lib/firstRun";
import { fleetNodes } from "@/lib/scope";
import { SEARCH_FIELD } from "@/lib/textAssist";
import { preferences, useHiddenAgents, useHiddenNodes, useOpenOn } from "@/preferences";
import { Group, Row, StackedRow } from "../agent/rows";
import { NodeAvatar, nodeAvatarOf } from "../AgentAvatar";

/* the choice that names no node: listed above the groups, and kept by any
   search, so the setting can always be cleared */
const LAST_USED = "";

/** The startup node, chosen from a searchable list: Last used on top, then
    this machine's node and the remote ones under their headings, each with
    its portrait and whether it answered, as the node switcher draws them. */
function StartupNodeRow({
  nodes,
  value,
  onChange,
  nameOf,
}: {
  nodes: readonly NodeView[];
  value: string | null;
  onChange: (nodeDid: string | null) => void;
  nameOf: (nodeDid: string) => string;
}) {
  const homeDid = useHomeDid();
  const popover = useExclusivePopover();
  const nodeOf = (nodeDid: string) => nodes.find((n) => n.nodeDid === nodeDid);
  /* a node chosen before it went away stays named, so the choice is visible */
  const gone = value && !nodeOf(value) ? [value] : [];
  const groups = [
    { value: "pinned", label: "", items: [LAST_USED] },
    {
      value: "local",
      label: "Local",
      items: nodes.filter((n) => isLocalNode(n, homeDid)).map((n) => n.nodeDid),
    },
    {
      value: "remote",
      label: "Remote",
      items: nodes.filter((n) => !isLocalNode(n, homeDid)).map((n) => n.nodeDid),
    },
    { value: "gone", label: "", items: gone },
  ].filter((g) => g.items.length > 0);
  const labelOf = (v: string) =>
    v === LAST_USED ? "Last used" : nodeOf(v) ? nameOf(v) : `${nameOf(v)} (not listed)`;
  const mark = (v: string) => {
    if (v === LAST_USED)
      return (
        <span className="grid size-6 shrink-0 place-items-center text-muted-foreground">
          <History className="size-4" />
        </span>
      );
    const node = nodeOf(v);
    return (
      <span className="relative shrink-0">
        <NodeAvatar
          name={nameOf(v)}
          {...(node ? nodeAvatarOf(node) : {})}
          className="size-6"
        />
        {node && (
          <span
            aria-hidden
            className={cn(
              "absolute -right-0.5 -bottom-0.5 size-2 rounded-full border-2 border-popover",
              node.dialSucceeded ? "bg-brand" : "bg-border",
            )}
          />
        )}
      </span>
    );
  };
  return (
    <Row
      label="Startup node"
      description="Which node the app shows when it starts or opens a new window. Last used picks up where you left off."
      htmlFor="settings-open-on"
    >
      <Combobox
        items={groups}
        value={value ?? LAST_USED}
        onValueChange={(v) => {
          popover.onOpenChange(false);
          onChange(v ? String(v) : null);
        }}
        open={popover.open}
        onOpenChange={popover.onOpenChange}
        onOpenChangeComplete={popover.onOpenChangeComplete}
        itemToStringLabel={(v: string) => labelOf(v)}
        filter={(v: string, query: string) => {
          const q = query.trim().toLowerCase();
          return !q || v === LAST_USED || labelOf(v).toLowerCase().includes(q);
        }}
      >
        <ComboboxTrigger
          id="settings-open-on"
          className="flex h-9 w-72 min-w-0 items-center gap-2 rounded-2xl border border-transparent bg-input/50 px-3 text-sm whitespace-nowrap outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 max-md:w-full"
        >
          {value ? (
            <NodeAvatar
              name={nameOf(value)}
              {...(nodeOf(value) ? nodeAvatarOf(nodeOf(value)!) : {})}
              className="size-5"
            />
          ) : null}
          <span className="min-w-0 truncate">{labelOf(value ?? LAST_USED)}</span>
        </ComboboxTrigger>
        <ComboboxContent
          ref={popover.popupRef}
          aria-label="Choose the startup node"
          className="w-[22rem] min-w-[22rem] max-md:w-[calc(100vw-2rem)] max-md:min-w-0"
        >
          <ComboboxInput
            showTrigger={false}
            aria-label="Search nodes"
            {...SEARCH_FIELD}
            placeholder={`Search ${nodes.length} nodes`}
          />
          <ComboboxEmpty>No node by that name.</ComboboxEmpty>
          <ComboboxList>
            {(group: (typeof groups)[number]) => (
              <ComboboxGroup
                key={group.value}
                items={group.items}
                aria-label={group.label || undefined}
              >
                {/* not the group's ComboboxLabel, which Base UI hides from
                    assistive technology; the group is named by aria-label */}
                {group.label && (
                  <div
                    aria-hidden
                    className="px-2 pt-2 pb-1 text-xs text-muted-foreground"
                  >
                    {group.label}
                  </div>
                )}
                <ComboboxCollection>
                  {(v: string) => (
                    <ComboboxItem key={v || "last-used"} value={v}>
                      <span className="flex min-w-0 flex-1 items-center gap-2.5">
                        {mark(v)}
                        <span className="min-w-0 truncate">{labelOf(v)}</span>
                      </span>
                    </ComboboxItem>
                  )}
                </ComboboxCollection>
              </ComboboxGroup>
            )}
          </ComboboxList>
        </ComboboxContent>
      </Combobox>
    </Row>
  );
}

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
      <StartupNodeRow
        nodes={nodes}
        value={openOn}
        onChange={(nodeDid) => preferences.setOpenOn(nodeDid)}
        nameOf={nameOf}
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
