import { useEffect, useState } from "react";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { JobView } from "@source-inc/gents-desktop-client/generated/JobView";
import { errorText } from "../lib/errorText";
import { useApp } from "../ui/app/AppContext";

export type JobsRequest = Parameters<DesktopApiAdapter["listJobs"]>[0];

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
