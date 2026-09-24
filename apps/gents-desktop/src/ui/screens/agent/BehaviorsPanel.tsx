/* Behaviors, with their context edited inline. The data model is unchanged
   (Behavior and AgentContext stay two documents), but the context is a
   visible block on the behavior: pick one, see who else uses it, duplicate
   it or start empty, and edit its instructions and capabilities in place.
   Everything waits for one Save. */
import { useExclusivePopover } from "@/hooks/useExclusivePopover";
import { dependentsWarning } from "./dependents";
import { Fragment, useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowLeftRight,
  ArrowUpRight,
  Copy,
  MoreHorizontal,
  Plus,
} from "lucide-react";
import type {
  AgentContext,
  BehaviorView,
  DeploymentView,
} from "@source-inc/gents-desktop-client";
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
import { Switch } from "@gents/ui/components/switch";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxTrigger,
} from "@gents/ui/components/combobox";
import { toast } from "sonner";
import type { Shell } from "@/hooks/useShell";
import { href, navigate } from "@/lib/router";
import { behaviorReadiness } from "@/lib/behavior-readiness";
import { bashAccess, fileAccess, network } from "../behavior";
import { BehaviorAvatar } from "../parts";
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
import { ProfileEditor, modelSentence } from "./ProfilesPanel";
import { newId, useDraft } from "./draft";
import { ConfirmDelete, DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { rememberContextOrigin } from "./contextOrigin";
import { clearPromptFocus, promptFocusRequested } from "./promptFocus";

/* the access modes at a glance, short enough for one line */
const short = (mode: string | null | undefined) =>
  ({ "read / write": "rw", "read-only": "ro", unrestricted: "any", off: "off" })[
    mode ?? "off"
  ] ?? mode;

/* picker values for a context that does not exist until Save */
const DUPLICATE = "new:duplicate";
const EMPTY = "new:empty";
const isNew = (choice: string) => choice === DUPLICATE || choice === EMPTY;

type Draft = {
  displayName: string;
  description: string;
  tags: string[];
  /* an existing context_id, '' for none, or DUPLICATE / EMPTY */
  contextChoice: string;
  /* for a new context: its name, and the context it copies */
  contextName: string;
  copiedFrom: string;
  systemPrompt: string;
  toolsId: string;
  compactionId: string;
  skillIds: string[];
  inferenceProfileId: string;
};

/* what a save should also do: make the change its own copy */
type SaveIntent = { copy?: boolean };

const contextFields = (c: AgentContext | null) => ({
  systemPrompt: c?.system_prompt ?? "",
  toolsId: c?.tools_id ?? "",
  compactionId: c?.compaction_id ?? "",
  skillIds: c?.skill_ids ?? [],
});

const nameOf = (c: AgentContext) => c.display_name ?? c.context_id;

/* the behaviors that point at a context, as saved */
const usersOf = (deployment: DeploymentView, contextId: string) =>
  deployment.behaviors.filter((b) => b.contextId === contextId);

const listNames = (names: string[]) =>
  names.length <= 2 ? names.join(" and ") : `${names[0]} and ${names.length - 1} more`;

/* a context name no other context has */
function freshName(deployment: DeploymentView, base: string) {
  const taken = new Set(deployment.contexts.map((c) => c.display_name));
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base} ${n}`)) return `${base} ${n}`;
}

/* what stops a save, keyed by the field that shows it */
function problems(deployment: DeploymentView, next: Draft, draft = false) {
  const out: Partial<Record<keyof Draft, string>> = {};
  if (!next.displayName.trim()) out.displayName = "Give the behavior a name.";
  if (!next.contextChoice)
    out.contextChoice =
      "Choose a context or start one. Without one the behavior has no instructions and no tools.";
  else if (
    !isNew(next.contextChoice) &&
    !deployment.contexts.some((c) => c.context_id === next.contextChoice)
  )
    out.contextChoice = "That context no longer exists. Choose another.";
  /* a draft's context is named after the behavior at save */
  if (!draft && isNew(next.contextChoice) && !next.contextName.trim())
    out.contextName = "Name the new context.";
  if (!next.inferenceProfileId)
    out.inferenceProfileId = "Choose the inference profile it runs on.";
  else if (
    !deployment.inferenceProfiles.some((p) => p.profile_id === next.inferenceProfileId)
  )
    out.inferenceProfileId = "That profile no longer exists. Choose another.";
  if (next.contextChoice && !out.contextChoice) {
    if (next.toolsId && !deployment.tools.some((t) => t.tools_id === next.toolsId))
      out.toolsId = "That Tools document no longer exists.";
    if (
      next.compactionId &&
      !deployment.compactions.some((c) => c.compaction_id === next.compactionId)
    )
      out.compactionId = "That compaction no longer exists.";
    const unknown = next.skillIds.filter(
      (s) => !deployment.skills.some((k) => k.skillId === s),
    );
    if (unknown.length)
      out.skillIds = `Unknown ${unknown.length === 1 ? "skill" : "skills"}: ${unknown.join(", ")}`;
  }
  return out;
}

/* the order the fields appear in, for focusing the first problem */
const FIELD_ORDER: [keyof Draft, string][] = [
  ["displayName", "name"],
  ["contextChoice", "context"],
  ["contextName", "context-name"],
  ["toolsId", "tools"],
  ["skillIds", "skills"],
  ["compactionId", "compact"],
  ["inferenceProfileId", "profile"],
];

/* the kit select trigger's look, for a combobox that opens like a select */
const TRIGGER =
  "flex h-9 w-auto min-w-48 max-w-full items-center justify-between gap-1.5 rounded-2xl border border-transparent bg-input/50 px-3 text-sm whitespace-nowrap outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/30 aria-invalid:border-destructive aria-invalid:ring-3 aria-invalid:ring-destructive/20 max-md:w-full";

/* Picks whose instructions and tools a behavior uses. Searchable, because an
   agent can have hundreds of contexts; each row says who uses it. */
function ContextPicker({
  id,
  deployment,
  value,
  invalid,
  labelFor,
  hint,
  onPick,
  autoOpen = false,
}: {
  id: string;
  deployment: DeploymentView;
  value: string;
  invalid: boolean;
  labelFor: (value: string) => string;
  hint: (c: AgentContext) => string;
  onPick: (value: string) => void;
  autoOpen?: boolean;
}) {
  /* a searchable popup is a dialog: one at a time with the shell's others */
  const popover = useExclusivePopover();
  const { onOpenChange } = popover;
  useEffect(() => {
    if (autoOpen) onOpenChange(true);
  }, [autoOpen, onOpenChange]);
  const byId = new Map(deployment.contexts.map((c) => [c.context_id, c]));
  return (
    <Combobox
      items={deployment.contexts.map((c) => c.context_id)}
      value={value || null}
      open={popover.open}
      onOpenChange={popover.onOpenChange}
      onOpenChangeComplete={popover.onOpenChangeComplete}
      onValueChange={(v) => {
        if (v == null) return;
        popover.onOpenChange(false);
        onPick(v);
      }}
      itemToStringLabel={labelFor}
      filter={(v: string, query: string) => {
        const q = query.trim().toLowerCase();
        const c = byId.get(v);
        return (
          !q || (c ? `${nameOf(c)} ${hint(c)}` : labelFor(v)).toLowerCase().includes(q)
        );
      }}
    >
      <ComboboxTrigger
        id={id}
        aria-label="Instructions and tools from"
        aria-invalid={invalid || undefined}
        className={TRIGGER}
      >
        <span className={`truncate ${value ? "" : "text-muted-foreground"}`}>
          {value ? labelFor(value) : "Choose"}
        </span>
      </ComboboxTrigger>
      <ComboboxContent
        ref={popover.popupRef}
        aria-label="Instructions and tools from"
        className="w-96 min-w-96 max-md:w-[calc(100vw-2rem)] max-md:min-w-0"
      >
        <ComboboxInput
          showTrigger={false}
          aria-label="Search contexts"
          placeholder={`Search ${deployment.contexts.length} contexts or behaviors`}
        />
        <ComboboxEmpty>Nothing by that name.</ComboboxEmpty>
        <ComboboxList>
          {(contextId: string) => {
            const c = byId.get(contextId);
            return (
              <ComboboxItem key={contextId} value={contextId}>
                <span className="flex min-w-0 flex-1 items-baseline justify-between gap-4">
                  <span className="truncate">{c ? nameOf(c) : contextId}</span>
                  <span className="max-w-40 shrink-0 truncate text-xs text-muted-foreground">
                    {c ? hint(c) : ""}
                  </span>
                </span>
              </ComboboxItem>
            );
          }}
        </ComboboxList>
      </ComboboxContent>
    </Combobox>
  );
}

/* one-click changes that a behavior's row and its header share */
/* the switch changes only `enabled`: a patch, so a behavior with no profile
   yet is never rewritten with an empty profile reference */
async function saveEnabled(
  shell: Shell,
  deployment: DeploymentView,
  b: BehaviorView,
  next: boolean,
) {
  await shell.applyConfig((api) =>
    api.patchConfigComponents({
      agentDid: deployment.agentDid,
      patches: [
        { collection: "AgentBehavior", id: b.behaviorId, changes: { enabled: next } },
      ],
    }),
  );
}

/* One apply enables the behavior and names it the default: publication
   rejects a disabled default, and it decides whether the behavior can run. */
export async function saveDefault(
  shell: Shell,
  deployment: DeploymentView,
  behaviorId: string,
) {
  await shell.applyConfig((api) =>
    api.setDefaultBehavior({ agentDid: deployment.agentDid, behaviorId }),
  );
}

export const DEFAULT_STAYS_ENABLED =
  "The default behavior stays enabled. Choose another default before turning it off.";

/* the end of a behavior's row: its enable switch and a menu */
function RowControls({
  shell,
  deployment,
  behavior,
  inEditor = false,
}: {
  shell: Shell;
  deployment: DeploymentView;
  behavior: BehaviorView;
  /* at the top of the behavior's page: the switch says its state, and there is no Edit */
  inEditor?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  /* the current default is never turned off in place */
  const keptOn = behavior.enabled && behavior.isDefault;
  const toggle = async (next: boolean) => {
    setBusy(true);
    try {
      await saveEnabled(shell, deployment, behavior, next);
      toast(`${behavior.displayName} is ${next ? "enabled" : "disabled"}`);
    } catch (e) {
      toast(
        `Couldn’t turn it ${next ? "on" : "off"}: ${e instanceof Error ? e.message : String(e)}`,
      );
    } finally {
      setBusy(false);
    }
  };
  const makeDefault = async () => {
    try {
      await saveDefault(shell, deployment, behavior.behaviorId);
      toast(
        behavior.enabled
          ? "Default behavior set"
          : `${behavior.displayName} is enabled and is now the default`,
      );
    } catch (e) {
      toast(
        `Default behavior set failed: ${e instanceof Error ? e.message : String(e)}`,
      );
    }
  };
  return (
    <div
      className="flex items-center gap-1"
      data-testid={inEditor ? "behavior-status" : "behavior-row-controls"}
    >
      <span
        className="flex items-center gap-2.5 px-1 text-sm"
        title={keptOn ? DEFAULT_STAYS_ENABLED : undefined}
      >
        {inEditor && (behavior.enabled ? "Enabled" : "Disabled")}
        <Switch
          aria-label={`${behavior.displayName} is ${behavior.enabled ? "enabled" : "disabled"}`}
          aria-description={keptOn ? DEFAULT_STAYS_ENABLED : undefined}
          checked={behavior.enabled}
          disabled={busy || keptOn}
          onCheckedChange={(v) => void toggle(v)}
        />
      </span>
      <DropdownMenu>
        <DropdownMenuTrigger
          render={
            <Button
              variant="quiet"
              size="icon-sm"
              aria-label={`More for ${behavior.displayName}`}
            />
          }
        >
          <MoreHorizontal />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-auto min-w-44">
          <DropdownMenuGroup>
            <DropdownMenuItem
              className="whitespace-nowrap"
              disabled={behavior.isDefault}
              onClick={() => void makeDefault()}
            >
              {behavior.isDefault ? (
                "The default behavior"
              ) : (
                <span className="flex flex-col">
                  <span>Make default</span>
                  {!behavior.enabled && (
                    <span className="text-xs text-muted-foreground">
                      Also enables it
                    </span>
                  )}
                </span>
              )}
            </DropdownMenuItem>
            {!inEditor && (
              <DropdownMenuItem
                className="whitespace-nowrap"
                render={
                  <a
                    href={href({
                      name: "agent",
                      agentDid: deployment.agentDid,
                      section: "behaviors",
                      item: behavior.behaviorId,
                    })}
                  />
                }
              >
                Edit
              </DropdownMenuItem>
            )}
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}

/* the behavior a draft starts from: nothing saved, the default model */
export function newBehaviorView(deployment: DeploymentView): BehaviorView {
  return {
    behaviorId: newId("behavior"),
    agentDid: deployment.agentDid,
    displayName: "",
    description: null,
    contextId: null,
    inferenceProfileId:
      deployment.behaviors.find((b) => b.isDefault)?.inferenceProfileId ??
      deployment.inferenceProfiles[0]?.profile_id ??
      null,
    enabled: false,
    isDefault: false,
    tags: [],
    createdAt: null,
  };
}

export type DraftMode = {
  /* the new behavior's id once saved */
  onSaved: (behaviorId: string) => void;
  onCancel: () => void;
  /* from a session's composer: saved enabled, to be used at once */
  enabled?: boolean;
};

export function BehaviorEditor({
  shell,
  deployment,
  behavior,
  draft: draftMode,
  embedded = false,
}: {
  shell: Shell;
  deployment: DeploymentView;
  behavior: BehaviorView;
  /* a new behavior that exists only on this page until Save */
  draft?: DraftMode;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "behaviors",
  };
  const context: AgentContext | null =
    deployment.contexts.find((c) => c.context_id === behavior.contextId) ?? null;
  const saved: Draft = {
    displayName: behavior.displayName,
    description: behavior.description ?? "",
    tags: behavior.tags ?? [],
    /* a draft starts its own empty instructions; Save creates them */
    contextChoice: draftMode ? EMPTY : (behavior.contextId ?? ""),
    contextName: "",
    copiedFrom: "",
    ...contextFields(context),
    inferenceProfileId: behavior.inferenceProfileId ?? "",
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
      const asCopy = Boolean(want.copy);
      const creating = isNew(next.contextChoice) || asCopy;
      const newName =
        next.contextName.trim() ||
        freshName(deployment, `${next.displayName.trim() || "New"} context`);
      const target = creating
        ? null
        : (deployment.contexts.find((c) => c.context_id === next.contextChoice) ??
          null);
      /* one id per new context for the life of this editor, so a retry after a
       failure writes the same document instead of minting another */
      if (creating && !pendingContextId.current)
        pendingContextId.current = newId("ctx");
      const contextId = creating ? pendingContextId.current! : next.contextChoice;
      const fields = {
        system_prompt: next.systemPrompt || null,
        tools_id: next.toolsId || null,
        compaction_id: next.compactionId || null,
        skill_ids: next.skillIds.length ? next.skillIds : null,
      };
      const edited =
        target !== null &&
        JSON.stringify(contextFields(target)) !==
          JSON.stringify({
            systemPrompt: next.systemPrompt,
            toolsId: next.toolsId,
            compactionId: next.compactionId,
            skillIds: next.skillIds,
          });
      const behaviorDocument = {
        behavior_id: behavior.behaviorId,
        agent_did: deployment.agentDid,
        display_name: next.displayName.trim(),
        description: next.description.trim() || null,
        context_id: contextId || null,
        inference_profile_id: next.inferenceProfileId,
        enabled: draftMode ? Boolean(draftMode.enabled) : behavior.enabled,
        tags: next.tags.length ? next.tags : null,
        created_at: behavior.createdAt,
      };
      /* only the context fields this edit changed, so a concurrent edit to
         another field of a shared context is not overwritten */
      const before = target ? contextFields(target) : null;
      const changedFields: Partial<typeof fields> = {};
      if (before) {
        if (before.systemPrompt !== next.systemPrompt)
          changedFields.system_prompt = fields.system_prompt;
        if (before.toolsId !== next.toolsId) changedFields.tools_id = fields.tools_id;
        if (before.compactionId !== next.compactionId)
          changedFields.compaction_id = fields.compaction_id;
        if (JSON.stringify(before.skillIds) !== JSON.stringify(next.skillIds))
          changedFields.skill_ids = fields.skill_ids;
      }
      await shell.applyConfig((api) => {
        /* a new context and the behavior that points at it land together or
           not at all: one component apply is one transaction */
        if (creating)
          return api.applyConfigComponents({
            document: {
              agent_principal: { agent_did: deployment.agentDid },
              contexts: [
                {
                  context_id: contextId,
                  agent_did: deployment.agentDid,
                  display_name: newName,
                  description: null,
                  ...fields,
                  tags: null,
                },
              ],
              agent_behaviors: [behaviorDocument],
            },
          });
        if (target && edited && !draftMode)
          /* an existing behavior and its existing context: one patch call, one
             transaction, changed fields only */
          return api.patchConfigComponents({
            agentDid: deployment.agentDid,
            patches: [
              {
                collection: "AgentContext",
                id: target.context_id,
                changes: changedFields,
              },
              {
                collection: "AgentBehavior",
                id: behavior.behaviorId,
                changes: {
                  display_name: behaviorDocument.display_name,
                  description: behaviorDocument.description,
                  context_id: behaviorDocument.context_id,
                  inference_profile_id: behaviorDocument.inference_profile_id,
                  tags: behaviorDocument.tags,
                },
              },
            ],
          });
        if (target && edited)
          /* a new behavior on an edited existing context: the behavior has no
             document to patch yet, so the context is patched with it applied */
          return api
            .patchConfigComponents({
              agentDid: deployment.agentDid,
              patches: [
                {
                  collection: "AgentContext",
                  id: target.context_id,
                  changes: changedFields,
                },
              ],
            })
            .then(() => api.saveBehaviorConfig({ document: behaviorDocument }));
        return api.saveBehaviorConfig({ document: behaviorDocument });
      });
      if (creating) pendingContextId.current = null;
      /* say what happened to the contexts; offer to clear one left unused */
      const said: string[] = [];
      if (creating) said.push(`Created ${newName}.`);
      const left =
        context && context.context_id !== contextId
          ? usersOf(deployment, context.context_id).filter(
              (b) => b.behaviorId !== behavior.behaviorId,
            )
          : null;
      if (context && left) {
        said.push(
          left.length
            ? `${nameOf(context)} is still used by ${listNames(left.map((b) => b.displayName))}.`
            : `${nameOf(context)} is no longer used.`,
        );
      }
      const unused = context && left && left.length === 0 ? context : null;
      if (!said.length) return undefined;
      return {
        savedToast: `Saved. ${said.join(" ")}`,
        savedAction: unused
          ? { label: "Delete it", onClick: () => setConfirmUnused(unused) }
          : undefined,
      };
    },
    { isNew: draftMode !== undefined },
  );
  const id = (f: string) => `${behavior.behaviorId}-${f}`;
  const errors = problems(deployment, d.draft, draftMode !== undefined);
  /* a draft closes only once the bridge has the behavior */
  const finish = async (intent: SaveIntent) => {
    const ok = await d.save(intent);
    if (ok && draftMode) draftMode.onSaved(behavior.behaviorId);
  };
  /* problems stay at their fields; Save takes you to the first one */
  const save = (intent: SaveIntent = {}): void | Promise<void> => {
    const first = FIELD_ORDER.find(([field]) => errors[field]);
    if (first) {
      document.getElementById(id(first[1]))?.focus();
      return;
    }
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
        (b) => b.behaviorId !== behavior.behaviorId,
      )
    : [];
  /* a change to shared instructions asks, at Save, whose change it is */
  const [confirmShared, setConfirmShared] = useState(false);
  /* what the save waiting on that question should also do */
  const [pendingIntent, setPendingIntent] = useState<SaveIntent>({});
  /* just created: open on the prompt, expanded and focused */
  const [focusPrompt] = useState(() => promptFocusRequested(behavior.behaviorId));
  useEffect(() => {
    if (!focusPrompt) return;
    clearPromptFocus();
    const el = document.getElementById(`${behavior.behaviorId}-prompt`);
    el?.scrollIntoView({ block: "center" });
    el?.focus();
  }, [focusPrompt, behavior.behaviorId]);
  /* switching to another behavior's instructions opens the picker inline */
  const [switching, setSwitching] = useState(false);
  const shared = others.length > 0;
  /* a changed field on instructions other behaviors also use */
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
  const usersByContext = new Map<string, BehaviorView[]>();
  for (const b of deployment.behaviors) {
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
    if (selected) rememberContextOrigin(selected.context_id, behavior.behaviorId);
  };
  const toolsSharers = d.draft.toolsId
    ? deployment.contexts.filter(
        (c) => c.tools_id === d.draft.toolsId && c.context_id !== selected?.context_id,
      )
    : [];
  const toolsNote = !d.draft.toolsId
    ? "No tools: the behavior can only talk."
    : toolsSharers.length
      ? `Also used by ${toolsSharers.length} other ${toolsSharers.length === 1 ? "context" : "contexts"}; edits to the document reach them too.`
      : "Only this behavior uses these tools.";
  /* the block only speaks up when there is something to act on: shared
     instructions, or ones that Save will create */
  const sharedWith = listNames(others.map((b) => b.displayName));
  const ownership = creating
    ? copiedFrom
      ? `A copy of ${nameOf(copiedFrom)} for this behavior, created when you save.`
      : "New empty instructions for this behavior, created when you save."
    : null;
  /* the behaviors that share these instructions, each a link; past two,
     the rest are counted and the count opens the context, which lists them */
  const behaviorLink = (b: BehaviorView) => (
    <a
      href={href({ ...base, item: b.behaviorId })}
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
            <Fragment key={b.behaviorId}>
              {i > 0 && " and "}
              {behaviorLink(b)}
            </Fragment>
          ))
        ) : (
          <>
            {behaviorLink(others[0]!)} and{" "}
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
  /* a context only this behavior uses can go with it */
  const soleContext =
    context &&
    (usersByContext.get(context.context_id) ?? []).every(
      (b) => b.behaviorId === behavior.behaviorId,
    )
      ? context
      : null;
  const readiness = behaviorReadiness(deployment, behavior.behaviorId);
  const env = deployment.behaviorEnvironments.find(
    (e) => e.behaviorId === behavior.behaviorId,
  );
  /* said under the summary only when something needs attention */
  const attention =
    behavior.enabled && !readiness.ready ? `It can’t run: ${readiness.reason}.` : null;
  return (
    <>
      <header
        data-testid="behavior-header"
        className="mb-8 rounded-3xl bg-raised px-5 py-4 shadow-sm ring-1 ring-foreground/5"
      >
        {/* avatar top-aligned beside the text; above it on phones */}
        <div className="flex min-w-0 items-start gap-3 max-md:flex-col">
          <BehaviorAvatar
            name={
              draftMode ? d.draft.displayName.trim() || "New" : behavior.displayName
            }
            behaviorId={behavior.behaviorId}
            className="size-10 shrink-0 text-sm"
          />
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="truncate font-heading text-lg font-medium text-heading">
                {draftMode
                  ? d.draft.displayName.trim() || "New behavior"
                  : behavior.displayName}
              </h2>
              {behavior.isDefault && <Badge variant="secondary">Default</Badge>}
              {draftMode && <Badge variant="secondary">Draft</Badge>}
            </div>
            {draftMode ? null : (
              <>
                <p
                  data-testid="behavior-summary"
                  className="mt-0.5 text-sm text-muted-foreground"
                >
                  {behavior.displayName}{" "}
                  <strong className="font-medium text-foreground">can</strong>{" "}
                  {env
                    ? `${fileAccess(env.fileAccess)} files and ${bashAccess(env.bashAccess)} commands`
                    : "…"}
                  , and{" "}
                  <strong className="font-medium text-foreground">has access</strong> to{" "}
                  {network(env?.networkAccess)}.
                </p>
                {attention && (
                  <p
                    id={`${behavior.behaviorId}-status-note`}
                    data-testid="behavior-status-note"
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
          <FactRow label="Behavior ID" mono>
            {behavior.behaviorId}
          </FactRow>
        )}
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
          error={errors.displayName}
        />
        <AreaRow
          id={id("description")}
          label="Description"
          description="One or two sentences, shown in the behavior picker."
          value={d.draft.description}
          onChange={(v) => d.set("description", v)}
          onCommit={d.commit}
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
              What this behavior is told, and what it may use.
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
                    <ArrowLeftRight /> Use another behavior’s instructions
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
                ? "Its instructions were deleted. Start again, or use another behavior’s."
                : "No instructions yet, so this behavior has no prompt and no tools."}
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
                Use another behavior’s…
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
                onCommit={d.commit}
                rows={6}
                stacked
                expandedByDefault={focusPrompt}
              />
            </Group>
            <Group title="Capabilities">
              <RefRow
                id={id("tools")}
                label="Tools"
                description={toolsNote}
                value={d.draft.toolsId}
                onChange={(v) => d.choose("toolsId", v)}
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
                description="None means the behavior runs without skills."
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
                onChange={(v) => d.choose("compactionId", v)}
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
          description="The backend, model and sampling this behavior runs on."
          value={d.draft.inferenceProfileId}
          onChange={(v) => d.choose("inferenceProfileId", v)}
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
        shell={shell}
        deployment={deployment}
        open={newProfile !== null}
        onClose={(profileId) => {
          newProfile?.(profileId);
          setNewProfile(null);
        }}
      />
      <ToolsSheet
        shell={shell}
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
              <ToolsEditor shell={shell} deployment={deployment} tools={t} embedded />
            ) : null;
          })()}
        {configure?.kind === "profile" &&
          (() => {
            const p = deployment.inferenceProfiles.find(
              (x) => x.profile_id === configure.id,
            );
            return p ? (
              <ProfileEditor
                shell={shell}
                deployment={deployment}
                profile={p}
                embedded
              />
            ) : null;
          })()}
      </EditorSheet>
      <NewSkillSheet
        shell={shell}
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
              {behavior.displayName} shares its instructions and tools with {sharedWith}
              . Save the change for {others.length === 1 ? "both" : "all of them"}, or
              give {behavior.displayName} its own copy and leave the others as they are.
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
            shell.applyConfig((api) =>
              api.deleteContextConfig({
                contextId: confirmUnused.context_id,
                agentDid: deployment.agentDid,
              }),
            )
          }
        />
      )}
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
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
          label={behavior.displayName}
          warning={dependentsWarning(deployment, "behavior", behavior.behaviorId)}
          base={base}
          companion={
            soleContext
              ? {
                  label: `Also delete its instructions and tools (${nameOf(soleContext)})`,
                  onDelete: () =>
                    shell.applyConfig((api) =>
                      api.deleteContextConfig({
                        contextId: soleContext.context_id,
                        agentDid: deployment.agentDid,
                      }),
                    ),
                }
              : undefined
          }
          onDelete={() =>
            shell.applyConfig((api) =>
              api.deleteBehaviorConfig({
                behaviorId: behavior.behaviorId,
                agentDid: deployment.agentDid,
              }),
            )
          }
        />
      )}
    </>
  );
}

export function BehaviorsPanel({
  shell,
  deployment,
  behaviorId,
}: {
  shell: Shell;
  deployment: DeploymentView;
  behaviorId?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "behaviors",
  };
  /* New behavior: a draft page, nothing saved until Save */
  const [draft, setDraft] = useState<BehaviorView | null>(null);
  const inUse = new Set(deployment.behaviors.map((b) => b.contextId));
  const unusedContexts = deployment.contexts.filter(
    (c) => !inUse.has(c.context_id),
  ).length;
  /* who uses each context, so a row can say when its instructions are shared */
  const byContext = new Map<string, BehaviorView[]>();
  for (const b of deployment.behaviors) {
    if (!b.contextId) continue;
    const list = byContext.get(b.contextId);
    if (list) list.push(b);
    else byContext.set(b.contextId, [b]);
  }
  const contextIds = new Set(deployment.contexts.map((c) => c.context_id));
  const line = (b: BehaviorView) => {
    const readiness = behaviorReadiness(deployment, b.behaviorId);
    if (!readiness.ready) return `unavailable: ${readiness.reason}`;
    const e = deployment.behaviorEnvironments.find(
      (x) => x.behaviorId === b.behaviorId,
    );
    const sharing = (b.contextId ? (byContext.get(b.contextId) ?? []) : []).filter(
      (x) => x.behaviorId !== b.behaviorId,
    );
    return [
      e?.modelName ?? "no backend",
      `files ${short(e?.fileAccess)}`,
      `bash ${short(e?.bashAccess)}`,
      ...(!b.contextId || !contextIds.has(b.contextId) ? ["no instructions"] : []),
      ...(sharing.length
        ? [`shared with ${listNames(sharing.map((x) => x.displayName))}`]
        : []),
    ].join(" · ");
  };
  if (draft)
    return (
      <div>
        <div className="mb-6 flex items-center justify-between gap-3">
          <button
            type="button"
            onClick={() => setDraft(null)}
            className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Behaviors
          </button>
        </div>
        <BehaviorEditor
          key={draft.behaviorId}
          shell={shell}
          deployment={deployment}
          behavior={draft}
          draft={{
            onSaved: (id) => {
              setDraft(null);
              navigate({ ...base, item: id });
            },
            onCancel: () => setDraft(null),
          }}
        />
      </div>
    );
  return (
    <>
      <ListDetail
        base={base}
        item={behaviorId}
        toolbar={(id) => {
          const b = deployment.behaviors.find((x) => x.behaviorId === id);
          return b ? (
            <RowControls shell={shell} deployment={deployment} behavior={b} inEditor />
          ) : null;
        }}
        /* the default is pinned first and named beside its title */
        rows={[...deployment.behaviors]
          .sort((a, b) => Number(b.isDefault) - Number(a.isDefault))
          .map((b) => ({
            tags: b.tags,
            id: b.behaviorId,
            title: b.displayName,
            titleNote: b.isDefault ? "Default" : undefined,
            meta: line(b),
            metaMono: true,
            trailing: (
              <RowControls shell={shell} deployment={deployment} behavior={b} />
            ),
            icon: (
              <BehaviorAvatar
                name={b.displayName}
                behaviorId={b.behaviorId}
                className="size-6 border-0 bg-transparent text-[10px]"
              />
            ),
          }))}
        createLabel="New behavior"
        empty="No behaviors yet. A behavior is what an agent is told, what it may use, and what runs it."
        onCreate={() => setDraft(newBehaviorView(deployment))}
        detail={(id) => {
          const behavior = deployment.behaviors.find((b) => b.behaviorId === id)!;
          return (
            <BehaviorEditor
              key={behavior.behaviorId}
              shell={shell}
              deployment={deployment}
              behavior={behavior}
            />
          );
        }}
      />
      {!behaviorId && unusedContexts > 0 && (
        <p data-testid="unused-contexts" className="mt-3 text-sm text-muted-foreground">
          {unusedContexts === 1
            ? "1 context isn’t used by any behavior"
            : `${unusedContexts} contexts aren’t used by any behavior`}{" "}
          ·{" "}
          <a
            href={href({ ...base, section: "contexts" })}
            className="text-foreground underline-offset-2 hover:underline"
          >
            Review
          </a>
        </p>
      )}
    </>
  );
}
