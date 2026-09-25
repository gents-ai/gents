import { useState } from "react";

import type { IncompatibleHome } from "../hooks/useIncompatibleHome";
import { Mark } from "../ui/app/Mark";

type IncompatibleHomeScreenProps = {
  home: IncompatibleHome;
  error: string | null;
};

const SCOPE_LABEL = {
  runtime: "Local agent data",
  client: "Desktop app data",
} as const;

/**
 * The single answer to a local home this version cannot open, whichever
 * operation found it: back it up (the default), delete it, or quit without
 * touching it.
 */
export function IncompatibleHomeScreen({ home, error }: IncompatibleHomeScreenProps) {
  const [confirmingDelete, setConfirmingDelete] = useState(false);
  const report = home.report;
  if (!report) return null;
  const location = report.managedHome ?? report.desktopHome ?? "this home";
  const older = report.stores.every((store) => store.older);
  const clientStateOnlyWithRuntime =
    report.managedHome !== null &&
    report.desktopHome !== null &&
    !report.stores.some((store) => store.scope === "client");
  const deletable = report.deleteConfirmation !== null;
  const unsafeKey = report.stores.some((store) => store.unsafeKey);
  const removesInstalledContent = report.deletePaths.some((path) =>
    /[\\/](packs|plugins)$/.test(path),
  );
  const pathList = (paths: string[], testId: string) => (
    <ul
      className="grid max-h-48 gap-1 overflow-y-auto rounded-lg border border-border/60 px-4 py-3 text-xs"
      aria-label="Files and folders this changes"
      data-testid={testId}
    >
      {paths.map((path) => (
        <li key={path}>
          <code className="break-all">{path}</code>
        </li>
      ))}
    </ul>
  );

  return (
    <section
      aria-labelledby="incompatible-home-title"
      className="viewport-frame grid place-items-center overflow-y-auto bg-background px-8 py-10 text-foreground"
      data-testid="incompatible-home"
    >
      <div className="grid w-full max-w-lg gap-6">
        <Mark className="h-4 text-ink" />
        {report.completed ? (
          <div className="grid gap-3" data-testid="incompatible-home-done">
            <h2
              id="incompatible-home-title"
              className="font-heading text-2xl font-medium text-heading"
            >
              {report.disposition === "delete"
                ? "Old home deleted"
                : "Old home backed up"}
            </h2>
            {report.backupPath ? (
              <p className="text-sm">
                Everything from the old home is in{" "}
                <code className="break-all" data-testid="incompatible-home-backup-path">
                  {report.backupPath}
                </code>
                . Gents will not read it again.
              </p>
            ) : (
              <p className="text-sm">The old home was permanently deleted.</p>
            )}
            <button
              className="inline-flex h-8 w-fit items-center rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground"
              data-testid="incompatible-home-continue"
              disabled={home.busy}
              onClick={() => void home.continueFresh()}
              type="button"
            >
              Set up Gents
            </button>
          </div>
        ) : (
          <div className="grid gap-4">
            <div className="grid gap-2">
              <p className="font-mono text-[10px] tracking-wide text-muted-foreground uppercase">
                Upgrade
              </p>
              <h2
                id="incompatible-home-title"
                className="font-heading text-2xl font-medium text-heading"
              >
                This home needs a fresh start
              </h2>
              {unsafeKey ? (
                <p className="text-sm" data-testid="incompatible-home-unsafe-key">
                  This home&apos;s keys were created by an older Gents version with
                  unsafe file permissions, so other users on this computer could read
                  them. 0.19 does not use a key that may have been exposed; start fresh
                  with a new identity.
                </p>
              ) : older ? (
                <p className="text-sm">
                  This home was created by an older Gents version. 0.19 is a breaking
                  release and can&apos;t open it.
                </p>
              ) : (
                <p
                  className="text-sm"
                  data-testid="incompatible-home-different-version"
                >
                  This home was written by a different Gents version, possibly a newer
                  one, and this version can&apos;t open it. Use the version that wrote
                  it, or back it up to start fresh.
                </p>
              )}
            </div>
            <ul
              className="grid gap-2 text-sm"
              aria-label="Stores this version cannot open"
            >
              {report.stores.map((store) => (
                <li
                  className="grid gap-1 rounded-lg border border-border/60 px-4 py-3"
                  key={`${store.scope}:${store.path}`}
                >
                  <span className="font-medium">{SCOPE_LABEL[store.scope]}</span>
                  <code className="break-all text-muted-foreground">{store.path}</code>
                </li>
              ))}
            </ul>
            {report.retainedPaths.length > 0 ? (
              <p
                className="text-sm text-muted-foreground"
                data-testid="incompatible-home-retained"
              >
                Other agent homes inside it stay where they are:{" "}
                {report.retainedPaths.map((path) => (
                  <code className="block break-all" key={path}>
                    {path}
                  </code>
                ))}
              </p>
            ) : null}
            <div className="grid gap-1 text-sm">
              <p>Starting fresh moves or deletes exactly these:</p>
              {pathList(report.plannedPaths, "incompatible-home-planned")}
            </div>
            {error ? <p className="text-sm text-destructive">{error}</p> : null}
            {confirmingDelete ? (
              <div
                className="grid gap-3 rounded-lg border border-destructive/40 p-3 text-sm"
                data-testid="incompatible-home-delete-confirmation"
              >
                <p>
                  Permanently delete the old home at{" "}
                  <code className="break-all">{location}</code>
                  {report.desktopHome && report.managedHome
                    ? " and the desktop app's data"
                    : ""}
                  ? This can&apos;t be undone.
                </p>
                {clientStateOnlyWithRuntime ? (
                  <p data-testid="incompatible-home-delete-client-note">
                    This also deletes this app&apos;s own identity key and its pairings
                    with remote agents. Back up instead to keep them.
                  </p>
                ) : null}
                {removesInstalledContent ? (
                  <p data-testid="incompatible-home-delete-installed-note">
                    This also deletes the packs and plugins you installed for this
                    agent.
                  </p>
                ) : null}
                {pathList(report.deletePaths, "incompatible-home-delete-paths")}
                <div className="flex flex-wrap gap-2">
                  <button
                    className="inline-flex h-8 w-fit items-center rounded-lg bg-destructive px-3 text-sm font-medium text-destructive-foreground disabled:opacity-50"
                    data-testid="incompatible-home-delete-confirm"
                    disabled={home.busy}
                    onClick={() => void home.remove()}
                    type="button"
                  >
                    Delete permanently
                  </button>
                  <button
                    className="inline-flex h-8 w-fit items-center rounded-lg border border-border px-3 text-sm font-medium"
                    data-testid="incompatible-home-delete-cancel"
                    disabled={home.busy}
                    onClick={() => setConfirmingDelete(false)}
                    type="button"
                  >
                    Cancel
                  </button>
                </div>
              </div>
            ) : (
              <div className="grid gap-3">
                <div className="grid gap-1">
                  <button
                    className="inline-flex h-8 w-fit items-center rounded-lg bg-brand px-3 text-sm font-medium text-brand-foreground disabled:opacity-50"
                    data-testid="incompatible-home-backup"
                    disabled={home.busy}
                    onClick={() => void home.backUp()}
                    type="button"
                  >
                    Back up and start fresh
                  </button>
                  <p className="text-xs text-muted-foreground">
                    Moves the old home into a dated backup folder next to{" "}
                    <code className="break-all">{location}</code>. Nothing is imported.
                  </p>
                </div>
                <div className="flex flex-wrap gap-2">
                  {deletable ? (
                    <button
                      className="inline-flex h-8 w-fit items-center rounded-lg border border-destructive/60 px-3 text-sm font-medium text-destructive disabled:opacity-50"
                      data-testid="incompatible-home-delete"
                      disabled={home.busy}
                      onClick={() => setConfirmingDelete(true)}
                      type="button"
                    >
                      Delete and start fresh
                    </button>
                  ) : null}
                  <button
                    className="inline-flex h-8 w-fit items-center rounded-lg border border-border px-3 text-sm font-medium disabled:opacity-50"
                    data-testid="incompatible-home-keep"
                    disabled={home.busy}
                    onClick={() => void home.keep()}
                    type="button"
                  >
                    Keep it and quit
                  </button>
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
