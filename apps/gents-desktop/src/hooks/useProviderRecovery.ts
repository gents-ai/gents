import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  ProviderRecoveryAction,
  ProviderRecoveryPlan,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { setupErrorMessage } from "../lib/setupErrors";

export function useProviderRecovery({
  nodeDid,
  sessionId,
  requestId,
  turnState,
}: {
  nodeDid: string;
  sessionId: string;
  requestId: string;
  turnState: string | null;
}) {
  const { api } = useApp();
  const scope = useMemo(
    () => ({ nodeDid, sessionId, requestId }),
    [nodeDid, sessionId, requestId],
  );
  const [plan, setPlan] = useState<ProviderRecoveryPlan | null>(null);
  const [target, setTarget] = useState("");
  const [withCompaction, setWithCompaction] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [now, setNow] = useState(Date.now);
  const inFlight = useRef(false);
  const generation = useRef(0);
  const reload = useCallback(async () => {
    if (!api.fetchProviderRecovery) return;
    const read = ++generation.current;
    try {
      const next = await api.fetchProviderRecovery(scope);
      if (read !== generation.current) return;
      setPlan(next);
      setTarget((previous) =>
        next?.candidates.some((candidate) => candidate.backendId === previous)
          ? previous
          : (next?.candidates[0]?.backendId ?? ""),
      );
    } catch (failed) {
      if (read === generation.current) setError(setupErrorMessage(failed));
    }
  }, [api, scope]);
  useEffect(() => {
    setPlan(null);
    setError("");
    setMessage("");
    setWithCompaction(false);
    void reload();
    return () => {
      generation.current += 1;
    };
  }, [reload, turnState]);
  useEffect(() => {
    if (!plan?.resetsAt) return;
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, [plan?.resetsAt]);

  const perform = async (action: ProviderRecoveryAction) => {
    if (inFlight.current || !api.applyProviderRecovery) return;
    inFlight.current = true;
    setBusy(true);
    setError("");
    setMessage("");
    const active = generation.current;
    try {
      const result = await api.applyProviderRecovery({ scope, action });
      if (active !== generation.current) return;
      if (result.outcome === "applied") setMessage(result.message);
      else setError(result.reason);
      if (result.outcome !== "refused") await reload();
    } catch (failed) {
      if (active === generation.current) setError(setupErrorMessage(failed));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  const refresh = () => {
    setError("");
    void reload();
  };

  return {
    plan,
    target,
    setTarget,
    withCompaction,
    setWithCompaction,
    busy,
    error,
    message,
    now,
    perform,
    refresh,
    canApply: Boolean(api.applyProviderRecovery),
  };
}
