import type { NodeView } from "../../../hooks/fleetStore";
import type { EventSource } from "@source-inc/gents-desktop-client";
import { dependentsWarning } from "./dependents";
import { navigate } from "@/lib/router";
import {
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  TextRow,
  TagsRow,
} from "./editors";
import {
  newId,
  optionalInteger,
  optionalGraphqlFilter,
  requiredGraphqlCollection,
  requiredGraphqlName,
  str,
  useDraft,
  problemOf,
  type Problems,
} from "./draft";
import { DeleteButton, ListDetail } from "./ListDetail";
import { Group } from "./rows";
import { RowMenu } from "./RowMenu";
import { useApp } from "@/app/AppContext";

type EventSourceDraft = {
  displayName: string;
  sourceCollection: string;
  eventKind: string;
  filter: string;
  correlationField: string;
  expectedCount: string;
  expectedCountField: string;
  timeoutSecs: string;
  minCount: string;
  workspaceAuthority: string;
  tags: string[];
};

/* each field's value as the document takes it; each throws its problem */
function eventSourceFields(next: EventSourceDraft) {
  return {
    sourceCollection: () =>
      requiredGraphqlCollection("Source collection", next.sourceCollection),
    eventKind: () => {
      const kind = next.eventKind.trim() || "created";
      if (kind !== "created")
        throw new Error("Event kind currently supports only created");
      return kind;
    },
    filter: () => optionalGraphqlFilter("Filter", next.filter),
    correlationField: () =>
      next.correlationField.trim()
        ? requiredGraphqlName("Correlation field", next.correlationField)
        : null,
    expectedCountField: () =>
      next.expectedCountField.trim()
        ? requiredGraphqlName("Expected count source field", next.expectedCountField)
        : null,
    expectedCount: () =>
      optionalInteger("Expected count", next.expectedCount, { min: 1, max: 256 }),
    timeoutSecs: () => optionalInteger("Timeout seconds", next.timeoutSecs, { min: 1 }),
    minCount: () =>
      optionalInteger("Minimum count", next.minCount, { min: 1, max: 256 }),
  };
}

/**
 * What is wrong with an event source draft, at its fields: each field's
 * own parse, then the grouping rules, each at the field that resolves it.
 */
export function eventSourceProblems(
  next: EventSourceDraft,
): Problems<EventSourceDraft> {
  const fields = eventSourceFields(next);
  const out: Problems<EventSourceDraft> = {};
  for (const field of Object.keys(fields) as (keyof typeof fields)[])
    out[field] = problemOf(fields[field]);
  /* the rules read only fields that parsed */
  const value = <K extends keyof typeof fields>(field: K) =>
    out[field] ? undefined : (fields[field]() as ReturnType<(typeof fields)[K]>);
  const expectedCount = value("expectedCount");
  const expectedCountField = value("expectedCountField");
  const timeoutSecs = value("timeoutSecs");
  const minCount = value("minCount");
  if (expectedCount != null && expectedCountField)
    out.expectedCountField ??=
      "Choose a fixed expected count or a source field, not both";
  const grouped =
    expectedCount != null ||
    Boolean(expectedCountField) ||
    timeoutSecs != null ||
    minCount != null;
  if (grouped && !next.correlationField.trim())
    out.correlationField ??= "Grouped events require a correlation field";
  if (grouped && expectedCount == null && !expectedCountField && timeoutSecs == null)
    out.expectedCount ??= "Grouped events require an expected count or timeout";
  if (expectedCount != null && minCount != null && minCount > expectedCount)
    out.minCount ??= "Minimum count cannot exceed expected count";
  return out;
}

