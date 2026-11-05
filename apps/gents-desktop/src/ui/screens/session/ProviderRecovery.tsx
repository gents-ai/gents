import { useState } from "react";
import { RefreshCw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@gents/ui/components/button";
import { Checkbox } from "@gents/ui/components/checkbox";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { useApp } from "@/app/AppContext";
import { useFleet } from "@/hooks/useFleet";
import { nodeOf } from "../../../hooks/fleetStore";
import { useProviderRecovery } from "../../../hooks/useProviderRecovery";
import { setupErrorMessage } from "../../../lib/setupErrors";
import { errorWords, reasonText, sentence } from "../../../lib/reasonText";
import { Notice } from "../../components/Notice";
import { Hint } from "../Hint";
import { ProviderLogo } from "../ProviderLogo";
import { SUBSCRIPTION } from "../agent/inferenceKinds";

const ACCOUNT_STATE: Record<string, string> = {
  usage_limit: "has reached its usage limit",
  account_disabled: "is disabled",
  account_removed: "is no longer on this node",
  account_signed_out: "needs to sign in again",
};

/** When the limit lifts, in words: "Resets in 2h 14m." */
export function resetDescription(reset: string | null, now: number) {
  if (!reset || !Number.isFinite(Date.parse(reset)))
    return "It didn't report when it resets.";
  const remaining = Date.parse(reset) - now;
  if (remaining <= 0) return "Its reported reset time has passed.";
  const minutes = Math.ceil(remaining / 60_000);
  const duration =
    minutes >= 60 ? `${Math.floor(minutes / 60)}h ${minutes % 60}m` : `${minutes}m`;
  return `Resets in ${duration}.`;
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
  } = useProviderRecovery({
    nodeDid,
    sessionId,
    requestId,
    turnState,
    readable: useFleet((state) => nodeOf(state, nodeDid)?.operatorAccess) ?? true,
  });
  const { actions } = useApp();
  const node = useFleet((state) => nodeOf(state, nodeDid));
  const [signingIn, setSigningIn] = useState(false);

  if (!plan && !error && !message) return null;
  const candidate = plan?.candidates.find((entry) => entry.backendId === target);
  const goal = plan?.goal;
  const limitedGoal = goal?.status === "usage_limited";
  /* the profile the limit stops, its backend and its model, by name */
  const profile = node?.inferenceProfiles.find((p) => p.profile_id === plan?.profileId);
  const backend = node?.inferenceBackends.find(
    (b) => b.backendId === profile?.backend_id,
  );
  const model = backend?.advertisedModels?.find(
    (m) => m.model_name === profile?.model_name,
  );
  const named = (text: string) =>
    profile && model?.display_name
      ? text.split(profile.model_name).join(model.display_name)
      : text;
  const sub = SUBSCRIPTION[backend?.providerKind ?? ""];
  const provider = sub?.title.split(" / ").pop() ?? null;
  const signIn = async () => {
    if (!sub) return;
    setSigningIn(true);
    try {
      await actions.signInToProvider(nodeDid, sub.login);
      refresh();
    } catch (failed) {
      toast(`Sign in failed: ${errorWords(setupErrorMessage(failed))}`);
    } finally {
      setSigningIn(false);
    }
  };
  const agents = plan?.affectedAgents.length ?? 0;
  return (
    <Notice
      tone={plan ? "warning" : "destructive"}
      role="region"
      aria-label="Account recovery"
      className="my-4"
      icon={
        backend ? (
          <ProviderLogo
            kind={backend.providerKind}
            endpoint={backend.endpoint}
            className="size-4"
          />
        ) : undefined
      }
      title={
        plan
          ? `${plan.accountLabel ?? "This account"} ${ACCOUNT_STATE[plan.reason] ?? "can't serve this turn"}`
          : "The account's state couldn't be read"
      }
      actions={
        <>
          {plan && plan.candidates.length > 0 && (
            <>
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
                  variant="brand"
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
                  Resume on {candidate?.label ?? "the chosen account"}
                </Button>
              )}
            </>
          )}
          {plan && plan.candidates.length === 0 && sub && (
            <Button size="sm" variant="outline" disabled={signingIn} onClick={signIn}>
              {signingIn ? "Signing in…" : `Add a ${provider} account`}
            </Button>
          )}
          <Hint label="Refresh account state">
            <Button
              size="icon-sm"
              variant="quiet"
              aria-label="Refresh account state"
              className="ml-auto"
              disabled={busy}
              onClick={refresh}
            >
              <RefreshCw />
            </Button>
          </Hint>
        </>
      }
    >
      {plan && (
        <>
          <p>
            {plan.reason === "usage_limit" &&
              (plan.resetsAt && Number.isFinite(Date.parse(plan.resetsAt)) ? (
                <Hint label={new Date(plan.resetsAt).toLocaleString()}>
                  <span className="underline decoration-border decoration-dotted underline-offset-4">
                    {resetDescription(plan.resetsAt, now)}
                  </span>
                </Hint>
              ) : (
                resetDescription(plan.resetsAt, now)
              ))}
            {plan.profileId && agents > 0 && (
              <>
                {" "}
                {profile?.display_name
                  ? `Its profile, ${profile.display_name}, is used by`
                  : "Its profile is used by"}{" "}
                {agents} {agents === 1 ? "agent" : "agents"}.
              </>
            )}
          </p>
          {plan.candidates.length > 0 ? (
            <div className="grid gap-2">
              <div className="flex flex-wrap items-center gap-2 text-foreground">
                <span>Move to</span>
                <Select
                  items={plan.candidates.map((c) => ({
                    value: c.backendId,
                    label: c.label,
                  }))}
                  value={target}
                  onValueChange={(v) => setTarget(String(v ?? ""))}
                  disabled={busy}
                >
                  <SelectTrigger aria-label="Move to account" className="w-64">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {plan.candidates.map((entry) => (
                      <SelectItem key={entry.backendId} value={entry.backendId}>
                        {entry.label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
              {plan.companionProfiles.length > 0 && (
                <label className="flex items-center gap-2 text-foreground">
                  <Checkbox
                    checked={withCompaction}
                    disabled={busy}
                    onCheckedChange={(checked) => setWithCompaction(checked === true)}
                  />
                  Also move the profiles it works with (
                  {plan.companionProfiles
                    .map(
                      (id) =>
                        node?.inferenceProfiles.find((p) => p.profile_id === id)
                          ?.display_name ?? id,
                    )
                    .join(", ")}
                  )
                </label>
              )}
              {plan.switchCost && (
                <p className="text-xs">{sentence(plan.switchCost)}</p>
              )}
            </div>
          ) : (
            <p>
              {named(reasonText(plan.unavailableReason)) ||
                "No other account can take over this turn."}
            </p>
          )}
          {limitedGoal && (
            <label className="flex items-start gap-2 text-foreground">
              <Checkbox
                className="mt-0.5"
                checked={goal.autoResumeAtReset}
                disabled={busy || !canApply}
                onCheckedChange={(checked) =>
                  void perform({
                    kind: "set_auto_resume",
                    goalId: goal.goalId,
                    enabled: checked === true,
                  })
                }
              />
              <span>
                Resume this goal automatically when the limit resets
                <span className="block text-xs text-muted-foreground">
                  On the same account, once it reports its limit has reset.
                </span>
              </span>
            </label>
          )}
        </>
      )}
      {error && (
        <p role="alert" className="text-destructive">
          {errorWords(error)}
        </p>
      )}
      {message && <p role="status">{sentence(message)}</p>}
    </Notice>
  );
}
