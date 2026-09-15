import type { DeploymentView, EventSource } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  TextRow,
} from "./editors";
import {
  fromLinesOrNull,
  newId,
  optionalInteger,
  optionalGraphqlFilter,
  requiredGraphqlCollection,
  requiredGraphqlName,
  str,
  toLines,
  useDraft,
} from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";

function Editor({
  shell,
  deployment,
  source,
}: {
  shell: Shell;
  deployment: DeploymentView;
  source: EventSource;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "event-sources",
  };
  const saved = {
    displayName: source.display_name ?? "",
    sourceCollection: source.source_collection,
    eventKind: source.event_kind ?? "",
    filter: source.filter ?? "",
    correlationField: source.correlation_field ?? "",
    expectedCount:
      typeof source.group?.expected_count === "number"
        ? str(source.group.expected_count)
        : "",
    expectedCountField:
      source.group?.expected_count && typeof source.group.expected_count === "object"
        ? source.group.expected_count.source_field
        : "",
    timeoutSecs: str(source.group?.timeout_secs),
    minCount: str(source.group?.min_count),
    workspaceAuthority: source.workspace_authority ?? "",
    tags: toLines(source.tags ?? []),
  };
  const d = useDraft(saved, async (next) => {
    const sourceCollection = requiredGraphqlCollection(
      "Source collection",
      next.sourceCollection,
    );
    const eventKind = next.eventKind.trim() || "created";
    if (eventKind !== "created")
      throw new Error("Event kind currently supports only created");
    const filter = optionalGraphqlFilter("Filter", next.filter);
    const correlationField = next.correlationField.trim()
      ? requiredGraphqlName("Correlation field", next.correlationField)
      : null;
    const expectedCountField = next.expectedCountField.trim()
      ? requiredGraphqlName("Expected count source field", next.expectedCountField)
      : null;
    const expectedCount = optionalInteger("Expected count", next.expectedCount, {
      min: 1,
      max: 256,
    });
    const timeoutSecs = optionalInteger("Group timeout", next.timeoutSecs, { min: 1 });
    const minCount = optionalInteger("Minimum count", next.minCount, {
      min: 1,
      max: 256,
    });
    if (expectedCount != null && expectedCountField)
      throw new Error("Choose a fixed expected count or a source field, not both");
    const grouped =
      expectedCount != null ||
      Boolean(expectedCountField) ||
      timeoutSecs != null ||
      minCount != null;
    if (grouped && !correlationField)
      throw new Error("Grouped events require a correlation field");
    if (grouped && expectedCount == null && !expectedCountField && timeoutSecs == null)
      throw new Error("Grouped events require an expected count or timeout");
    if (expectedCount != null && minCount != null && minCount > expectedCount)
      throw new Error("Minimum count cannot exceed expected count");
    await shell.applyConfig((api) =>
      api.saveEventSourceConfig({
        document: {
          ...source,
          display_name: next.displayName.trim() || null,
          source_collection: sourceCollection,
          event_kind: eventKind,
          filter,
          correlation_field: correlationField,
          group: grouped
            ? {
                expected_count:
                  expectedCount ??
                  (expectedCountField ? { source_field: expectedCountField } : null),
                timeout_secs: timeoutSecs,
                min_count: minCount,
              }
            : null,
          workspace_authority: (next.workspaceAuthority || null) as
            "readOnly" | "readWrite" | "integrate" | null,
          tags: fromLinesOrNull(next.tags),
        },
      }),
    );
  });
  const id = (f: string) => `${source.event_source_id}-${f}`;
  return (
    <>
      <Group title={source.display_name ?? source.event_source_id}>
        <FactRow label="Event source ID" mono>
          {source.event_source_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("collection")}
          label="Source collection"
          value={d.draft.sourceCollection}
          onChange={(v) => d.set("sourceCollection", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("kind")}
          label="Event kind"
          value={d.draft.eventKind}
          onChange={(v) => d.set("eventKind", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("filter")}
          label="Filter"
          value={d.draft.filter}
          onChange={(v) => d.set("filter", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("correlation")}
          label="Correlation field"
          description="Required when events are grouped."
          value={d.draft.correlationField}
          onChange={(v) => d.set("correlationField", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <ChoiceRow
          id={id("authority")}
          label="Workspace authority"
          value={d.draft.workspaceAuthority}
          onChange={(v) => d.choose("workspaceAuthority", v)}
          items={[
            { value: "readOnly", label: "Read only" },
            { value: "readWrite", label: "Read / write" },
            { value: "integrate", label: "Integrate (inspect only)" },
          ]}
          none="None"
        />
      </Group>
      <Group title="Grouping">
        <NumberRow
          id={id("expected-count")}
          label="Expected count"
          value={d.draft.expectedCount}
          onChange={(v) => d.set("expectedCount", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <TextRow
          id={id("expected-field")}
          label="Expected count source field"
          value={d.draft.expectedCountField}
          onChange={(v) => d.set("expectedCountField", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("timeout")}
          label="Timeout seconds"
          value={d.draft.timeoutSecs}
          onChange={(v) => d.set("timeoutSecs", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
        <NumberRow
          id={id("min-count")}
          label="Minimum count"
          value={d.draft.minCount}
          onChange={(v) => d.set("minCount", v)}
          onCommit={d.commit}
          onEnter={d.onEnter}
        />
      </Group>
      <Group title="Metadata">
        <AreaRow
          id={id("tags")}
          label="Tags"
          description="One per line."
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
          onCommit={d.commit}
          rows={3}
        />
      </Group>
      <DraftActions
        dirty={d.dirty}
        saving={d.saving}
        error={d.error}
        onSave={d.save}
        onCancel={d.reset}
      />
      <DeleteButton
        label={source.display_name ?? source.event_source_id}
        base={base}
        onDelete={() =>
          shell.applyConfig((api) =>
            api.deleteEventSourceConfig({
              eventSourceId: source.event_source_id,
              agentDid: deployment.agentDid,
            }),
          )
        }
      />
    </>
  );
}

export function EventSourcesPanel({
  shell,
  deployment,
  item,
}: {
  shell: Shell;
  deployment: DeploymentView;
  item?: string;
}) {
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "event-sources",
  };
  return (
    <ListDetail
      base={base}
      item={item}
      rows={deployment.eventSources.map((s) => ({
        id: s.event_source_id,
        title: s.display_name ?? s.event_source_id,
        meta: s.source_collection,
        tags: s.tags,
      }))}
      createLabel="New event source"
      empty="No event sources. A trigger binds a task to a reusable source."
      onCreate={async () => {
        const event_source_id = newId("evsrc");
        await shell.applyConfig((api) =>
          api.saveEventSourceConfig({
            document: {
              agent_did: deployment.agentDid,
              event_source_id,
              display_name: "New event source",
              source_collection: "AgentRequest",
              event_kind: "created",
            },
          }),
        );
        navigate({
          name: "agent",
          agentDid: deployment.agentDid,
          section: "event-sources",
          item: event_source_id,
        });
      }}
      detail={(id) => {
        const source = deployment.eventSources.find((s) => s.event_source_id === id)!;
        return (
          <Editor
            key={source.event_source_id}
            shell={shell}
            deployment={deployment}
            source={source}
          />
        );
      }}
    />
  );
}
