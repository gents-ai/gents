import { useEffect, useMemo, useState } from "react";

import type {
  BehaviorEnvironmentView,
  SessionSummary,
} from "@source-inc/gents-desktop-client";
import { displaySessionTitle } from "../../lib/sessionDisplay";
import { formatRelativeTime } from "@source-inc/gents-desktop-fleet";
import {
  sessionLifecycleGroup,
  sessionStatusClass,
  type SessionLifecycleGroup,
} from "./sidebarUtils";

export type SessionListSectionProps = {
  sessions: SessionSummary[];
  environments: BehaviorEnvironmentView[];
  selectedAgentDid: string | null;
  selectedSessionId: string | null;
  onSelectSession: (sessionId: string) => void;
  onOpenSession?: (sessionId: string) => void;
  onCreateSession: () => void;
};

export function SessionListSection({
  sessions,
  environments,
  selectedAgentDid,
  selectedSessionId,
  onSelectSession,
  onOpenSession,
  onCreateSession,
}: SessionListSectionProps) {
  const [query, setQuery] = useState("");

  useEffect(() => setQuery(""), [selectedAgentDid]);

  const environmentById = useMemo(
    () =>
      new Map(environments.map((environment) => [environment.behaviorId, environment])),
    [environments],
  );
  const filteredSessions = useMemo(() => {
    const needle = query.trim().toLowerCase();
    if (!needle) return sessions;
    return sessions.filter((session) => {
      const environment = session.behaviorId
        ? environmentById.get(session.behaviorId)
        : undefined;
      return `${displaySessionTitle(session.title)} ${session.previewText ?? ""} ${environment?.displayName ?? session.behaviorId ?? ""}`
        .toLowerCase()
        .includes(needle);
    });
  }, [sessions, environmentById, query]);
  const grouped = useMemo(() => {
    const groups: Record<SessionLifecycleGroup, SessionSummary[]> = {
      attention: [],
      active: [],
      recent: [],
    };
    for (const session of filteredSessions) {
      groups[sessionLifecycleGroup(session)].push(session);
    }
    return groups;
  }, [filteredSessions]);

  return (
    <section className="sidebar-section session-section">
      <div className="session-section-header">
        <h2>Sessions</h2>
        <button
          className="primary-button session-new-button"
          data-testid="session-new"
          disabled={!selectedAgentDid || !environments.some((item) => item.enabled)}
          onClick={onCreateSession}
          type="button"
        >
          New session
        </button>
      </div>
      {selectedAgentDid && sessions.length > 0 ? (
        <input
          aria-label="Search sessions"
          className="session-search"
          data-testid="session-search"
          onChange={(event) => setQuery(event.currentTarget.value)}
          placeholder="Search sessions"
          type="search"
          value={query}
        />
      ) : null}
      {!selectedAgentDid ? (
        <p className="muted">Select an agent to see sessions.</p>
      ) : !sessions.length ? (
        <p className="muted">No sessions yet. Choose an environment to start one.</p>
      ) : !filteredSessions.length ? (
        <p className="muted">No sessions match the search.</p>
      ) : (
        <div
          className="session-list"
          data-scroll-owner="section-list"
          data-testid="session-list"
        >
          <SessionGroupList
            sessions={grouped.attention}
            environmentById={environmentById}
            label="Needs attention"
            onOpenSession={onOpenSession ?? onSelectSession}
            selectedSessionId={selectedSessionId}
          />
          <SessionGroupList
            sessions={grouped.active}
            environmentById={environmentById}
            label="Active"
            onOpenSession={onOpenSession ?? onSelectSession}
            selectedSessionId={selectedSessionId}
          />
          <SessionGroupList
            sessions={grouped.recent}
            environmentById={environmentById}
            label="Recent"
            onOpenSession={onOpenSession ?? onSelectSession}
            selectedSessionId={selectedSessionId}
          />
        </div>
      )}
    </section>
  );
}

function SessionGroupList({
  sessions,
  environmentById,
  label,
  onOpenSession,
  selectedSessionId,
}: {
  sessions: SessionSummary[];
  environmentById: Map<string, BehaviorEnvironmentView>;
  label: string;
  onOpenSession: (sessionId: string) => void;
  selectedSessionId: string | null;
}) {
  if (!sessions.length) return null;
  return (
    <section className="session-group">
      <h3>{label}</h3>
      <div className="session-group-list">
        {sessions.map((session) => {
          const when = session.updatedAt ?? session.createdAt;
          const environment = session.behaviorId
            ? environmentById.get(session.behaviorId)
            : undefined;
          const statusClass = sessionStatusClass(session);
          const lifecycle = sessionLifecycleGroup(session);
          const title = displaySessionTitle(session.title);
          return (
            <button
              aria-label={`${title}, ${sessionGroupLabel(lifecycle)}, ${environment?.displayName ?? "unassigned behavior"}`}
              className={
                session.sessionId === selectedSessionId
                  ? "list-item session-list-item selected"
                  : "list-item session-list-item"
              }
              data-testid={`session-${session.sessionId}`}
              key={session.sessionId}
              onClick={() => onOpenSession(session.sessionId)}
              type="button"
            >
              <span className="session-list-row">
                {statusClass ? (
                  <span aria-hidden="true" className={statusClass} />
                ) : null}
                <span
                  className={
                    session.title
                      ? "list-item-title session-list-title"
                      : "list-item-title session-list-title untitled-title"
                  }
                >
                  {title}
                </span>
                {when ? (
                  <span className="session-time" title={when}>
                    {formatRelativeTime(when)}
                  </span>
                ) : null}
              </span>
              <span className="session-environment-line">
                {environment?.displayName ??
                  session.behaviorId ??
                  "Unassigned behavior"}
                {environment?.workspaceRoot ? (
                  <>
                    <span aria-hidden="true"> · </span>
                    <span className="mono">
                      {workspaceName(environment.workspaceRoot)}
                    </span>
                  </>
                ) : null}
              </span>
              {session.previewText ? (
                <span className="session-preview">{session.previewText}</span>
              ) : null}
              {session.taskId ? (
                <span
                  className="session-task-tag"
                  title={displaySessionTaskLabel(session)}
                >
                  {displaySessionTaskLabel(session)}
                </span>
              ) : null}
            </button>
          );
        })}
      </div>
    </section>
  );
}

function workspaceName(root: string) {
  const parts = root.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? root;
}

function sessionGroupLabel(group: SessionLifecycleGroup) {
  return group === "attention" ? "needs attention" : group;
}

function displaySessionTaskLabel(session: SessionSummary) {
  const name = session.taskName?.trim();
  return name && name.length > 0 ? name : (session.taskId ?? "Task");
}
