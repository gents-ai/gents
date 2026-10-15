import { useEffect, useRef, useState } from "react";

import type {
  BackendProviderKind,
  InferenceBackendRecommendationRequest,
  InferenceBackendView,
  InferenceModelRecommendation,
} from "@source-inc/gents-desktop-client";

type AdvertisedModel = NonNullable<InferenceBackendView["advertisedModels"]>[number];

/**
 * The settings a backend recommends for a model, asked for 150ms after the
 * backend, the model or what the backend advertises about it last changed.
 * An answer is held with what it was asked about, so it shows only while
 * that is still the question: no stale recommendation (or error) outlives
 * a change, and none is shown while the next is out.
 *
 * `choose` marks a model the person just picked; when its recommendation
 * lands, `onChosen` applies it, where a recommendation for a model that was
 * already set leaves the draft's own values alone.
 */
export function useModelRecommendation({
  recommend,
  backendId,
  modelName,
  backend,
  advertised,
  onChosen,
}: {
  recommend: (
    request: InferenceBackendRecommendationRequest,
  ) => Promise<InferenceModelRecommendation>;
  backendId: string;
  modelName: string;
  backend: InferenceBackendView | undefined;
  advertised: AdvertisedModel | undefined;
  onChosen: (recommendation: InferenceModelRecommendation) => void;
}) {
  const model = modelName.trim();
  const advertisedKey = JSON.stringify(advertised ?? null);
  /* everything the request is about; the backend's concurrency is not */
  const asked = [
    backendId,
    model,
    backend?.providerKind ?? "",
    backend?.endpoint ?? "",
    advertisedKey,
  ].join("\u0000");
  const [answer, setAnswer] = useState<{
    asked: string;
    recommendation: InferenceModelRecommendation | null;
    error: string | null;
  } | null>(null);
  const chosen = useRef<string | null>(null);
  const onChosenRef = useRef(onChosen);
  useEffect(() => {
    onChosenRef.current = onChosen;
  });

  useEffect(() => {
    if (!backend?.providerKind || !backend.endpoint || !model) return;
    const providerKind = backend.providerKind as BackendProviderKind;
    const endpoint = backend.endpoint;
    const choice = `${backendId}\u0000${model}`;
    let canceled = false;
    const timeout = window.setTimeout(() => {
      void recommend({
        providerKind,
        endpoint,
        modelName: model,
        displayName: advertised?.display_name ?? null,
        // Only the backend's advertised facts describe the model.
        contextWindow: advertised?.context_window ?? null,
        maxContextWindow: advertised?.max_context_window ?? null,
        maxOutputTokens: advertised?.max_output_tokens ?? null,
        reasoningEfforts: advertised?.reasoning_efforts ?? null,
      })
        .then((recommendation) => {
          if (canceled) return;
          setAnswer({ asked, recommendation, error: null });
          if (chosen.current === choice) {
            chosen.current = null;
            onChosenRef.current(recommendation);
          }
        })
        .catch((error) => {
          if (canceled) return;
          setAnswer({
            asked,
            recommendation: null,
            error: `Model-aware settings unavailable: ${error instanceof Error ? error.message : String(error)}`,
          });
        });
    }, 150);
    return () => {
      canceled = true;
      window.clearTimeout(timeout);
    };
    // `asked` names every input the request reads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [asked, recommend]);

  const current = answer?.asked === asked ? answer : null;
  return {
    recommendation: current?.recommendation ?? null,
    error: current?.error ?? null,
    /** the person picked this model: its recommendation, when it lands, is
        applied to the draft */
    choose(nextBackendId: string, nextModelName: string) {
      chosen.current = `${nextBackendId}\u0000${nextModelName.trim()}`;
    },
  };
}
