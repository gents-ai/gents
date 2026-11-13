import type { Dispatch } from "react";
import { ArrowRight, RefreshCw } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { InferenceModelControls } from "../inference/InferenceModelControls";
import { Field, IconAction } from "./parts";
import type { SetupEvent, SetupForm } from "./inferenceSetupForm";
import { SEARCH_FIELD } from "@/lib/textAssist";

export function ModelChoice({
  choice,
  busy,
  dispatch,
  purpose,
  onDiscover,
  onDescribe,
  onSave,
}: {
  choice: SetupForm["model"];
  busy: boolean;
  dispatch: Dispatch<SetupEvent>;
  purpose: "onboarding" | "add-backend";
  onDiscover: () => void;
  onDescribe: () => void;
  onSave: () => void;
}) {
  const {
    discovery,
    search: modelSearch,
    pickerOpen: modelPickerOpen,
    name: model,
    manual: manualModel,
    recommendation: selectedRecommendation,
    settings,
    customize,
  } = choice;
  const advertised = discovery?.models.find(
    (option) => option.advertised.model_name === model,
  )?.advertised;
  const contextWindow =
    advertised?.context_window ?? selectedRecommendation?.contextWindow?.recommended;
  const maxOutput =
    discovery?.providerKind === "ChatGptCodex"
      ? null
      : (advertised?.max_output_tokens ?? selectedRecommendation?.maxOutputTokens?.max);
  const limits = selectedRecommendation
    ? [
        contextWindow ? `${contextWindow.toLocaleString()} context` : null,
        discovery?.providerKind === "ChatGptCodex"
          ? "max output set by the provider"
          : maxOutput
            ? `${maxOutput.toLocaleString()} max output`
            : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : "";
  const filteredModels =
    discovery?.models.filter((option) => {
      const query = modelSearch.trim().toLocaleLowerCase();
      return (
        !query ||
        option.advertised.model_name.toLocaleLowerCase().includes(query) ||
        option.advertised.display_name?.toLocaleLowerCase().includes(query)
      );
    }) ?? [];
  return (
    <>
      {discovery ? (
        <section
          className="grid animate-in gap-3 border-t border-border/60 pt-4 duration-300 ease-out fade-in slide-in-from-top-2 motion-reduce:animate-none"
          aria-label="Model selection"
        >
          <div className="flex items-center justify-between gap-3">
            <h2 className="text-sm font-medium">Choose a model</h2>
            <IconAction
              label="Refresh models"
              icon={RefreshCw}
              disabled={busy}
              onClick={onDiscover}
            />
          </div>
          {discovery?.failure ? (
            <div className="mb-4 rounded-xl border border-destructive/30 p-3">
              <p className="text-sm text-destructive">{discovery.failure.message}</p>
              <Button
                className="mt-3"
                variant="outline"
                disabled={busy}
                onClick={onDiscover}
              >
                Retry connection
              </Button>
            </div>
          ) : null}
          {discovery?.models.length ? (
            model && !modelPickerOpen ? (
              <div className="flex items-center justify-between gap-3 rounded-xl border border-border/60 p-3">
                <span className="grid min-w-0 gap-0.5">
                  <span className="break-words text-sm font-medium">{model}</span>
                  {limits ? (
                    <span className="text-xs text-muted-foreground">{limits}</span>
                  ) : null}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  disabled={busy}
                  onClick={() => dispatch({ type: "pickerOpened" })}
                >
                  Change model
                </Button>
              </div>
            ) : (
              <div className="grid gap-3">
                <Input
                  disabled={busy}
                  value={modelSearch}
                  onChange={(event) =>
                    dispatch({ type: "searchEdited", search: event.target.value })
                  }
                  {...SEARCH_FIELD}
                  placeholder="Search advertised models"
                  aria-label="Search advertised models"
                />
                <div
                  role="listbox"
                  aria-label="Advertised models"
                  className="max-h-40 overflow-y-auto rounded-xl border border-border/60 p-1"
                >
                  {filteredModels.map((option) => (
                    <button
                      key={option.advertised.model_name}
                      type="button"
                      role="option"
                      disabled={busy}
                      aria-selected={model === option.advertised.model_name}
                      className={cn(
                        "block w-full rounded-lg px-3 py-2 text-left text-sm",
                        model === option.advertised.model_name
                          ? "bg-accent text-foreground"
                          : "hover:bg-accent/60",
                      )}
                      onClick={() => dispatch({ type: "modelChosen", option })}
                    >
                      <span className="block font-medium">
                        {option.advertised.display_name ?? option.advertised.model_name}
                      </span>
                      {option.advertised.display_name ? (
                        <span className="block font-mono text-xs text-muted-foreground">
                          {option.advertised.model_name}
                        </span>
                      ) : null}
                    </button>
                  ))}
                </div>
              </div>
            )
          ) : discovery?.manualEntryAllowed ? (
            <div className="grid gap-3">
              <p className="text-sm text-muted-foreground">
                Model discovery is unavailable. Manual entry is enabled as an explicit
                fallback and will be saved exactly as entered.
              </p>
              <Field label="Manual model identifier">
                <Input
                  disabled={busy}
                  value={model}
                  onChange={(event) =>
                    dispatch({ type: "manualModelTyped", name: event.target.value })
                  }
                  placeholder="Exact served model ID"
                />
              </Field>
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">No discovery result.</p>
          )}
          {manualModel && !selectedRecommendation ? (
            <Button
              variant="outline"
              disabled={busy || !model.trim()}
              onClick={onDescribe}
            >
              {busy ? <Spinner /> : null} Load model defaults
            </Button>
          ) : null}
        </section>
      ) : null}
      {selectedRecommendation && settings ? (
        <section
          className="grid animate-in gap-3 duration-300 ease-out fade-in slide-in-from-top-2 motion-reduce:animate-none"
          aria-label="Model defaults"
        >
          <fieldset disabled={busy} className="min-w-0">
            {selectedRecommendation && settings ? (
              <InferenceModelControls
                recommendation={selectedRecommendation}
                value={settings}
                onChange={(next) =>
                  dispatch({ type: "settingsEdited", settings: next })
                }
                expanded={customize}
                onExpandedChange={(expanded) =>
                  dispatch({ type: "customizeToggled", expanded })
                }
              />
            ) : null}
          </fieldset>
          <Button
            data-testid="setup-save-inference"
            variant="brand"
            disabled={busy}
            onClick={onSave}
          >
            {busy ? <Spinner /> : null}{" "}
            {purpose === "add-backend" ? "Save backend" : "Continue"} <ArrowRight />
          </Button>
        </section>
      ) : null}
    </>
  );
}
