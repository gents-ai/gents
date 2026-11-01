import type { NodeView } from "../../../hooks/fleetStore";
import { useState } from "react";
import type { CompactionConfig } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { ChoiceRow, DraftActions, NumberRow, TagsRow, TextRow } from "./editors";
import { EditorSheet } from "./EditorSheet";
import {
  newId,
  optionalInteger,
  optionalNumber,
  problemOf,
  str,
  useDraft,
} from "./draft";
import { profileLabel } from "./automation";
import { Group } from "./rows";

type Strategy = NonNullable<CompactionConfig["strategy"]>;

/** What the runtime uses for a field a compaction leaves empty. */
export const COMPACTION_DEFAULTS = {
  strategy: "StripThenSummarize" as Strategy,
  threshold: 0.75,
  keepRecentTokens: 20_000,
  toolResultMaxChars: 2_000,
  summaryMaxOutputTokens: 32_768,
  summaryFileListMax: 100,
};

const STRATEGIES: { value: Strategy; label: string }[] = [
  { value: "StripThenSummarize", label: "Trim tool results, then summarize" },
  { value: "StripToolResults", label: "Trim tool results only" },
];

const tokens = (n: number) => (n >= 1000 ? `${Math.round(n / 1000)}k` : String(n));

/** One line on what a compaction does, for under the field that picks it. */
export function compactionInWords(
  deployment: NodeView,
  c: CompactionConfig | null | undefined,
): string {
  const threshold = c?.threshold ?? COMPACTION_DEFAULTS.threshold;
  const keep = c?.keep_recent_tokens ?? COMPACTION_DEFAULTS.keepRecentTokens;
  const strategy = c?.strategy ?? COMPACTION_DEFAULTS.strategy;
  const at = `At ${Math.round(threshold * 100)}% of the context window`;
  const what =
    strategy === "StripToolResults"
      ? "trims old tool results"
      : `trims old tool results and summarizes the rest${
          c?.inference_profile_id
            ? `, written by ${profileLabel(deployment, c.inference_profile_id)}`
            : ""
        }`;
  return `${at}, ${what}, keeping the last ${tokens(keep)} tokens.`;
}

export function newCompactionDocument(deployment: NodeView): CompactionConfig {
  return {
    compaction_id: newId("compaction"),
    node_did: deployment.nodeDid,
    display_name: "",
  };
}

