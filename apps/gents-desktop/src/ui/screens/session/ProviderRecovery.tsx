import { Button } from "@gents/ui/components/button";
import { useProviderRecovery } from "../../../hooks/useProviderRecovery";

const ACCOUNT_STATE: Record<string, string> = {
  usage_limit: "has reached its usage limit",
  account_disabled: "is disabled",
  account_removed: "is no longer on this node",
  account_signed_out: "needs to sign in again",
};

export function resetDescription(reset: string | null, now: number) {
  if (!reset || !Number.isFinite(Date.parse(reset))) return "Reset time not reported.";
  const remaining = Date.parse(reset) - now;
  if (remaining <= 0) return "The reported reset time has passed.";
  const minutes = Math.ceil(remaining / 60_000);
  const duration =
    minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${minutes}m`;
  return `Reset in ${duration} (${new Date(reset).toLocaleString()}).`;
}

export function ProviderRecovery({
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
  const {
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
    canApply,
  } = useProviderRecovery({ nodeDid, sessionId, requestId, turnState });

  if (!plan && !error && !message) return null;
  const candidate = plan?.candidates.find((entry) => entry.backendId === target);
  const goal = plan?.goal;
  const limitedGoal = goal?.status === "usage_limited";
  return (
    <aside
      aria-label="Account recovery"
      className="my-4 space-y-3 rounded-lg border border-border bg-muted/30 p-4 text-sm"
    >
      {plan && (
        <>
          <p className="font-medium">
            {plan.accountLabel ?? "This account"}{" "}
            {ACCOUNT_STATE[plan.reason] ?? "cannot serve this turn"}.
          </p>
          {plan.reason === "usage_limit" && (
            <p>{resetDescription(plan.resetsAt, now)}</p>
          )}
          {plan.profileId && (
            <p>
              Moving this profile affects {plan.affectedAgents.length}{" "}
              {plan.affectedAgents.length === 1 ? "Agent" : "Agents"}.
            </p>
          )}
          {plan.candidates.length > 0 ? (
            <>
              <label className="flex flex-wrap items-center gap-2">
                <span>Move to account</span>
                <select
                  aria-label="Move to account"
                  value={target}
                  disabled={busy}
                  onChange={(event) => setTarget(event.target.value)}
                  className="rounded border border-border bg-background px-2 py-1"
                >
                  {plan.candidates.map((entry) => (
                    <option key={entry.backendId} value={entry.backendId}>
                      {entry.label}
                    </option>
                  ))}
                </select>
              </label>
              {plan.companionProfiles.length > 0 && (
                <label className="flex items-center gap-2">
                  <input
                    type="checkbox"
                    checked={withCompaction}
                    disabled={busy}
                    onChange={(event) => setWithCompaction(event.target.checked)}
                  />
                  Also move companion profiles: {plan.companionProfiles.join(", ")}
                </label>
              )}
              <p className="text-muted-foreground">{plan.switchCost}</p>
              <div className="flex flex-wrap gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy || !candidate || !plan.profileId || !canApply}
                  onClick={() => {
                    if (plan.profileId)
                      void perform({
                        kind: "move_profile",
                        profileId: plan.profileId,
                        backendId: target,
                        withCompaction,
                      });
                  }}
                >
                  Move profile
                </Button>
                {limitedGoal && (
                  <Button
                    size="sm"
                    disabled={busy || !candidate || !canApply}
                    onClick={() => {
                      if (goal)
                        void perform({
                          kind: "resume_goal",
                          goalId: goal.goalId,
                          backendId: target,
                          withCompaction,
                        });
                    }}
                  >
                    Resume on {candidate?.label ?? "selected account"}
                  </Button>
                )}
              </div>
            </>
          ) : (
            <p className="text-muted-foreground">
              {plan.unavailableReason ?? "No other eligible account is available."}
            </p>
          )}
          {limitedGoal && (
            <label className="flex items-start gap-2">
              <input
                type="checkbox"
                className="mt-1"
                checked={goal.autoResumeAtReset}
                disabled={busy || !canApply}
                onChange={(event) =>
                  void perform({
                    kind: "set_auto_resume",
                    goalId: goal.goalId,
                    enabled: event.target.checked,
                  })
                }
              />
              <span>
                Automatically resume this Goal at reset
                <span className="block text-xs text-muted-foreground">
                  Uses the same account and waits for its reported reset.
                </span>
              </span>
            </label>
          )}
        </>
      )}
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
      <Button size="sm" variant="quiet" disabled={busy} onClick={refresh}>
        Refresh account state
      </Button>
    </aside>
  );
}
