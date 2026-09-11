import { useEffect, useMemo, useState } from "react";
import type { FormEvent } from "react";

import type {
  DeploymentView,
  EventSource,
  EventSourceDeleteRequest,
  EventSourceSaveRequest,
} from "@source-inc/gents-desktop-client";
import { ConfirmDialog } from "@source-inc/gents-desktop-ui";
import { isDirty } from "./configDirty";
import { ConfigDocumentList, ConfigEditorHeader, FieldHint } from "./ConfigChrome";
import { isOptionalInt, linesToArray, parseOptionalInt } from "./formUtils";

export type EventSourceConfigPanelProps = {
  deployment: DeploymentView;
  selectedEventSourceId: string | null;
  saving: boolean;
  savedStatus: string | null;
  onSelectEventSource: (eventSourceId: string) => void;
  onCreateEventSource: () => void;
  onSavedStatusChange: (value: string) => void;
  onSaveEventSourceConfig: (request: EventSourceSaveRequest) => Promise<unknown>;
  onDeleteEventSourceConfig: (request: EventSourceDeleteRequest) => Promise<unknown>;
  onDeletedEventSource: () => void;
};

export function EventSourceConfigPanel({
  deployment,
  selectedEventSourceId,
  saving,
  savedStatus,
  onSelectEventSource,
  onCreateEventSource,
  onSavedStatusChange,
  onSaveEventSourceConfig,
  onDeleteEventSourceConfig,
  onDeletedEventSource,
}: EventSourceConfigPanelProps) {
  const selectedEventSource = useMemo(
    () =>
      deployment.eventSources.find(
        (source) => source.event_source_id === selectedEventSourceId,
      ) ?? null,
    [deployment.eventSources, selectedEventSourceId],
  );

  return (
    <section className="config-layout">
      <ConfigDocumentList
        eyebrow="Automation"
        items={deployment.eventSources.map((source) => ({
          id: source.event_source_id,
          title: source.display_name ?? source.event_source_id,
          meta: `${source.source_collection} / ${source.event_kind ?? "created"}`,
        }))}
        selectedId={selectedEventSourceId}
        testPrefix="event-source"
        title="Event Sources"
        onCreate={onCreateEventSource}
        onSelect={onSelectEventSource}
      />

      <EventSourceConfigEditor
        agentDid={deployment.agentDid}
        eventSource={selectedEventSource}
        savedStatus={savedStatus}
        saving={saving}
        onSaved={(eventSourceId) => {
          onSelectEventSource(eventSourceId);
          onSavedStatusChange(`event-source:${eventSourceId}`);
        }}
        onSaveEventSourceConfig={onSaveEventSourceConfig}
        onDeleteEventSourceConfig={onDeleteEventSourceConfig}
        onDeleted={() => {
          onDeletedEventSource();
        }}
      />
    </section>
  );
}

export type EventSourceConfigEditorProps = {
  agentDid: string;
  eventSource: EventSource | null;
  savedStatus: string | null;
  saving: boolean;
  onSaved: (eventSourceId: string) => void;
  onSaveEventSourceConfig: (request: EventSourceSaveRequest) => Promise<unknown>;
  onDeleteEventSourceConfig: (request: EventSourceDeleteRequest) => Promise<unknown>;
  onDeleted: () => void;
};

