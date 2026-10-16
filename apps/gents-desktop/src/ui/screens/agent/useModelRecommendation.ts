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
 * backend, model or advertised facts last change; an answer shows only
 * while what it was asked about is still current. `choose` marks a model
 * the person just picked; when its recommendation lands, `onChosen`
 * applies it.
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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [asked, recommend]);

  const current = answer?.asked === asked ? answer : null;
  return {
    recommendation: current?.recommendation ?? null,
    error: current?.error ?? null,
    choose(nextBackendId: string, nextModelName: string) {
      chosen.current = `${nextBackendId}\u0000${nextModelName.trim()}`;
    },
  };
}
