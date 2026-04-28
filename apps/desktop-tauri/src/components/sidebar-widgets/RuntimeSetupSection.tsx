import type { FormEvent } from "react";

import type { InitSummary, P2PHealth } from "../../lib/types";

export type RuntimeSetupSectionProps = {
  running: boolean;
  starting: boolean;
  stopping: boolean;
  runtimeHealth: P2PHealth | null;
  label: string;
  dangerouslyOverwrite: boolean;
  reset: boolean;
  initializing: boolean;
  initSummary: InitSummary | null;
  onLabelChange: (value: string) => void;
  onDangerouslyOverwriteChange: (value: boolean) => void;
  onResetChange: (value: boolean) => void;
  onRefresh: () => void;
  onShutdown: () => void;
  onStart: () => void;
  onInit: (event: FormEvent) => void;
};

export function RuntimeSetupSection({
  running,
  starting,
  stopping,
  runtimeHealth,
  label,
  dangerouslyOverwrite,
  reset,
  initializing,
  initSummary,
  onLabelChange,
  onDangerouslyOverwriteChange,
  onResetChange,
  onRefresh,
  onShutdown,
  onStart,
  onInit,
}: RuntimeSetupSectionProps) {
  return (
    <details className="sidebar-utility">
      <summary>Local Runtime Setup</summary>
      <div className="utility-actions">
        <button className="ghost-button" onClick={onRefresh} type="button">
          Refresh
        </button>
        {!running ? (
          <button
            className="primary-button"
            disabled={starting}
            onClick={onStart}
            type="button"
          >
            {starting ? "Starting…" : "Start Core"}
          </button>
        ) : (
          <button
            className="ghost-button"
            disabled={stopping}
            onClick={onShutdown}
            type="button"
          >
            {stopping ? "Stopping…" : "Shutdown Core"}
          </button>
        )}
      </div>
      <div className="utility-status">
        <span
          className={
            runtimeHealth?.status === "healthy" ? "chip chip-green" : "chip"
          }
        >
          {running ? runtimeHealth?.status ?? "running" : "stopped"}
        </span>
      </div>
      <form className="stack compact-stack" onSubmit={onInit}>
        <label className="field">
          <span>Saved deployment label</span>
          <input
            onChange={(event) => onLabelChange(event.currentTarget.value)}
            placeholder="Local Agent"
            value={label}
          />
        </label>
        <label className="checkbox">
          <input
            checked={dangerouslyOverwrite}
            onChange={(event) =>
              onDangerouslyOverwriteChange(event.currentTarget.checked)
            }
            type="checkbox"
          />
          <span>Dangerously overwrite desktop home</span>
        </label>
        <label className="checkbox">
          <input
            checked={reset}
            onChange={(event) => onResetChange(event.currentTarget.checked)}
            type="checkbox"
          />
          <span>Reset desktop runtime state</span>
        </label>
        <button className="primary-button" disabled={initializing}>
          {initializing ? "Initializing…" : "Run desktop init"}
        </button>
      </form>
      {initSummary ? (
        <div className="callout success">
          <h3>Init complete</h3>
          <p>{initSummary.label}</p>
          <p className="mono">{initSummary.agentDid}</p>
        </div>
      ) : null}
      <p className="muted small">
        Desktop core is currently {running ? "running" : "stopped"}.
      </p>
    </details>
  );
}
