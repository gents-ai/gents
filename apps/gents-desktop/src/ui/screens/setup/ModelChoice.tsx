/* The model the inference step saves: chosen from what discovery found,
   or typed by hand where it allows, and its recommended defaults. */
import type { Dispatch } from "react";
import { ArrowRight } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { InferenceModelControls } from "../inference/InferenceModelControls";
import { Field } from "./parts";
import type { SetupEvent, SetupForm } from "./inferenceSetupForm";

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
  /** asks the provider for its models again */
  onDiscover: () => void;
  /** asks for a hand-typed model's defaults */
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
          className="grid gap-3 border-t border-border/60 pt-4"
          aria-label="Model selection"
        >
          <h2 className="text-sm font-medium">Choose a model</h2>
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
                <span className="min-w-0 break-words text-sm font-medium">{model}</span>
                <Button
                  variant="outline"
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
          className="grid gap-3 border-t border-border/60 pt-4"
          aria-label="Model defaults"
        >
          <h2 className="text-sm font-medium">Model defaults</h2>
          <fieldset disabled={busy} className="min-w-0">
            <div className="mb-4 rounded-2xl border border-border/60 bg-raised p-4 text-sm">
              <p className="font-medium">{model}</p>
              <p className="mt-2 text-xs text-muted-foreground">
                Model defaults and limits
              </p>
              <dl className="mt-1 grid grid-cols-2 gap-2 text-xs">
                <div>
                  <dt className="text-muted-foreground">Default context</dt>
                  <dd>
                    {(
                      advertised?.context_window ??
                      selectedRecommendation.contextWindow?.recommended
                    )?.toLocaleString() ?? "Not advertised"}
                  </dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Max output</dt>
                  <dd>
                    {discovery?.providerKind === "ChatGptCodex"
                      ? "Provider managed"
                      : ((
                          advertised?.max_output_tokens ??
                          selectedRecommendation.maxOutputTokens?.max
                        )?.toLocaleString() ?? "Not advertised")}
                  </dd>
                </div>
              </dl>
            </div>
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
            {purpose === "add-backend" ? "Save backend" : "Save and start chatting"}{" "}
            <ArrowRight />
          </Button>
        </section>
      ) : null}
    </>
  );
}