export function EventSourceEditor({
  deployment,
  source,
  embedded = false,
}: {
  deployment: NodeView;
  source: EventSource;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
    tags: source.tags ?? [],
  };
  const d = useDraft(
    saved,
    async (next) => {
      const value = eventSourceFields(next);
      const sourceCollection = value.sourceCollection();
      const eventKind = value.eventKind();
      const filter = value.filter();
      const correlationField = value.correlationField();
      const expectedCountField = value.expectedCountField();
      const expectedCount = value.expectedCount();
      const timeoutSecs = value.timeoutSecs();
      const minCount = value.minCount();
      const grouped =
        expectedCount != null ||
        Boolean(expectedCountField) ||
        timeoutSecs != null ||
        minCount != null;
      await changeConfig("saveEventSourceConfig", {
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
          tags: next.tags.length ? next.tags : null,
        },
      });
    },
    { problems: eventSourceProblems },
  );
  const id = (f: string) => `${source.event_source_id}-${f}`;
  return (
    <>
      <Group
        title={embedded ? undefined : (source.display_name ?? source.event_source_id)}
      >
        <FactRow label="Event source ID" mono>
          {source.event_source_id}
        </FactRow>
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
        />
        <TextRow
          id={id("collection")}
          label="Source collection"
          value={d.draft.sourceCollection}
          error={d.problems.sourceCollection}
          onChange={(v) => d.set("sourceCollection", v)}
        />
        <TextRow
          id={id("kind")}
          label="Event kind"
          value={d.draft.eventKind}
          error={d.problems.eventKind}
          onChange={(v) => d.set("eventKind", v)}
        />
        <TextRow
          id={id("filter")}
          label="Filter"
          value={d.draft.filter}
          error={d.problems.filter}
          onChange={(v) => d.set("filter", v)}
        />
        <TextRow
          id={id("correlation")}
          label="Correlation field"
          description="Required when events are grouped."
          value={d.draft.correlationField}
          error={d.problems.correlationField}
          onChange={(v) => d.set("correlationField", v)}
        />
        <ChoiceRow
          id={id("authority")}
          label="Workspace authority"
          value={d.draft.workspaceAuthority}
          onChange={(v) => d.set("workspaceAuthority", v)}
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
          error={d.problems.expectedCount}
          onChange={(v) => d.set("expectedCount", v)}
        />
        <TextRow
          id={id("expected-field")}
          label="Expected count source field"
          value={d.draft.expectedCountField}
          error={d.problems.expectedCountField}
          onChange={(v) => d.set("expectedCountField", v)}
        />
        <NumberRow
          id={id("timeout")}
          label="Timeout seconds"
          value={d.draft.timeoutSecs}
          error={d.problems.timeoutSecs}
          onChange={(v) => d.set("timeoutSecs", v)}
        />
        <NumberRow
          id={id("min-count")}
          label="Minimum count"
          value={d.draft.minCount}
          error={d.problems.minCount}
          onChange={(v) => d.set("minCount", v)}
        />
      </Group>
      <Group title="Metadata">
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
          sourceCollection: id("collection"),
          eventKind: id("kind"),
          filter: id("filter"),
          correlationField: id("correlation"),
          expectedCount: id("expected-count"),
          expectedCountField: id("expected-field"),
          timeoutSecs: id("timeout"),
          minCount: id("min-count"),
        }}
      />
      {!embedded && (
        <DeleteButton
          label={source.display_name ?? source.event_source_id}
          warning={dependentsWarning(
            deployment,
            "event-source",
            source.event_source_id,
          )}
          base={base}
          onDelete={() =>
            changeConfig("deleteEventSourceConfig", {
              eventSourceId: source.event_source_id,
              nodeDid: deployment.nodeDid,
            })
          }
        />
      )}
    </>
  );
}

export function EventSourcesPanel({
  deployment,
  item,
}: {
  deployment: NodeView;
  item?: string;
}) {
  const { changeConfig } = useApp().actions;
  const base = {
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
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
        trailing: (
          <RowMenu
            name={s.display_name ?? s.event_source_id}
            base={base}
            id={s.event_source_id}
            onDuplicate={async () => {
              const event_source_id = newId("evsrc");
              await changeConfig("saveEventSourceConfig", {
                document: {
                  ...s,
                  event_source_id,
                  display_name: `${s.display_name ?? s.event_source_id} copy`,
                  created_at: null,
                  updated_at: null,
                },
              });
              return event_source_id;
            }}
            onDelete={() =>
              changeConfig("deleteEventSourceConfig", {
                eventSourceId: s.event_source_id,
                nodeDid: deployment.nodeDid,
              })
            }
            warning={dependentsWarning(deployment, "event-source", s.event_source_id)}
          />
        ),
      }))}
      createLabel="New event source"
      empty="No event sources. A trigger binds a task to a reusable source."
      onCreate={async () => {
        const event_source_id = newId("evsrc");
        await changeConfig("saveEventSourceConfig", {
          document: {
            node_did: deployment.nodeDid,
            event_source_id,
            display_name: "New event source",
            source_collection: "AgentRequest",
            event_kind: "created",
          },
        });
        navigate({
          name: "agent",
          nodeDid: deployment.nodeDid,
          section: "event-sources",
          item: event_source_id,
        });
      }}
      detail={(id) => {
        const source = deployment.eventSources.find((s) => s.event_source_id === id)!;
        return (
          <EventSourceEditor
            key={source.event_source_id}
            deployment={deployment}
            source={source}
          />
        );
      }}
    />
  );
}
