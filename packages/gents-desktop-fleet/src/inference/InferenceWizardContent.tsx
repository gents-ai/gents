import {
  ChooseStep,
  CodexStep,
  CustomStep,
  GrokStep,
  LocalStep,
  OpenAiStep,
} from "./steps.js";
import type { InferenceSetupController } from "./useInferenceSetup.js";

export function InferenceWizardContent({
  deploymentLabel,
  setup,
}: {
  deploymentLabel: string;
  setup: InferenceSetupController;
}) {
  return (
    <>
      <header className="inference-wizard-header">
        <h3 id="inference-wizard-title">Set up inference</h3>
        <p className="muted">
          Choose how <strong>{deploymentLabel}</strong> runs model inference.
        </p>
      </header>
      <div className="inference-wizard-body">
        {setup.done ? (
          <DoneState setup={setup} />
        ) : setup.step === "choose" ? (
          <ChooseStep detection={setup.detection} onPick={setup.setStep} />
        ) : (
          <button
            className="inference-wizard-back"
            type="button"
            disabled={setup.submitting}
            onClick={setup.backToOptions}
          >
            ← Back to options
          </button>
        )}

        {setup.error ? (
          <p
            className="inference-wizard-error"
            data-testid="inference-wizard-error"
          >
            {setup.error}
          </p>
        ) : null}
        {!setup.done ? <ActiveStep setup={setup} /> : null}
      </div>
    </>
  );
}

function DoneState({ setup }: { setup: InferenceSetupController }) {
  return (
    <div className="inference-wizard-done" data-testid="inference-wizard-done">
      <p className="inference-wizard-success">Inference is set up.</p>
      <p className="muted">{setup.done}</p>
      {setup.codexResult ? (
        <p className="muted">
          Signed in
          {setup.codexResult.chatgptPlanType
            ? ` · ${setup.codexResult.chatgptPlanType} plan`
            : ""}
          {setup.codexResult.accountId
            ? ` · ${setup.codexResult.accountId}`
            : ""}
        </p>
      ) : null}
      {setup.grokResult ? (
        <p className="muted">
          Signed in with Grok · credential {setup.grokResult.credentialId}
        </p>
      ) : null}
    </div>
  );
}

function ActiveStep({ setup }: { setup: InferenceSetupController }) {
  switch (setup.step) {
    case "openai":
      return (
        <OpenAiStep
          apiKey={setup.openaiKey}
          model={setup.openaiModel}
          submitting={setup.submitting}
          onApiKeyChange={setup.setOpenaiKey}
          onModelChange={setup.setOpenaiModel}
          onSubmit={() => void setup.submitOpenai()}
        />
      );
    case "local":
      return (
        <LocalStep
          detection={setup.detection}
          model={setup.localModel}
          submitting={setup.submitting}
          url={setup.localUrl}
          onDetect={() => void setup.reprobeLocal()}
          onModelChange={setup.setLocalModel}
          onSubmit={() => void setup.submitLocal()}
          onUrlChange={setup.setLocalUrl}
        />
      );
    case "custom":
      return (
        <CustomStep
          apiKey={setup.customKey}
          model={setup.customModel}
          submitting={setup.submitting}
          url={setup.customUrl}
          onApiKeyChange={setup.setCustomKey}
          onModelChange={setup.setCustomModel}
          onSubmit={() => void setup.submitCustom()}
          onUrlChange={setup.setCustomUrl}
        />
      );
    case "grok":
      return (
        <GrokStep
          authUrl={setup.grokAuthUrl}
          signingIn={setup.signingIn}
          submitting={setup.submitting}
          onCancel={setup.cancelAndClose}
          onSubmit={() => void setup.signInWithGrok()}
        />
      );
    case "codex":
      return (
        <CodexStep
          authUrl={setup.codexAuthUrl}
          signingIn={setup.signingIn}
          submitting={setup.submitting}
          onCancel={setup.cancelAndClose}
          onSubmit={() => void setup.signInWithChatGpt()}
        />
      );
    default:
      return null;
  }
}
