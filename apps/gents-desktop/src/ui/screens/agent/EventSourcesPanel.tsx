import type { DeploymentView, EventSource } from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";
import { navigate } from "@/lib/router";
import { FactRow, TextRow } from "./editors";
import { newId, useDraft } from "./draft";
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
  };
  const d = useDraft(saved, (next) =>
    shell.applyConfig((api) =>
      api.saveEventSourceConfig({
        document: {
          ...source,
          display_name: next.displayName || null,
          source_collection: next.sourceCollection,
          event_kind: next.eventKind || null,
          filter: next.filter || null,
        },
      }),
    ),
  );
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
      </Group>
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
            key={JSON.stringify(source)}
            shell={shell}
            deployment={deployment}
            source={source}
          />
        );
      }}
    />
  );
}
