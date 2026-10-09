/* One inference profile's editor: the backend and model it runs, the
   model's recommended settings and how far they may be changed, and who
   uses it. */
import type { NodeView } from "../../../hooks/fleetStore";
import { useState } from "react";
import { dependentsWarning } from "./dependents";
import type {
  ProviderAccountView,
  InferenceExecution,
  InferenceModelRecommendation,
  InferenceProfile,
  InferenceSampling,
} from "@source-inc/gents-desktop-client";
import { href } from "@/lib/router";
import {
  AreaRow,
  ChoiceRow,
  DraftActions,
  FactRow,
  NumberRow,
  RefRow,
  TagsRow,
  TextRow,
} from "./editors";
import { profileBackend } from "./ProviderAccounts";
import { useAccounts, useSetupCatalog } from "@/hooks/useProviders";
import { BackendSheet } from "./BackendSheet";
import type { InferenceBackendView } from "@source-inc/gents-desktop-client";
import { SetupScreen } from "../setup/SetupScreen";
import { newId, str, useDraft } from "./draft";
import {
  PROFILE_NUMBERS,
  profileDraftFrom,
  profileNumbers,
  profileProblems,
  type ProfileContext,
  type ProfileDraft,
} from "./profileDraft";
import { DeleteButton } from "./ListDetail";
import { Group } from "./rows";
import {
  InferenceModelControls,
  recommendedInferenceSettings,
  validateInferenceSettings,
  type InferenceSettingsDraft,
} from "../inference/InferenceModelControls";
import { useApp } from "@/app/AppContext";
import { useModelRecommendation } from "./useModelRecommendation";
import { Disclosure } from "../../components/Disclosure";

/* behaviors named in Used by before the rest are counted */
const USERS_SHOWN = 3;

function settingsForDraft(
  recommendation: InferenceModelRecommendation,
  draft: {
    contextWindow: string;
    maxOutputTokens: string;
    temperature: string;
    topP: string;
    reasoningEffort: string;
  },
  maxConcurrent: number | null | undefined,
  edited: ReadonlySet<keyof InferenceSettingsDraft> = new Set(),
): InferenceSettingsDraft {
  const defaults = recommendedInferenceSettings(recommendation);
  return {
    ...defaults,
    contextWindow: edited.has("contextWindow")
      ? draft.contextWindow
      : draft.contextWindow || defaults.contextWindow,
    maxOutputTokens: edited.has("maxOutputTokens")
      ? draft.maxOutputTokens
      : draft.maxOutputTokens || defaults.maxOutputTokens,
    temperature: edited.has("temperature")
      ? draft.temperature
      : draft.temperature || defaults.temperature,
    topP: edited.has("topP") ? draft.topP : draft.topP || defaults.topP,
    reasoningEffort: edited.has("reasoningEffort")
      ? (draft.reasoningEffort as InferenceSettingsDraft["reasoningEffort"])
      : (draft.reasoningEffort as InferenceSettingsDraft["reasoningEffort"]) ||
        defaults.reasoningEffort,
    maxConcurrent: str(maxConcurrent) || defaults.maxConcurrent,
  };
}

/* the profile a draft starts from: the one asked for if its account (if
   any) is usable, else the first usable backend that advertises a model,
   moved to its provider's default account (the first enabled one in the
   resolver order the list keeps), and its first model */
export function newProfileDocument(
  deployment: NodeView,
  backendId?: string,
  accounts: readonly ProviderAccountView[] = [],
): InferenceProfile {
  const backends = deployment.inferenceBackends;
  const usable = (b: InferenceBackendView) => profileBackend(accounts, b).usable;
  const asked = backends.find((b) => b.backendId === backendId && usable(b));
  const start =
    asked ?? backends.find((b) => b.models.length > 0 && usable(b)) ?? backends[0];
  const provider =
    start && !asked ? profileBackend(accounts, start).provider : undefined;
  const first = accounts.find(
    (a) => a.provider === provider && a.enabled && !a.pendingSave,
  );
  const backend =
    (first &&
      backends.find(
        (b) => profileBackend(accounts, b).account?.credentialId === first.credentialId,
      )) ||
    start;
  return {
    agent_did: deployment.agentDid,
    profile_id: newId("profile"),
    display_name: "",
    backend_id: backend?.backendId ?? "",
    model_name: backend?.models[0] ?? "",
  };
}

