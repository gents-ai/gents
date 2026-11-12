/* A node's workflow runs, read with `listJobs` over the node's operator
   access, so a remote node reads as a local one does. The reader filters
   every job row on the node in memory and has no limit. */

import { useEffect, useState } from "react";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { errorText } from "../lib/errorText";
import { useApp } from "../ui/app/AppContext";

export type JobsRequest = Parameters<DesktopApiAdapter["listJobs"]>[0];

/** The runs `request` names, read again whenever `observed` changes; a
    failed re-read keeps the runs it had. */
export function useJobs(request: JobsRequest | null, observed: unknown) {
  const { actions } = useApp();
  const key = request ? JSON.stringify(request) : null;
  const [read, setRead] = useState<{
    key: string;
    runs: JobView[] | null;
    error: string | null;
  } | null>(null);
  useEffect(() => {
    if (!key) return;
    let current = true;
    actions.listJobs(JSON.parse(key) as JobsRequest).then(
      (runs) => current && setRead({ key, runs, error: null }),
      (e: unknown) =>
        current &&
        setRead((held) => ({
          key,
          runs: held?.key === key ? held.runs : null,
          error: errorText(e),
        })),
    );
    return () => {
      current = false;
    };
  }, [actions, key, observed]);
  const current = read && read.key === key ? read : null;
  return { runs: current?.runs ?? null, error: current?.error ?? null };
}