export function CompactionEditor({
  deployment,
  compaction,
  draft: draftMode,
}: {
  deployment: NodeView;
  compaction: CompactionConfig;
  /** creating: the sheet's Create and Cancel */
  draft?: { onSaved: (compactionId: string) => void; onCancel: () => void };
}) {
  const { changeConfig } = useApp().actions;
  const saved = {
    displayName: compaction.display_name ?? "",
    strategy: (compaction.strategy ?? "") as Strategy | "",
    threshold:
      compaction.threshold == null
        ? ""
        : String(Math.round(compaction.threshold * 100)),
    keepRecentTokens: str(compaction.keep_recent_tokens),
    toolResultMaxChars: str(compaction.tool_result_max_chars),
    summaryMaxOutputTokens: str(compaction.summary_max_output_tokens),
    summaryFileListMax: str(compaction.summary_file_list_max),
    profileId: compaction.inference_profile_id ?? "",
    tags: compaction.tags ?? [],
  };
  /* the runtime's own bounds; it rejects anything outside them */
  const MAX: Record<string, number> = {
    "Summary length": 32_768,
    "Files listed": 1_000,
  };
  const positive = (label: string, value: string) =>
    optionalInteger(label, value, { min: 1, max: MAX[label] });
  const d = useDraft(
    saved,
    async (next) => {
      const percent = optionalNumber("Threshold", next.threshold, { min: 1, max: 100 });
      const changes = {
        display_name: next.displayName.trim() || null,
        strategy: next.strategy || null,
        threshold: percent == null ? null : percent / 100,
        keep_recent_tokens: positive("Recent tokens kept", next.keepRecentTokens),
        tool_result_max_chars: positive("Tool result cap", next.toolResultMaxChars),
        summary_max_output_tokens: positive(
          "Summary length",
          next.summaryMaxOutputTokens,
        ),
        summary_file_list_max: positive("Files listed", next.summaryFileListMax),
        inference_profile_id: next.profileId || null,
        tags: next.tags.length ? next.tags : null,
      };
      if (draftMode) {
        await changeConfig("applyConfigComponents", {
          document: {
            node: { node_did: deployment.nodeDid },
            compactions: [
              {
                compaction_id: compaction.compaction_id,
                node_did: deployment.nodeDid,
                ...changes,
              },
            ],
          },
        });
      } else {
        await changeConfig("patchConfigComponents", {
          nodeDid: deployment.nodeDid,
          patches: [
            { collection: "Compaction", id: compaction.compaction_id, changes },
          ],
        });
      }
    },
    {
      isNew: draftMode !== undefined,
      problems: (next) => ({
        threshold: problemOf(() =>
          optionalNumber("Threshold", next.threshold, { min: 1, max: 100 }),
        ),
        keepRecentTokens: problemOf(() =>
          positive("Recent tokens kept", next.keepRecentTokens),
        ),
        toolResultMaxChars: problemOf(() =>
          positive("Tool result cap", next.toolResultMaxChars),
        ),
        summaryMaxOutputTokens: problemOf(() =>
          positive("Summary length", next.summaryMaxOutputTokens),
        ),
        summaryFileListMax: problemOf(() =>
          positive("Files listed", next.summaryFileListMax),
        ),
      }),
    },
  );
  const id = (f: string) => `${compaction.compaction_id}-${f}`;
  const summarizes =
    (d.draft.strategy || COMPACTION_DEFAULTS.strategy) === "StripThenSummarize";
  const runtime = (value: string | number) => `Runtime default (${value})`;
  return (
    <>
      <Group>
        <TextRow
          id={id("name")}
          label="Name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          placeholder={compaction.compaction_id}
        />
        <ChoiceRow
          id={id("strategy")}
          label="Strategy"
          description="Trimming drops old tool output; summarizing also condenses the rest of the conversation."
          value={d.draft.strategy}
          onChange={(v) => d.set("strategy", v as Strategy | "")}
          items={[
            {
              value: "",
              label: runtime(
                STRATEGIES.find((s) => s.value === COMPACTION_DEFAULTS.strategy)!.label,
              ),
            },
            ...STRATEGIES,
          ]}
        />
        <NumberRow
          id={id("threshold")}
          label="Threshold"
          description="Compacts once a session fills this percent of the model's context window."
          value={d.draft.threshold}
          onChange={(v) => d.set("threshold", v)}
          error={d.problems.threshold}
          placeholder={runtime(`${COMPACTION_DEFAULTS.threshold * 100}%`)}
        />
        <NumberRow
          id={id("keep")}
          label="Recent tokens kept"
          description="The latest part of the conversation, kept as it is."
          value={d.draft.keepRecentTokens}
          onChange={(v) => d.set("keepRecentTokens", v)}
          error={d.problems.keepRecentTokens}
          placeholder={runtime(COMPACTION_DEFAULTS.keepRecentTokens)}
        />
        <NumberRow
          id={id("toolmax")}
          label="Tool result cap"
          description="Characters of an older tool result kept after trimming."
          value={d.draft.toolResultMaxChars}
          onChange={(v) => d.set("toolResultMaxChars", v)}
          error={d.problems.toolResultMaxChars}
          placeholder={runtime(COMPACTION_DEFAULTS.toolResultMaxChars)}
        />
      </Group>
      {summarizes && (
        <Group title="Summary">
          <ChoiceRow
            id={id("profile")}
            label="Written by"
            description="The model that writes the summary."
            value={d.draft.profileId}
            onChange={(v) => d.set("profileId", v)}
            items={[
              { value: "", label: "The agent's own model" },
              ...deployment.inferenceProfiles.map((p) => ({
                value: p.profile_id,
                label: profileLabel(deployment, p.profile_id),
              })),
            ]}
          />
          <NumberRow
            id={id("summarymax")}
            label="Summary length"
            description="Most tokens the summary may take."
            value={d.draft.summaryMaxOutputTokens}
            onChange={(v) => d.set("summaryMaxOutputTokens", v)}
            error={d.problems.summaryMaxOutputTokens}
            placeholder={runtime(COMPACTION_DEFAULTS.summaryMaxOutputTokens)}
          />
          <NumberRow
            id={id("files")}
            label="Files listed"
            description="Most files the summary names as touched."
            value={d.draft.summaryFileListMax}
            onChange={(v) => d.set("summaryFileListMax", v)}
            error={d.problems.summaryFileListMax}
            placeholder={runtime(COMPACTION_DEFAULTS.summaryFileListMax)}
          />
        </Group>
      )}
      <Group>
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        draft={d}
        fields={{
          threshold: id("threshold"),
          keepRecentTokens: id("keep"),
          toolResultMaxChars: id("toolmax"),
          summaryMaxOutputTokens: id("summarymax"),
          summaryFileListMax: id("files"),
        }}
        saveLabel={draftMode ? "Create" : undefined}
        onSave={() => {
          if (draftMode) {
            void d
              .save()
              .then((ok) => ok && draftMode.onSaved(compaction.compaction_id));
            return;
          }
          void d.save();
        }}
        onCancel={() => (draftMode ? draftMode.onCancel() : d.reset())}
      />
    </>
  );
}

/** A compaction to create or edit beside the field that picks it. */
export function CompactionSheet({
  deployment,
  editing,
  creating,
  onClose,
}: {
  deployment: NodeView;
  /** the compaction to edit, or null */
  editing: string | null;
  creating: boolean;
  onClose: (createdId: string | null) => void;
}) {
  const [fresh, setFresh] = useState(() => newCompactionDocument(deployment));
  const close = (id: string | null) => {
    onClose(id);
    setFresh(newCompactionDocument(deployment));
  };
  const existing = editing
    ? deployment.compactions.find((c) => c.compaction_id === editing)
    : undefined;
  return (
    <EditorSheet
      open={creating || existing !== undefined}
      onClose={() => close(null)}
      title={
        creating
          ? "New compaction"
          : (existing?.display_name ?? existing?.compaction_id ?? "Compaction")
      }
      description={
        creating
          ? "How a long session is shortened so it keeps fitting the model."
          : compactionInWords(deployment, existing)
      }
    >
      {creating ? (
        <CompactionEditor
          key={fresh.compaction_id}
          deployment={deployment}
          compaction={fresh}
          draft={{ onSaved: (id) => close(id), onCancel: () => close(null) }}
        />
      ) : existing ? (
        <CompactionEditor
          key={existing.compaction_id}
          deployment={deployment}
          compaction={existing}
        />
      ) : null}
    </EditorSheet>
  );
}