export function ProfileEditor({
  deployment,
  profile,
  embedded = false,
  draft: draftMode,
}: {
  deployment: NodeView;
  /* in a sheet beside another page: no Danger zone */
  embedded?: boolean;
  profile: InferenceProfile;
  /* a new profile that exists only here until Create */
  draft?: { onSaved: (profileId: string) => void; onCancel: () => void };
}) {
  const {
    api,
    actions: { changeConfig },
  } = useApp();
  const base = {
    name: "agent" as const,
    agentDid: deployment.agentDid,
    section: "profiles",
  };
  const saved = profileDraftFrom(profile, deployment);
  const [customize, setCustomize] = useState(false);
  const [editedModelFields, setEditedModelFields] = useState<
    Set<keyof InferenceSettingsDraft>
  >(new Set());
  const advertisedMaxContext = (backendId: string, modelName: string) =>
    deployment.inferenceBackends
      .find((backend) => backend.backendId === backendId)
      ?.advertisedModels?.find((model) => model.model_name === modelName.trim())
      ?.max_context_window ?? undefined;
  const { accounts } = useAccounts(deployment.agentDid);
  const d = useDraft(
    saved,
    async (next) => {
      if (!recommendation)
        throw new Error(
          recommendationError ?? "Wait for model-aware settings before saving",
        );
      const selected = deployment.inferenceBackends.find(
        (backend) => backend.backendId === next.backendId,
      );
      const effectiveSettings = settingsForDraft(
        recommendation,
        next,
        selected?.maxConcurrent,
        editedModelFields,
      );
      const validationError = validateInferenceSettings(
        recommendation,
        effectiveSettings,
      );
      if (validationError) throw new Error(validationError);
      const hasSamplingValues = [
        next.temperature,
        next.topP,
        next.topK,
        next.seed,
        next.minP,
        next.frequencyPenalty,
        next.presencePenalty,
        next.repetitionPenalty,
      ].some((value) => value.trim());
      const effectiveSamplingId =
        next.samplingId.trim() ||
        (hasSamplingValues ? `${profile.profile_id}-sampling` : "");
      /* the fields' problems are said there and hold the save back; what
         has no field here is checked as it is read */
      const {
        contextWindow,
        maxOutputTokens,
        temperature,
        topP,
        topK,
        seed,
        minP,
        frequencyPenalty,
        presencePenalty,
        repetitionPenalty,
        maxTurns,
        maxTotalTokens,
        streamBatchMs,
        streamLivenessSecs,
        providerIdleSecs,
        deadlineSecs,
      } = profileNumbers(next, profileContext(next));

      const sampling = deployment.inferenceSampling.find(
        (row) => row.sampling_id === effectiveSamplingId,
      );
      const execution = deployment.inferenceExecution.find(
        (row) => row.execution_id === next.executionId.trim(),
      );
      const nextProfile: InferenceProfile = {
        ...profile,
        display_name: next.displayName.trim() || null,
        description: next.description.trim() || null,
        backend_id: next.backendId,
        model_name: next.modelName.trim(),
        reasoning_effort: (next.reasoningEffort || null) as NonNullable<
          InferenceProfile["reasoning_effort"]
        > | null,
        context_window: contextWindow,
        max_output_tokens: maxOutputTokens,
        sampling_id: effectiveSamplingId || null,
        execution_id: next.executionId.trim() || null,
        tags: next.tags.length ? next.tags : null,
      };
      const nextSampling: InferenceSampling | null = effectiveSamplingId
        ? {
            ...sampling,
            agent_did: deployment.agentDid,
            sampling_id: effectiveSamplingId,
            temperature,
            top_p: topP,
            top_k: topK,
            seed,
            min_p: minP,
            frequency_penalty: frequencyPenalty,
            presence_penalty: presencePenalty,
            repetition_penalty: repetitionPenalty,
          }
        : null;
      const nextExecution: InferenceExecution | null = next.executionId.trim()
        ? {
            ...execution,
            agent_did: deployment.agentDid,
            execution_id: next.executionId.trim(),
            max_turns: maxTurns,
            max_total_tokens: maxTotalTokens,
            stream_batch_ms: streamBatchMs,
            stream_liveness_timeout_secs: streamLivenessSecs,
            provider_idle_timeout_secs: providerIdleSecs,
            deadline_duration_secs: deadlineSecs,
            retry_policy_id: next.retryPolicyId.trim() || null,
          }
        : null;
      await changeConfig("applyConfigComponents", {
        document: {
          agent_principal: { agent_did: deployment.agentDid },
          inference_profiles: [nextProfile],
          ...(nextSampling ? { inference_sampling: [nextSampling] } : {}),
          ...(nextExecution ? { inference_execution: [nextExecution] } : {}),
        },
      });
    },
    { isNew: draftMode !== undefined },
  );
  const { catalog, error: executionDefaultsError } = useSetupCatalog();
  const executionDefaults: Record<string, number | null | undefined> =
    catalog?.executionDefaults ?? {};
  const executionDefault = (key: string) =>
    executionDefaults[key] == null ? "Unlimited" : String(executionDefaults[key]);
  const [editedExecution, setEditedExecution] = useState<Set<string>>(new Set());
  const setExecution = (
    key:
      | "maxTurns"
      | "maxTotalTokens"
      | "streamBatchMs"
      | "streamLivenessSecs"
      | "providerIdleSecs"
      | "deadlineSecs",
    value: string,
  ) => {
    setEditedExecution((keys) => new Set(keys).add(key));
    if (value && !d.draft.executionId)
      d.set("executionId", `${profile.profile_id}-execution`);
    d.set(key, value);
  };
  const selectedBackend = deployment.inferenceBackends.find(
    (entry) => entry.backendId === d.draft.backendId,
  );
  const advertisedModel = selectedBackend?.advertisedModels?.find(
    (entry) => entry.model_name === d.draft.modelName.trim(),
  );
  const {
    recommendation,
    error: recommendationError,
    choose,
  } = useModelRecommendation({
    api,
    backendId: d.draft.backendId,
    modelName: d.draft.modelName,
    backend: selectedBackend,
    advertised: advertisedModel,
    /* a model the person just picked takes its recommended settings */
    onChosen: (next) => {
      const defaults = recommendedInferenceSettings(next);
      d.set("contextWindow", defaults.contextWindow);
      d.set("maxOutputTokens", defaults.maxOutputTokens);
      d.set("temperature", defaults.temperature);
      d.set("topP", defaults.topP);
      d.set(
        "reasoningEffort",
        defaults.reasoningEffort as typeof d.draft.reasoningEffort,
      );
      if ((defaults.temperature || defaults.topP) && !d.draft.samplingId)
        d.set("samplingId", `${profile.profile_id}-sampling`);
    },
  });
  /* what the problems and the numbers are read against */
  const profileContext = (draft: ProfileDraft): ProfileContext => ({
    deployment,
    contextMax: recommendation?.contextWindow
      ? undefined
      : advertisedMaxContext(draft.backendId, draft.modelName),
    limitsShown: {
      contextWindow: Boolean(recommendation && !recommendation.contextWindow),
      maxOutputTokens: Boolean(recommendation && !recommendation.maxOutputTokens),
    },
  });
  const problems = profileProblems(d.draft, profileContext(d.draft));
  const beginModelSelection = (backendId: string, modelName: string) => {
    if (
      backendId === d.draft.backendId &&
      modelName.trim() === d.draft.modelName.trim()
    )
      return;
    choose(backendId, modelName);
    setEditedModelFields(new Set());
  };
  const updateGuided = (next: InferenceSettingsDraft) => {
    if (guided) {
      setEditedModelFields((fields) => {
        const changed = new Set(fields);
        for (const key of [
          "contextWindow",
          "maxOutputTokens",
          "temperature",
          "topP",
          "reasoningEffort",
        ] as const) {
          if (next[key] !== guided[key]) changed.add(key);
        }
        return changed;
      });
    }
    d.set("contextWindow", next.contextWindow);
    d.set("maxOutputTokens", next.maxOutputTokens);
    d.set("temperature", next.temperature);
    d.set("topP", next.topP);
    d.set("reasoningEffort", next.reasoningEffort as typeof d.draft.reasoningEffort);
    if ((next.temperature || next.topP) && !d.draft.samplingId) {
      d.set("samplingId", `${profile.profile_id}-sampling`);
    }
  };
  const guided = recommendation
    ? settingsForDraft(
        recommendation,
        d.draft,
        selectedBackend?.maxConcurrent,
        editedModelFields,
      )
    : null;
  const id = (f: string) => `${profile.profile_id}-${f}`;
  /* the backend's full editor, open beside the model */
  const [besideBackend, setBesideBackend] = useState<string | null>(null);
  /* New backend… from the Backend field: the onboarding's provider step, in place */
  const [addingBackend, setAddingBackend] = useState<
    ((id: string | null) => void) | null
  >(null);
  if (addingBackend) {
    const before = new Set(deployment.inferenceBackends.map((b) => b.backendId));
    return (
      <SetupScreen
        initialStep="inference"
        purpose="add-backend"
        agentDid={deployment.agentDid}
        onCancel={() => {
          addingBackend(null);
          setAddingBackend(null);
        }}
        onDone={(snapshot) => {
          const added = (snapshot.client?.deployments ?? [])
            .find((x) => x.agentDid === deployment.agentDid)
            ?.inferenceBackends.find((b) => !before.has(b.backendId));
          addingBackend(added?.backendId ?? null);
          setAddingBackend(null);
        }}
      />
    );
  }
  const usedBy = deployment.behaviors.filter(
    (b) => b.inferenceProfileId === profile.profile_id,
  );
  return (
    <>
      <BackendSheet
        deployment={deployment}
        backendId={besideBackend}
        onClose={() => setBesideBackend(null)}
      />
      {!embedded && (
        <header className="mb-6">
          <h2 className="font-heading text-lg text-heading">
            {profile.display_name ?? profile.profile_id}
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            {modelSentence(deployment, profile)}
          </p>
        </header>
      )}
      <Group title="Model">
        {!draftMode && (
          <FactRow label="Profile ID" mono>
            {profile.profile_id}
          </FactRow>
        )}
        <TextRow
          id={id("name")}
          label="Display name"
          value={d.draft.displayName}
          onChange={(v) => d.set("displayName", v)}
        />
        <Disclosure summary="Description" summaryClassName="px-4 py-3">
          <AreaRow
            id={id("description")}
            label="Description"
            value={d.draft.description}
            onChange={(v) => d.set("description", v)}
            rows={2}
          />
        </Disclosure>
        <RefRow
          id={id("backend")}
          label="Backend"
          description="The provider and endpoint the model is served from."
          value={d.draft.backendId}
          error={problems.backendId}
          onChange={(v) => {
            const modelName =
              deployment.inferenceBackends.find((backend) => backend.backendId === v)
                ?.models[0] ?? "";
            beginModelSelection(v, modelName);
            d.set("backendId", v);
            d.set("modelName", modelName);
          }}
          /* a disabled or missing account's backend only while it is the current one */
          items={deployment.inferenceBackends
            .filter(
              (b) =>
                b.backendId === d.draft.backendId || profileBackend(accounts, b).usable,
            )
            .map((b) => ({
              value: b.backendId,
              label: profileBackend(accounts, b).label,
            }))}
          createLabel="New backend…"
          onCreate={() =>
            new Promise<string | null>((resolve) => {
              setAddingBackend(() => resolve);
            })
          }
          onOpen={(backendId) => setBesideBackend(backendId)}
        />
        <ChoiceRow
          id={id("model")}
          label="Model"
          value={d.draft.modelName}
          error={problems.modelName}
          onChange={(v) => {
            beginModelSelection(d.draft.backendId, v);
            d.set("modelName", v);
          }}
          items={[
            ...new Set([
              d.draft.modelName,
              ...(deployment.inferenceBackends.find(
                (backend) => backend.backendId === d.draft.backendId,
              )?.models ?? []),
            ]),
          ]
            .filter(Boolean)
            .map((model) => ({ value: model, label: model }))}
        />
        <FactRow label="Used by">
          {usedBy.length ? (
            <span className="flex flex-wrap justify-end gap-x-3 gap-y-1">
              {usedBy.slice(0, USERS_SHOWN).map((b) => (
                <a
                  key={b.behaviorId}
                  href={href({ ...base, section: "behaviors", item: b.behaviorId })}
                  className="max-w-48 truncate underline-offset-2 hover:text-foreground hover:underline"
                >
                  {b.displayName}
                </a>
              ))}
              {usedBy.length > USERS_SHOWN && (
                <span>and {usedBy.length - USERS_SHOWN} more</span>
              )}
            </span>
          ) : (
            "No behavior yet. Pick it under a behavior’s Model."
          )}
        </FactRow>
      </Group>
      {recommendation && guided ? (
        <Group title="Model-aware defaults">
          <div className="px-5 py-4">
            <InferenceModelControls
              recommendation={recommendation}
              value={guided}
              onChange={updateGuided}
              expanded={customize}
              onExpandedChange={setCustomize}
              includeConcurrency={false}
              alwaysExpanded
            />
          </div>
          {/* The backend does not advertise these limits for this model, so
              there is no model-aware bound; the profile value still applies. */}
          {!recommendation.contextWindow && (
            <NumberRow
              id={id("context-window")}
              label={PROFILE_NUMBERS.contextWindow.label}
              error={problems.contextWindow}
              description={
                advertisedModel?.max_context_window
                  ? `Tokens per request, up to ${advertisedModel.max_context_window.toLocaleString()}. Empty uses the runtime default.`
                  : "Tokens the model accepts per request. Empty uses the runtime default."
              }
              value={d.draft.contextWindow}
              onChange={(v) => d.set("contextWindow", v)}
            />
          )}
          {!recommendation.maxOutputTokens && (
            <NumberRow
              id={id("max-output")}
              label={PROFILE_NUMBERS.maxOutputTokens.label}
              error={problems.maxOutputTokens}
              description="Empty uses the runtime default."
              value={d.draft.maxOutputTokens}
              onChange={(v) => d.set("maxOutputTokens", v)}
            />
          )}
        </Group>
      ) : null}
      {recommendationError ? (
        <p role="alert" className="mb-4 text-sm text-destructive">
          {recommendationError}
        </p>
      ) : null}
      <Group title="Execution">
        {executionDefaultsError && (
          <p role="alert" className="px-4 py-3 text-sm text-destructive">
            Couldn’t read the runtime’s execution defaults: {executionDefaultsError}
          </p>
        )}
        <TextRow
          id={id("execution")}
          label="Execution document ID"
          description="Reuse an existing ID or enter a new one for these limits."
          value={d.draft.executionId}
          error={problems.executionId}
          placeholder="Created when limits are customized"
          onChange={(v) => d.set("executionId", v)}
          mono
        />
        <NumberRow
          id={id("max-turns")}
          label={PROFILE_NUMBERS.maxTurns.label}
          error={problems.maxTurns}
          value={
            editedExecution.has("maxTurns")
              ? d.draft.maxTurns
              : d.draft.maxTurns ||
                (executionDefaults.maxTurns == null
                  ? ""
                  : String(executionDefaults.maxTurns))
          }
          onChange={(v) => setExecution("maxTurns", v)}
        />
        <NumberRow
          id={id("max-total")}
          label={PROFILE_NUMBERS.maxTotalTokens.label}
          error={problems.maxTotalTokens}
          value={d.draft.maxTotalTokens}
          placeholder={executionDefault("maxTotalTokens")}
          onChange={(v) => setExecution("maxTotalTokens", v)}
        />
        <NumberRow
          id={id("batch")}
          label={PROFILE_NUMBERS.streamBatchMs.label}
          error={problems.streamBatchMs}
          value={
            editedExecution.has("streamBatchMs")
              ? d.draft.streamBatchMs
              : d.draft.streamBatchMs ||
                (executionDefaults.streamBatchMs == null
                  ? ""
                  : String(executionDefaults.streamBatchMs))
          }
          onChange={(v) => setExecution("streamBatchMs", v)}
        />
        <NumberRow
          id={id("liveness")}
          label={PROFILE_NUMBERS.streamLivenessSecs.label}
          error={problems.streamLivenessSecs}
          value={
            editedExecution.has("streamLivenessSecs")
              ? d.draft.streamLivenessSecs
              : d.draft.streamLivenessSecs ||
                (executionDefaults.streamLivenessSecs == null
                  ? ""
                  : String(executionDefaults.streamLivenessSecs))
          }
          onChange={(v) => setExecution("streamLivenessSecs", v)}
        />
        <NumberRow
          id={id("provider-idle")}
          label={PROFILE_NUMBERS.providerIdleSecs.label}
          error={problems.providerIdleSecs}
          value={
            editedExecution.has("providerIdleSecs")
              ? d.draft.providerIdleSecs
              : d.draft.providerIdleSecs ||
                (executionDefaults.providerIdleSecs == null
                  ? ""
                  : String(executionDefaults.providerIdleSecs))
          }
          onChange={(v) => setExecution("providerIdleSecs", v)}
        />
        <NumberRow
          id={id("deadline")}
          label={PROFILE_NUMBERS.deadlineSecs.label}
          error={problems.deadlineSecs}
          value={
            editedExecution.has("deadlineSecs")
              ? d.draft.deadlineSecs
              : d.draft.deadlineSecs ||
                (executionDefaults.deadlineSecs == null
                  ? ""
                  : String(executionDefaults.deadlineSecs))
          }
          onChange={(v) => setExecution("deadlineSecs", v)}
        />
        <TextRow
          id={id("retry")}
          label="Retry policy ID"
          value={d.draft.retryPolicyId}
          placeholder="Runtime default"
          onChange={(v) => d.set("retryPolicyId", v)}
          mono
        />
      </Group>
      <Group title="Metadata">
        <TagsRow
          id={id("tags")}
          label="Tags"
          value={d.draft.tags}
          onChange={(v) => d.set("tags", v)}
        />
      </Group>
      <DraftActions
        draft={d}
        problems={problems}
        fields={{
          backendId: id("backend"),
          modelName: id("model"),
          contextWindow: id("context-window"),
          maxOutputTokens: id("max-output"),
          executionId: id("execution"),
          maxTurns: id("max-turns"),
          maxTotalTokens: id("max-total"),
          streamBatchMs: id("batch"),
          streamLivenessSecs: id("liveness"),
          providerIdleSecs: id("provider-idle"),
          deadlineSecs: id("deadline"),
        }}
        saveLabel={draftMode ? "Create" : undefined}
        onSave={() =>
          draftMode
            ? void d.save().then((ok) => ok && draftMode.onSaved(profile.profile_id))
            : d.save()
        }
        onCancel={() => {
          if (draftMode) {
            draftMode.onCancel();
            return;
          }
          setEditedExecution(new Set());
          setEditedModelFields(new Set());
          d.reset();
        }}
      />
      {!embedded && !draftMode && (
        <DeleteButton
          label={profile.display_name ?? profile.profile_id}
          warning={dependentsWarning(deployment, "profile", profile.profile_id)}
          base={base}
          onDelete={() =>
            changeConfig("deleteInferenceProfileConfig", {
              profileId: profile.profile_id,
              agentDid: deployment.agentDid,
            })
          }
        />
      )}
    </>
  );
}

/* "claude-sonnet-5 · via Anthropic · 2 behaviors" */
export function modelSentence(deployment: NodeView, p: InferenceProfile) {
  const backend = deployment.inferenceBackends.find(
    (b) => b.backendId === p.backend_id,
  );
  const users = deployment.behaviors.filter(
    (b) => b.inferenceProfileId === p.profile_id,
  ).length;
  return [
    p.model_name,
    `via ${backend?.name ?? p.backend_id}`,
    users ? `${users} ${users === 1 ? "behavior" : "behaviors"}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
}