export function EventSourceConfigEditor({
  agentDid,
  eventSource,
  savedStatus,
  saving,
  onSaved,
  onSaveEventSourceConfig,
  onDeleteEventSourceConfig,
  onDeleted,
}: EventSourceConfigEditorProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);

  async function deleteEventSource() {
    setConfirmingDelete(false);
    if (!eventSource) {
      return;
    }
    try {
      await onDeleteEventSourceConfig({
        eventSourceId: eventSource.event_source_id,
        agentDid,
      });
      onDeleted();
    } catch {}
  }

  const [eventSourceId, setEventSourceId] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [sourceCollection, setSourceCollection] = useState("AgentRequest");
  const [eventKind, setEventKind] = useState("created");
  const [filter, setFilter] = useState("");
  const [correlationField, setCorrelationField] = useState("");
  const [groupExpectedCount, setGroupExpectedCount] = useState("");
  const [groupTimeoutSecs, setGroupTimeoutSecs] = useState("");
  const [groupMinCount, setGroupMinCount] = useState("");
  const [workspaceAuthority, setWorkspaceAuthority] = useState("none");
  const [tags, setTags] = useState("");

  const [saveError, setSaveError] = useState<string | null>(null);

  useEffect(() => {
    const baseline = eventSourceFormValues(eventSource);
    setEventSourceId(baseline.eventSourceId);
    setDisplayName(baseline.displayName);
    setSourceCollection(baseline.sourceCollection);
    setEventKind(baseline.eventKind);
    setFilter(baseline.filter);
    setCorrelationField(baseline.correlationField);
    setGroupExpectedCount(baseline.groupExpectedCount);
    setGroupTimeoutSecs(baseline.groupTimeoutSecs);
    setGroupMinCount(baseline.groupMinCount);
    setWorkspaceAuthority(baseline.workspaceAuthority);
    setTags(baseline.tags);
    setSaveError(null);
  }, [eventSource?.event_source_id, eventSource?.updated_at]);

  const groupEnabled =
    groupExpectedCount.trim() !== "" || groupTimeoutSecs.trim() !== "";
  const groupValid =
    !groupEnabled ||
    (isOptionalInt(groupExpectedCount, { min: 1 }) &&
      isOptionalInt(groupTimeoutSecs, { min: 1 }) &&
      isOptionalInt(groupMinCount, { min: 1 }));

  async function submitEventSource(event: FormEvent) {
    event.preventDefault();
    if (!eventSourceId.trim() || !sourceCollection.trim()) {
      return;
    }
    const document: EventSource = {
      agent_did: agentDid,
      event_source_id: eventSourceId.trim(),
      display_name: optionalTrimmed(displayName),
      source_collection: sourceCollection.trim(),
      event_kind: optionalTrimmed(eventKind),
      filter: optionalTrimmed(filter),
      correlation_field: optionalTrimmed(correlationField),
      group: groupEnabled
        ? {
            expected_count:
              parseOptionalInt(groupExpectedCount) != null
                ? parseOptionalInt(groupExpectedCount)!
                : null,
            timeout_secs:
              parseOptionalInt(groupTimeoutSecs) != null
                ? parseOptionalInt(groupTimeoutSecs)!
                : null,
            min_count:
              parseOptionalInt(groupMinCount) != null
                ? parseOptionalInt(groupMinCount)!
                : null,
          }
        : null,
      workspace_authority:
        workspaceAuthority === "none"
          ? null
          : (workspaceAuthority as "readOnly" | "readWrite" | "integrate"),
      tags: linesToArray(tags),
    };
    try {
      await onSaveEventSourceConfig({ document });
      onSaved(document.event_source_id);
      setSaveError(null);
    } catch (error) {
      setSaveError(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <form className="panel config-editor" onSubmit={submitEventSource}>
      <ConfigEditorHeader
        dirty={isDirty(
          {
            eventSourceId,
            displayName,
            sourceCollection,
            eventKind,
            filter,
            correlationField,
            groupExpectedCount,
            groupTimeoutSecs,
            groupMinCount,
            workspaceAuthority,
            tags,
          },
          eventSourceFormValues(eventSource),
        )}
        eyebrow="Event Source"
        saved={savedStatus === `event-source:${eventSourceId.trim()}`}
        title={displayName || eventSourceId || "New Event Source"}
      />
      {saveError ? <FieldHint show>Save failed: {saveError}</FieldHint> : null}
      <div className="grid-2">
        <label className="field">
          <span>Event source ID</span>
          <input
            data-testid="event-source-id"
            onChange={(event) => {
              if (!eventSource) {
                setEventSourceId(event.currentTarget.value);
              }
            }}
            readOnly={Boolean(eventSource)}
            title={
              eventSource
                ? "Event source IDs cannot be renamed after creation."
                : undefined
            }
            value={eventSourceId}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input
            data-testid="event-source-display-name"
            onChange={(event) => setDisplayName(event.currentTarget.value)}
            value={displayName}
          />
        </label>
      </div>
      <div className="grid-3">
        <label className="field">
          <span>Source collection</span>
          <input
            data-testid="event-source-source-collection"
            onChange={(event) => setSourceCollection(event.currentTarget.value)}
            value={sourceCollection}
          />
        </label>
        <label className="field">
          <span>Event kind</span>
          <select
            data-testid="event-source-event-kind"
            onChange={(event) => setEventKind(event.currentTarget.value)}
            value={eventKind}
          >
            <option value="created">Created</option>
          </select>
          <FieldHint show>Only the created event kind is supported.</FieldHint>
        </label>
        <label className="field">
          <span>Workspace authority</span>
          <select
            data-testid="event-source-workspace-authority"
            onChange={(event) => setWorkspaceAuthority(event.currentTarget.value)}
            value={workspaceAuthority}
          >
            <option value="none">None</option>
            <option value="readOnly">Read only</option>
            <option value="readWrite">Read write</option>
            <option value="integrate">Integrate</option>
          </select>
        </label>
      </div>
      <label className="field">
        <span>Filter</span>
        <textarea
          className="config-small-textarea"
          data-testid="event-source-filter"
          onChange={(event) => setFilter(event.currentTarget.value)}
          value={filter}
        />
      </label>
      <label className="field">
        <span>Correlation field</span>
        <input
          data-testid="event-source-correlation-field"
          onChange={(event) => setCorrelationField(event.currentTarget.value)}
          value={correlationField}
        />
      </label>
      <div className="grid-3">
        <label className="field">
          <span>Group expected count</span>
          <input
            data-testid="event-source-group-expected-count"
            onChange={(event) => setGroupExpectedCount(event.currentTarget.value)}
            type="number"
            value={groupExpectedCount}
          />
        </label>
        <label className="field">
          <span>Group timeout seconds</span>
          <input
            data-testid="event-source-group-timeout-secs"
            onChange={(event) => setGroupTimeoutSecs(event.currentTarget.value)}
            type="number"
            value={groupTimeoutSecs}
          />
        </label>
        <label className="field">
          <span>Group minimum count</span>
          <input
            data-testid="event-source-group-min-count"
            onChange={(event) => setGroupMinCount(event.currentTarget.value)}
            type="number"
            value={groupMinCount}
          />
        </label>
      </div>
      {groupEnabled && !groupValid ? (
        <FieldHint show>Group counts and timeouts must be 1 or more</FieldHint>
      ) : null}
      <label className="field">
        <span>Tags</span>
        <textarea
          className="config-small-textarea"
          data-testid="event-source-tags"
          onChange={(event) => setTags(event.currentTarget.value)}
          placeholder="One tag per line"
          value={tags}
        />
      </label>
      {eventSource ? (
        <div className="facts">
          <div>
            <dt>Created</dt>
            <dd>{eventSource.created_at ?? "unknown"}</dd>
          </div>
          <div>
            <dt>Updated</dt>
            <dd>{eventSource.updated_at ?? "unknown"}</dd>
          </div>
        </div>
      ) : null}
      <div className="config-actions">
        {eventSource ? (
          <button
            className="ghost-button danger-button"
            data-testid="event-source-delete"
            disabled={saving}
            onClick={() => setConfirmingDelete(true)}
            type="button"
          >
            Delete Event Source
          </button>
        ) : null}
        <ConfirmDialog
          open={confirmingDelete}
          title="Delete event source"
          message={`Delete event source "${eventSource?.event_source_id ?? ""}"? Triggers referencing it stop firing.`}
          confirmLabel="Delete Event Source"
          danger
          onConfirm={() => {
            void deleteEventSource();
          }}
          onCancel={() => setConfirmingDelete(false)}
        />
        <button
          className="primary-button"
          data-testid="event-source-save"
          disabled={
            saving || !eventSourceId.trim() || !sourceCollection.trim() || !groupValid
          }
          type="submit"
        >
          {saving ? "Saving..." : "Save Event Source"}
        </button>
      </div>
    </form>
  );
}

function eventSourceFormValues(eventSource: EventSource | null) {
  const group = eventSource?.group ?? null;
  return {
    eventSourceId: eventSource?.event_source_id ?? "",
    displayName: eventSource?.display_name ?? "",
    sourceCollection: eventSource?.source_collection ?? "AgentRequest",
    eventKind: eventSource?.event_kind ?? "created",
    filter: eventSource?.filter ?? "",
    correlationField: eventSource?.correlation_field ?? "",
    groupExpectedCount:
      group?.expected_count != null && typeof group.expected_count === "number"
        ? String(group.expected_count)
        : "",
    groupTimeoutSecs: group?.timeout_secs != null ? String(group.timeout_secs) : "",
    groupMinCount: group?.min_count != null ? String(group.min_count) : "",
    workspaceAuthority: eventSource?.workspace_authority ?? "none",
    tags: eventSource?.tags?.length ? eventSource.tags.join("\n") : "",
  };
}

function optionalTrimmed(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}
