/* One agent's editor, with its context edited inline. The data model
   is unchanged (Agent and AgentContext stay two documents), but the
   context is a visible block on the agent: pick one, see who else uses
   it, duplicate it or start empty, and edit its instructions and
   capabilities in place. Everything waits for one Save. */
import type { NodeView } from "../../../hooks/fleetStore";
import { dependentsWarning } from "./dependents";
import { Fragment, useRef, useState } from "react";
import { ArrowLeftRight, ArrowUpRight, Copy, MoreHorizontal, Plus } from "lucide-react";
import type { AgentContext, AgentView } from "@source-inc/gents-desktop-client";
import {
  AlertDialog,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import { href } from "@/lib/router";
import { agentReadiness } from "@/lib/agent-readiness";
import { AccessSentence, AgentInitials } from "../parts";
import {
  AreaRow,
  ChipsRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  RefRow,
  TagsRow,
  TextRow,
} from "./editors";
import { ProfileSheet } from "./ProfileSheet";
import { profileLabel, toolsInWords } from "./automation";
import { ToolsSheet } from "./ToolsSheet";
import { NewSkillSheet } from "./NewSkillSheet";
import { EditorSheet } from "./EditorSheet";
import { ToolsEditor } from "./ToolsPanel";
import { ProfileEditor, modelSentence } from "./ProfileEditor";
import { focusFirstProblem, newId, useDraft } from "./draft";
import { ConfirmDelete, DeleteButton } from "./ListDetail";
import { Group } from "./rows";
import { rememberContextOrigin } from "./contextOrigin";
import { useApp } from "@/app/AppContext";
import {
  contextFields,
  DUPLICATE,
  EMPTY,
  FIELD_ORDER,
  freshName,
  isNew,
  listNames,
  nameOf,
  problems,
  usersOf,
  type Draft,
  type DraftMode,
  type SaveIntent,
} from "./behaviorDraft";
import { ContextPicker } from "./ContextPicker";
import { contextOutcome, writeAgentDraft } from "./behaviorSave";

export function AgentEditor({
  deployment,
  agent,
  draft: draftMode,
  embedded = false,
}: {
  deployment: NodeView;
  agent: AgentView;
  /* a new agent that exists only on this page until Save */
  draft?: DraftMode;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
    section: "agents",
  };
  const context: AgentContext | null =
    deployment.contexts.find((c) => c.context_id === agent.contextId) ?? null;
  const saved: Draft = {
    displayName: agent.displayName,
    description: agent.description ?? "",
    tags: agent.tags ?? [],
    /* a draft starts its own empty instructions; Save creates them */
    contextChoice: draftMode ? EMPTY : (agent.contextId ?? ""),
    contextName: "",
    copiedFrom: "",
    ...contextFields(context),
    inferenceProfileId: agent.inferenceProfileId ?? "",
  };
  /* an unused context offered for deletion after a save: the typed-name
     confirmation every delete goes through */
  const [confirmUnused, setConfirmUnused] = useState<AgentContext | null>(null);
  /* the inline create dialogs' resolvers while one is open */
  const [newProfile, setNewProfile] = useState<((id: string | null) => void) | null>(
    null,
  );
  const [newTools, setNewTools] = useState<((id: string | null) => void) | null>(null);
  const [newSkill, setNewSkill] = useState<((id: string | null) => void) | null>(null);
  /* a just-created document whose full editor is open beside the page */
  const [configure, setConfigure] = useState<{
    kind: "tools" | "profile";
    id: string;
  } | null>(null);
  const pendingContextId = useRef<string | null>(null);
  const d = useDraft(
    saved,
    async (next, intent) => {
      const want = (intent ?? {}) as SaveIntent;
      const written = await writeAgentDraft({
        changeConfig,
        deployment,
        agent,
        next,
        asCopy: Boolean(want.copy),
        draftMode,
        /* one id per new context for the life of this editor, so a retry
           after a failure writes the same document instead of minting
           another */
        newContextId: () => (pendingContextId.current ??= newId("ctx")),
      });
      if (written.creating) pendingContextId.current = null;
      const { said, unused } = contextOutcome(deployment, agent, context, written);
      if (!said.length) return undefined;
      return {
        savedToast: `Saved. ${said.join(" ")}`,
        savedAction: unused
          ? { label: "Delete it", onClick: () => setConfirmUnused(unused) }
          : undefined,
      };
    },
    {
      isNew: draftMode !== undefined,
      problems: (next) => problems(deployment, next, draftMode !== undefined),
    },
  );
  const id = (f: string) => `${agent.agentId}-${f}`;
  const errors = d.problems;
  /* a draft closes only once the bridge has the agent */
  const finish = async (intent: SaveIntent) => {
    const ok = await d.save(intent);
    if (ok && draftMode) draftMode.onSaved(agent.agentId);
  };
  /* problems stay at their fields; Save takes you to the first one */
  const fields = Object.fromEntries(
    FIELD_ORDER.map(([field, slug]) => [field, id(slug)]),
  );
  const save = (intent: SaveIntent = {}): void | Promise<void> => {
    if (focusFirstProblem(errors, fields)) return;
    if (sharedEdit) {
      setPendingIntent(intent);
      setConfirmShared(true);
      return;
    }
    return finish(intent);
  };

  /* the context the draft points at, or the one a new context copies */
  const creating = isNew(d.draft.contextChoice);
  const selected = creating
    ? null
    : (deployment.contexts.find((c) => c.context_id === d.draft.contextChoice) ?? null);
  const copiedFrom =
    deployment.contexts.find((c) => c.context_id === d.draft.copiedFrom) ?? null;
  const others = selected
    ? usersOf(deployment, selected.context_id).filter(
        (b) => b.agentId !== agent.agentId,
      )
    : [];
  /* a change to shared instructions asks, at Save, whose change it is */
  const [confirmShared, setConfirmShared] = useState(false);
  /* what the save waiting on that question should also do */
  const [pendingIntent, setPendingIntent] = useState<SaveIntent>({});
  /* switching to another agent's instructions opens the picker inline */
  const [switching, setSwitching] = useState(false);
  const shared = others.length > 0;
  /* a changed field on instructions other agents also use */
  const sharedEdit =
    selected !== null &&
    shared &&
    JSON.stringify(contextFields(selected)) !==
      JSON.stringify({
        systemPrompt: d.draft.systemPrompt,
        toolsId: d.draft.toolsId,
        compactionId: d.draft.compactionId,
        skillIds: d.draft.skillIds,
      });
  const pick = (choice: string) => {
    if (choice === d.draft.contextChoice) return;
    const name = freshName(
      deployment,
      `${d.draft.displayName.trim() || "New"} context`,
    );
    if (choice === DUPLICATE) {
      /* the copy starts from what the fields hold now */
      d.set("contextChoice", DUPLICATE);
      d.set("copiedFrom", selected?.context_id ?? "");
      d.set("contextName", name);
      return;
    }
    const fields =
      choice === EMPTY
        ? {
            systemPrompt: "",
            toolsId: deployment.tools[0]?.tools_id ?? "",
            compactionId: deployment.compactions[0]?.compaction_id ?? "",
            skillIds: [],
          }
        : contextFields(
            deployment.contexts.find((c) => c.context_id === choice) ?? null,
          );
    d.set("contextChoice", choice);
    d.set("copiedFrom", "");
    d.set("contextName", choice === EMPTY ? name : "");
    d.set("systemPrompt", fields.systemPrompt);
    d.set("toolsId", fields.toolsId);
    d.set("compactionId", fields.compactionId);
    d.set("skillIds", fields.skillIds);
  };
  const newLabel = d.draft.contextName.trim() || "New context";
  /* who points at each context, counted once per render */
  const usersByContext = new Map<string, AgentView[]>();
  for (const b of deployment.agents) {
    if (!b.contextId) continue;
    const list = usersByContext.get(b.contextId);
    if (list) list.push(b);
    else usersByContext.set(b.contextId, [b]);
  }
  const labelFor = (value: string) => {
    if (value === DUPLICATE) return `${newLabel} (new copy)`;
    if (value === EMPTY) return `${newLabel} (new)`;
    const c = deployment.contexts.find((x) => x.context_id === value);
    return c ? nameOf(c) : "None";
  };
  const hint = (c: AgentContext) => {
    const users = usersByContext.get(c.context_id) ?? [];
    return users.length ? listNames(users.map((b) => b.displayName)) : "unused";
  };
  const contextLink = selected
    ? href({ ...base, section: "contexts", item: selected.context_id })
    : null;
  /* so the context page's Back returns here */
  const rememberOrigin = () => {
    if (selected) rememberContextOrigin(selected.context_id, agent.agentId);
  };
  const toolsSharers = d.draft.toolsId
    ? deployment.contexts.filter(
        (c) => c.tools_id === d.draft.toolsId && c.context_id !== selected?.context_id,
      )
    : [];
  const toolsNote = !d.draft.toolsId
    ? "No tools: the agent can only talk."
    : toolsSharers.length
      ? `Also used by ${toolsSharers.length} other ${toolsSharers.length === 1 ? "context" : "contexts"}; edits to the document reach them too.`
      : "Only this agent uses these tools.";
  /* the block only speaks up when there is something to act on: shared
     instructions, or ones that Save will create */
  const sharedWith = listNames(others.map((b) => b.displayName));
  const ownership = creating
    ? copiedFrom
      ? `A copy of ${nameOf(copiedFrom)} for this agent, created when you save.`
      : "New empty instructions for this agent, created when you save."
    : null;
  /* the agents that share these instructions, each a link; past two,
     the rest are counted and the count opens the context, which lists them */
  const agentLink = (b: AgentView) => (
    <a
      href={href({ ...base, item: b.agentId })}
      className="text-foreground underline-offset-2 hover:underline"
    >
      {b.displayName}
    </a>
  );
  const sharedNote =
    shared && !creating ? (
      <>
        Shared with{" "}
        {others.length <= 2 ? (
          others.map((b, i) => (
            <Fragment key={b.agentId}>
              {i > 0 && " and "}
              {agentLink(b)}
            </Fragment>
          ))
        ) : (
          <>
            {agentLink(others[0]!)} and{" "}
            <a
              href={contextLink ?? undefined}
              onClick={rememberOrigin}
              className="text-foreground underline-offset-2 hover:underline"
            >
              {others.length - 1} more
            </a>
          </>
        )}
      </>
    ) : null;
  /* its context id points at nothing: deleted from under it */
  const dangling = Boolean(d.draft.contextChoice) && !creating && !selected;
  /* a context only this agent uses can go with it */
  const soleContext =
    context &&
    (usersByContext.get(context.context_id) ?? []).every(
      (b) => b.agentId === agent.agentId,
    )
      ? context
      : null;
  const readiness = agentReadiness(deployment, agent.agentId);
  const env = deployment.agentEnvironments.find((e) => e.agentId === agent.agentId);
  /* said under the summary only when something needs attention */
  const attention =
    agent.enabled && !readiness.ready ? `It can’t run: ${readiness.reason}.` : null;
  return (
    <>
      <header
        data-testid="agent-header"
        className="mb-8 rounded-3xl bg-raised px-5 py-4 shadow-sm ring-1 ring-foreground/5"
      >
        {/* avatar top-aligned beside the text; above it on phones */}
        <div className="flex min-w-0 items-start gap-3 max-md:flex-col">
          <AgentInitials
            name={draftMode ? d.draft.displayName.trim() || "New" : agent.displayName}
            className="size-10 shrink-0 text-sm"
          />
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="truncate font-heading text-lg font-medium text-heading">
                {draftMode
                  ? d.draft.displayName.trim() || "New agent"
                  : agent.displayName}
              </h2>
              {agent.isDefault && <Badge variant="secondary">Default</Badge>}
              {draftMode && <Badge variant="secondary">Draft</Badge>}
            </div>
            {draftMode ? null : (
              <>
                <p
                  data-testid="agent-summary"
                  className="mt-0.5 text-sm text-muted-foreground"
                >
                  <AccessSentence name={agent.displayName} env={env} />
                </p>
                {attention && (
                  <p
                    id={`${agent.agentId}-status-note`}
                    data-testid="agent-status-note"
                    className="mt-0.5 text-sm text-muted-foreground"
                  >
                    {attention}
                  </p>
                )}
              </>
            )}
          </div>
        </div>
      </header>

      <Group title="About">
        {!draftMode && (
          <FactRow label="Agent ID" mono>
            {agent.agentId}
          </FactRow>
        )}
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          error={errors.displayName}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          description="One or two sentences, shown in the agent picker."
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          rows={2}
        />
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>

      <section
        data-testid="context-frame"
        aria-labelledby={id("context-heading")}
        className="mb-8 rounded-3xl bg-surface p-4 ring-1 ring-border/60 md:p-5"
      >
        <div className="flex items-start justify-between gap-4">
          <div className="min-w-0">
            <h3
              id={id("context-heading")}
              className="font-mono text-[11px] tracking-wide text-muted-foreground uppercase"
            >
              Instructions and tools
            </h3>
            <p className="mt-1.5 text-sm text-muted-foreground">
              What this agent is told, and what it may use.
            </p>
          </div>
          {(selected || creating) && (
            <DropdownMenu>
              <DropdownMenuTrigger
                render={
                  <Button
                    variant="quiet"
                    size="icon-sm"
                    aria-label="More for instructions and tools"
                    data-testid="context-menu"
                  />
                }
              >
                <MoreHorizontal />
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end" className="w-auto min-w-56">
                <DropdownMenuGroup>
                  <DropdownMenuItem
                    className="whitespace-nowrap"
                    onClick={() => setSwitching(true)}
                  >
                    <ArrowLeftRight /> Use another agent’s instructions
                  </DropdownMenuItem>
                  {shared && (
                    <DropdownMenuItem
                      className="whitespace-nowrap"
                      onClick={() => pick(DUPLICATE)}
                    >
                      <Copy /> Duplicate as new context
                    </DropdownMenuItem>
                  )}
                  {contextLink && (
                    <DropdownMenuItem
                      className="whitespace-nowrap"
                      render={<a href={contextLink} onClick={rememberOrigin} />}
                    >
                      <ArrowUpRight /> Open context page
                    </DropdownMenuItem>
                  )}
                </DropdownMenuGroup>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </div>

        {switching && (
          <div
            data-testid="context-switch"
            className="mt-3 flex flex-wrap items-center justify-between gap-3 border-t border-border/60 pt-3 text-sm"
          >
            <p className="text-muted-foreground">Use the instructions and tools of</p>
            <div className="flex items-center gap-2 max-md:w-full">
              <ContextPicker
                id={id("context")}
                autoOpen
                deployment={deployment}
                value={d.draft.contextChoice}
                invalid={Boolean(errors.contextChoice)}
                labelFor={labelFor}
                hint={hint}
                onPick={(v) => {
                  setSwitching(false);
                  pick(v);
                }}
              />
              <Button variant="quiet" size="sm" onClick={() => setSwitching(false)}>
                Done
              </Button>
            </div>
          </div>
        )}

        {!selected && !creating && !switching && (
          <div
            data-testid="context-empty"
            className="mt-4 rounded-2xl bg-raised px-4 py-4 text-sm ring-1 ring-border/60"
          >
            <p
              className={
                errors.contextChoice ? "text-destructive" : "text-muted-foreground"
              }
            >
              {dangling
                ? "Its instructions were deleted. Start again, or use another agent’s."
                : "No instructions yet, so this agent has no prompt and no tools."}
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <Button
                id={id("context")}
                variant="outline"
                size="sm"
                onClick={() => pick(EMPTY)}
              >
                <Plus /> Start empty
              </Button>
              <Button variant="quiet" size="sm" onClick={() => setSwitching(true)}>
                Use another agent’s…
              </Button>
            </div>
          </div>
        )}

        {(ownership || sharedNote) && (
          <div
            data-testid="context-users"
            className="mt-3 flex flex-wrap items-center justify-between gap-x-4 gap-y-1 border-t border-border/60 pt-3 text-sm"
          >
            <p className="min-w-0 text-muted-foreground">{sharedNote ?? ownership}</p>
          </div>
        )}

        {(selected || creating) && (
          <div className="mt-5 [&>fieldset:last-child]:mb-0">
            <Group>
              <AreaRow
                id={id("prompt")}
                label="System prompt"
                description="What the agent is told at the start of every session."
                value={d.draft.systemPrompt}
                onChange={(v) => d.set("systemPrompt", v)}
                rows={6}
                stacked
              />
            </Group>
            <Group title="Capabilities">
              <RefRow
                id={id("tools")}
                label="Tools"
                description={toolsNote}
                value={d.draft.toolsId}
                onChange={(v) => d.set("toolsId", v)}
                none="None"
                error={errors.toolsId}
                items={deployment.tools.map((t) => ({
                  value: t.tools_id,
                  label: `${t.display_name ?? t.tools_id} · ${toolsInWords(t)}`,
                }))}
                createLabel="New tools…"
                onCreate={() =>
                  new Promise<string | null>((resolve) => {
                    setNewTools(() => resolve);
                  })
                }
                onOpen={(toolsId) => setConfigure({ kind: "tools", id: toolsId })}
              />
              <ChipsRow
                id={id("skills")}
                label="Skills"
                description="None means the agent runs without skills."
                value={d.draft.skillIds}
                onChange={(v) => d.set("skillIds", v)}
                error={errors.skillIds}
                items={deployment.skills.map((sk) => ({
                  value: sk.skillId,
                  label: sk.displayName ?? sk.name ?? sk.skillId,
                }))}
                placeholder="Add a skill"
                empty="No skill by that name."
                createLabel="New skill…"
                onCreate={() =>
                  new Promise<string | null>((resolve) => {
                    setNewSkill(() => resolve);
                  })
                }
              />
              <ChoiceRow
                id={id("compact")}
                label="Compaction"
                description="How a long session is summarized."
                value={d.draft.compactionId}
                onChange={(v) => d.set("compactionId", v)}
                none="Runtime default"
                error={errors.compactionId}
                items={deployment.compactions.map((c) => ({
                  value: c.compaction_id,
                  label: c.display_name ?? c.compaction_id,
                }))}
              />
            </Group>
          </div>
        )}
      </section>

      <Group title="Model">
        <RefRow
          id={id("profile")}
          label="Inference profile"
          description="The backend, model and sampling this agent runs on."
          value={d.draft.inferenceProfileId}
          onChange={(v) => d.set("inferenceProfileId", v)}
          none="None"
          error={errors.inferenceProfileId}
          items={deployment.inferenceProfiles.map((p) => ({
            value: p.profile_id,
            label: profileLabel(deployment, p.profile_id),
          }))}
          createLabel="New profile…"
          onCreate={() =>
            new Promise<string | null>((resolve) => {
              setNewProfile(() => resolve);
            })
          }
          onOpen={(profileId) => setConfigure({ kind: "profile", id: profileId })}
        />
      </Group>
      <ProfileSheet
        deployment={deployment}
        open={newProfile !== null}
        onClose={(profileId) => {
          newProfile?.(profileId);
          setNewProfile(null);
        }}
      />
      <ToolsSheet
        deployment={deployment}
        open={newTools !== null}
        onClose={(toolsId) => {
          newTools?.(toolsId);
          setNewTools(null);
        }}
      />
      <EditorSheet
        open={configure !== null}
        onClose={() => setConfigure(null)}
        title={
          configure?.kind === "tools"
            ? (deployment.tools.find((x) => x.tools_id === configure.id)
                ?.display_name ?? "Tools")
            : (deployment.inferenceProfiles.find((x) => x.profile_id === configure?.id)
                ?.display_name ?? "Model")
        }
        description={
          configure?.kind === "profile"
            ? (() => {
                const p = deployment.inferenceProfiles.find(
                  (x) => x.profile_id === configure.id,
                );
                return p ? modelSentence(deployment, p) : undefined;
              })()
            : (() => {
                const t = deployment.tools.find((x) => x.tools_id === configure?.id);
                return t ? toolsInWords(t) : undefined;
              })()
        }
        page={
          configure
            ? {
                ...base,
                section: configure.kind === "tools" ? "tools" : "profiles",
                item: configure.id,
              }
            : undefined
        }
      >
        {configure?.kind === "tools" &&
          (() => {
            const t = deployment.tools.find((x) => x.tools_id === configure.id);
            return t ? (
              <ToolsEditor deployment={deployment} tools={t} embedded />
            ) : null;
          })()}
        {configure?.kind === "profile" &&
          (() => {
            const p = deployment.inferenceProfiles.find(
              (x) => x.profile_id === configure.id,
            );
            return p ? (
              <ProfileEditor deployment={deployment} profile={p} embedded />
            ) : null;
          })()}
      </EditorSheet>
      <NewSkillSheet
        deployment={deployment}
        open={newSkill !== null}
        onClose={(skillId) => {
          newSkill?.(skillId);
          setNewSkill(null);
        }}
      />

      <AlertDialog open={confirmShared} onOpenChange={setConfirmShared}>
        <AlertDialogContent aria-modal="true" className="sm:max-w-lg">
          <AlertDialogHeader>
            <AlertDialogTitle>This also changes {sharedWith}</AlertDialogTitle>
            <AlertDialogDescription>
              {agent.displayName} shares its instructions and tools with {sharedWith}.
              Save the change for {others.length === 1 ? "both" : "all of them"}, or
              give {agent.displayName} its own copy and leave the others as they are.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <Button
              variant="outline"
              onClick={() => {
                setConfirmShared(false);
                void finish({ ...pendingIntent, copy: true });
              }}
            >
              Save as its own copy
            </Button>
            <Button
              variant="brand"
              onClick={() => {
                setConfirmShared(false);
                void finish(pendingIntent);
              }}
            >
              {others.length === 1
                ? "Save for both"
                : `Save for all ${others.length + 1}`}
            </Button>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      {confirmUnused && (
        <ConfirmDelete
          label={nameOf(confirmUnused)}
          noun="context"
          open
          onOpenChange={(open) => {
            if (!open) setConfirmUnused(null);
          }}
          warning={dependentsWarning(deployment, "context", confirmUnused.context_id)}
          onDelete={() =>
            changeConfig("deleteContextConfig", {
              contextId: confirmUnused.context_id,
              nodeDid: deployment.nodeDid,
            })
          }
        />
      )}
      <DraftActions
        draft={d}
        fields={fields}
        saveLabel={draftMode ? "Create" : undefined}
        onSave={() => save()}
        onCancel={() => {
          setSwitching(false);
          if (draftMode) {
            draftMode.onCancel();
            return;
          }
          d.reset();
        }}
      />
      {!draftMode && !embedded && (
        <DeleteButton
          label={agent.displayName}
          warning={dependentsWarning(deployment, "agent", agent.agentId)}
          base={base}
          companion={
            soleContext
              ? {
                  label: `Also delete its instructions and tools (${nameOf(soleContext)})`,
                  onDelete: () =>
                    changeConfig("deleteContextConfig", {
                      contextId: soleContext.context_id,
                      nodeDid: deployment.nodeDid,
                    }),
                }
              : undefined
          }
          onDelete={() =>
            changeConfig("deleteAgentConfig", {
              agentId: agent.agentId,
              nodeDid: deployment.nodeDid,
            })
          }
        />
      )}
    </>
  );
}
