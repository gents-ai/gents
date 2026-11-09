import type { ReactNode } from "react";
import { Pencil } from "lucide-react";
import type {
  InferenceBackendView,
  InferenceProfile,
  ProviderAccountView,
} from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../../hooks/fleetStore";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { Switch } from "@gents/ui/components/switch";
import { cn } from "@gents/ui/lib/utils";
import { ProviderLogo } from "../ProviderLogo";
import { StateDot, counted, type Tone } from "../NodeFacts";
import { Summary } from "./AgentCard";
import { healthy, SUBSCRIPTION } from "./inferenceKinds";
import { backendAccount } from "./ProviderAccounts";
import { isPool, poolAccounts } from "./backendRotation";

/** How the provider is paid for and whether that needs the person. */
function credentialOf(
  backend: InferenceBackendView,
  accounts: readonly ProviderAccountView[],
): { text: string; tone: Tone; problem: string | null } {
  const sub = SUBSCRIPTION[backend.providerKind ?? ""];
  if (sub) {
    if (isPool(backend)) {
      const pooled = poolAccounts(accounts, sub.provider).length;
      return pooled
        ? { text: `Pool of ${counted(pooled, "account")}`, tone: "good", problem: null }
        : { text: "No account in the pool", tone: "bad", problem: "No account to use" };
    }
    const stored = backendAccount(accounts, backend);
    if (!stored)
      return backend.accountRef
        ? {
            text: "Account not on this node",
            tone: "bad",
            problem: "Its account isn't on this node",
          }
        : { text: "Not signed in", tone: "bad", problem: "Not signed in" };
    return stored.enabled
      ? { text: stored.label, tone: "good", problem: null }
      : {
          text: `${stored.label}, disabled`,
          tone: "bad",
          problem: "Its account is disabled",
        };
  }
  if (backend.authKind === "credential")
    return { text: "Stored credential", tone: "good", problem: null };
  if (backend.apiKeyConfigured)
    return { text: "Stored key", tone: "good", problem: null };
  if (backend.apiKeyEnvVar)
    return { text: `From ${backend.apiKeyEnvVar}`, tone: "good", problem: null };
  /* a local endpoint often takes no key, so none is a fact, not a fault */
  return { text: "No key", tone: "idle", problem: null };
}

export function ProviderSummary({
  deployment,
  backend,
  accounts,
  profiles,
  draft,
  edit,
  probe,
}: {
  deployment: NodeView;
  backend: InferenceBackendView;
  accounts: readonly ProviderAccountView[];
  profiles: readonly InferenceProfile[];
  /** what the header shows and switches, as the form has it */
  draft: {
    name: string;
    kind: string;
    kindLabel: string;
    endpoint: string;
    enabled: boolean;
    onEnabled: (enabled: boolean) => void;
  };
  edit: { open: boolean; onToggle: () => void; controls: string };
  /** Probe, or Refresh models for a subscription */
  probe: ReactNode;
}) {
  const credential = credentialOf(backend, accounts);
  const profileIds = new Set(profiles.map((p) => p.profile_id));
  const agents = deployment.agents.filter(
    (a) => a.inferenceProfileId && profileIds.has(a.inferenceProfileId),
  );
  const probeFailed = backend.probeStatus != null && !healthy(backend.probeStatus);
  const problem =
    credential.problem ?? (probeFailed ? `Last probe: ${backend.probeStatus}` : null);
  const section = (section: string, item?: string) => ({
    name: "agent" as const,
    nodeDid: deployment.nodeDid,
    section,
    ...(item ? { item } : {}),
  });
  return (
    <div className="@container mb-8 grid gap-4">
      <div className="flex items-start gap-4">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <ProviderLogo
              kind={draft.kind}
              endpoint={draft.endpoint}
              className="size-5"
            />
            <h2
              className={cn(
                "truncate font-heading text-lg font-medium",
                draft.enabled ? "text-heading" : "text-muted-foreground",
              )}
            >
              {draft.name.trim() || backend.backendId}
            </h2>
            {!draft.enabled && <Badge variant="outline">Disabled</Badge>}
            <Button
              size="xs"
              variant="quiet"
              aria-expanded={edit.open}
              aria-controls={edit.controls}
              onClick={edit.onToggle}
            >
              {edit.open ? (
                "Done"
              ) : (
                <>
                  <Pencil /> Edit
                </>
              )}
            </Button>
          </div>
          <p className="mt-0.5 text-sm text-foreground/80">{draft.kindLabel}</p>
          {problem && <p className="mt-1 text-sm text-destructive">{problem}</p>}
        </div>
        <div className="flex shrink-0 items-center gap-3">
          {/* switched here, saved with the rest of the form */}
          <label className="flex items-center gap-2.5 text-sm">
            {draft.enabled ? "Enabled" : "Disabled"}
            <Switch
              aria-label="Enabled"
              checked={draft.enabled}
              onCheckedChange={draft.onEnabled}
            />
          </label>
          {probe}
        </div>
      </div>
      <div className="grid gap-2 @xl:grid-cols-3">
        <Summary
          label="Used by"
          to={
            profiles.length === 1
              ? section("profiles", profiles[0]!.profile_id)
              : section("profiles")
          }
        >
          <span className="truncate">
            {profiles.length
              ? `${counted(profiles.length, "profile")} · ${counted(agents.length, "agent")}`
              : "No profile yet"}
          </span>
        </Summary>
        <Summary label="Models">
          <span className="truncate">
            {backend.models.length
              ? counted(backend.models.length, "model")
              : "None read yet"}
          </span>
        </Summary>
        <Summary label={SUBSCRIPTION[backend.providerKind ?? ""] ? "Account" : "Key"}>
          <StateDot tone={credential.tone} />
          <span className="truncate">{credential.text}</span>
        </Summary>
      </div>
    </div>
  );
}
