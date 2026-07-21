import { useCallback, useEffect, useRef, useState } from "react";

import { listWorkspace } from "../../lib/desktop-api";
import type { WorkspaceListingView } from "../../lib/types";
import { CopyButton } from "../CopyButton";

/// Read-only browser over the local agent's tool root — the workspace the
/// code tools actually operate in. One directory per fetch (lazy descent),
/// jailed server-side to the root.
export function WorkspaceTreePanel() {
  const [listing, setListing] = useState<WorkspaceListingView | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generationRef = useRef(0);

  const load = useCallback(async (subpath: string | null) => {
    const generation = ++generationRef.current;
    setLoading(true);
    setError(null);
    try {
      const next = await listWorkspace(subpath);
      if (generationRef.current === generation) {
        setListing(next);
      }
    } catch (err) {
      if (generationRef.current === generation) {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      if (generationRef.current === generation) {
        setLoading(false);
      }
    }
  }, []);

  useEffect(() => {
    void load(null);
  }, [load]);

  const subpath = listing?.subpath ?? "";
  const crumbs = subpath.split("/").filter(Boolean);
  const parent = crumbs.slice(0, -1).join("/");

  return (
    <section className="workspace-tree" data-testid="workspace-tree">
      <div className="workspace-tree-toolbar">
        <div className="workspace-tree-path mono" title={listing?.root ?? ""}>
          <button
            className="ghost-button workspace-crumb"
            data-testid="workspace-root-crumb"
            onClick={() => void load(null)}
            type="button"
          >
            workspace
          </button>
          {crumbs.map((crumb, index) => (
            <span key={`${crumb}-${index}`}>
              {" / "}
              <button
                className="ghost-button workspace-crumb"
                onClick={() => void load(crumbs.slice(0, index + 1).join("/"))}
                type="button"
              >
                {crumb}
              </button>
            </span>
          ))}
        </div>
        <button
          className="ghost-button"
          data-testid="workspace-refresh"
          disabled={loading}
          onClick={() => void load(subpath || null)}
          type="button"
        >
          {loading ? "Loading..." : "Refresh"}
        </button>
      </div>

      {error ? (
        <p className="workspace-tree-error" data-testid="workspace-error" role="alert">
          {error}
        </p>
      ) : null}

      {listing ? (
        <ul className="workspace-entries">
          {subpath ? (
            <li>
              <button
                className="workspace-entry workspace-entry-dir"
                data-testid="workspace-up"
                onClick={() => void load(parent || null)}
                type="button"
              >
                ..
              </button>
            </li>
          ) : null}
          {listing.entries.map((entry) => (
            <li key={entry.name}>
              {entry.kind === "dir" ? (
                <button
                  className="workspace-entry workspace-entry-dir"
                  data-testid={`workspace-dir-${entry.name}`}
                  onClick={() =>
                    void load(subpath ? `${subpath}/${entry.name}` : entry.name)
                  }
                  type="button"
                >
                  {entry.name}/
                </button>
              ) : (
                <span className="workspace-entry workspace-entry-file">
                  <span className="workspace-entry-name">{entry.name}</span>
                  <span className="workspace-entry-size">{formatSize(entry.size)}</span>
                  <CopyButton
                    className="workspace-entry-copy"
                    label="Copy path"
                    getText={() => (subpath ? `${subpath}/${entry.name}` : entry.name)}
                  />
                </span>
              )}
            </li>
          ))}
          {!listing.entries.length ? (
            <li className="muted workspace-empty">empty directory</li>
          ) : null}
        </ul>
      ) : null}
      {listing?.truncated ? (
        <p className="muted">Listing capped at 500 entries.</p>
      ) : null}
    </section>
  );
}

function formatSize(size?: number | null): string {
  if (size == null) {
    return "";
  }
  if (size < 1024) {
    return `${size} B`;
  }
  if (size < 1024 * 1024) {
    return `${Math.round(size / 1024)} KiB`;
  }
  return `${(size / (1024 * 1024)).toFixed(1)} MiB`;
}
