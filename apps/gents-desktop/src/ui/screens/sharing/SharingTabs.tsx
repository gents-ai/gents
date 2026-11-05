import type { ReactNode } from "react";
import { useStore } from "zustand";
import { createStore } from "zustand/vanilla";
import { FieldRows } from "@gents/ui/components/field";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@gents/ui/components/tabs";
import type {
  Invite,
  SharingAvailability,
  SharingDecision,
  SharingMember,
} from "../../../hooks/sharing";
import type { ApprovedHere } from "@/app/approvedHere";
import { EmptyList } from "../agent/ListDetail";
import { DecisionRow } from "./History";
import { InviteRow } from "./Invites";
import { ApprovedHereRow, MemberRow } from "./Members";
import { scopeText } from "./parts";

export type SharingTab = "access" | "invites" | "history";

/* the tab chosen, kept while the app runs */
const chosen = createStore<{ tab: SharingTab }>(() => ({ tab: "access" }));
export const chooseSharingTab = (tab: SharingTab) => chosen.setState({ tab });
export const resetSharingTab = () => chosen.setState({ tab: "access" });

/** The node's sharing one view at a time: who has access, the open invites,
    and what was decided. A view the runtime doesn't offer is left out; the
    line under the tabs names it. Access stays while this device remembers
    approvals the runtime can't list. */
export function SharingTabs({
  nodeDid,
  name,
  available,
  members,
  approved,
  invites,
  history,
  note,
}: {
  nodeDid: string;
  name: string;
  available: SharingAvailability;
  members: SharingMember[];
  approved: ApprovedHere[];
  invites: Invite[];
  history: SharingDecision[];
  /** what the runtime doesn't offer, under the tabs */
  note: ReactNode;
}) {
  const stored = useStore(chosen, (s) => s.tab);
  const tabs: { id: SharingTab; label: string; count?: number }[] = [
    ...(available.members || approved.length > 0
      ? [
          {
            id: "access" as const,
            label: "Access",
            count: members.length + approved.length,
          },
        ]
      : []),
    ...(available.invites
      ? [{ id: "invites" as const, label: "Invites", count: invites.length }]
      : []),
    ...(available.history ? [{ id: "history" as const, label: "History" }] : []),
  ];
  if (tabs.length === 0) return <>{note}</>;
  const tab = tabs.some((t) => t.id === stored) ? stored : tabs[0]!.id;
  return (
    <Tabs
      value={tab}
      onValueChange={(v) => chooseSharingTab(v as SharingTab)}
      className="gap-4"
    >
      <TabsList variant="line" aria-label="Show" className="min-h-9">
        {tabs.map((t) => (
          <TabsTrigger key={t.id} value={t.id} className="text-[13px]">
            {t.label}
            {t.count !== undefined && t.count > 0 && (
              <span className="ml-1 text-muted-foreground tabular-nums">{t.count}</span>
            )}
          </TabsTrigger>
        ))}
      </TabsList>
      {note}
      <TabsContent value="access">
        <List
          label="Who has access"
          empty={`Nobody else has access to ${name}.`}
          count={members.length + approved.length}
        >
          {members.map((member) => (
            <MemberRow
              key={`${member.party.did} ${scopeText(member.scope)}`}
              nodeDid={nodeDid}
              member={member}
            />
          ))}
          {approved.map((entry) => (
            <ApprovedHereRow key={entry.requestId} nodeDid={nodeDid} entry={entry} />
          ))}
        </List>
      </TabsContent>
      <TabsContent value="invites">
        <List label="Open invites" empty="No open invites." count={invites.length}>
          {invites.map((invite) => (
            <InviteRow key={invite.inviteId} nodeDid={nodeDid} invite={invite} />
          ))}
        </List>
      </TabsContent>
      <TabsContent value="history">
        <List
          label="Sharing history"
          empty="Nothing decided yet."
          count={history.length}
        >
          {history.map((decision, i) => (
            <DecisionRow key={`${decision.at} ${i}`} decision={decision} />
          ))}
        </List>
      </TabsContent>
    </Tabs>
  );
}

function List({
  label,
  empty,
  count,
  children,
}: {
  label: string;
  empty: string;
  count: number;
  children: ReactNode;
}) {
  if (count === 0) return <EmptyList>{empty}</EmptyList>;
  return (
    <FieldRows role="list" aria-label={label}>
      {children}
    </FieldRows>
  );
}

const PARTS: { key: keyof SharingAvailability; name: string; one: boolean }[] = [
  { key: "requests", name: "Requests to join", one: false },
  { key: "members", name: "Access", one: true },
  { key: "invites", name: "Invites", one: false },
  { key: "history", name: "History", one: true },
];

/** what the node's runtime doesn't offer yet, in one sentence, or null when
    it offers everything: "Invites and history need a newer runtime on
    Forge." */
export function missingText(available: SharingAvailability, name: string) {
  const missing = PARTS.filter((p) => !available[p.key]);
  if (missing.length === 0) return null;
  const names = missing.map((p, i) => (i === 0 ? p.name : p.name.toLowerCase()));
  const list =
    names.length === 1
      ? names[0]
      : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
  const verb = missing.length === 1 && missing[0]!.one ? "needs" : "need";
  return `${list} ${verb} a newer runtime on ${name}.`;
}
