import { InferenceWizardContent } from "./inference/InferenceWizardContent.js";
import {
  useInferenceSetup,
  type InferenceSetupOptions,
} from "./inference/useInferenceSetup.js";

export type InferenceSetupWizardProps = InferenceSetupOptions;

export function InferenceSetupWizard(props: InferenceSetupWizardProps) {
  const setup = useInferenceSetup(props);

  return (
    <div
      className="dialog-backdrop viewport-overlay open"
      role="presentation"
      onClick={setup.cancelAndClose}
    >
      <div
        className="dialog inference-wizard viewport-overlay-surface"
        data-scroll-owner="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="inference-wizard-title"
        data-testid="inference-wizard"
        onClick={(event) => event.stopPropagation()}
      >
        <InferenceWizardContent
          deploymentLabel={props.deployment.label}
          setup={setup}
        />
        <footer className="inference-wizard-footer">
          <button
            className="ghost-button"
            data-testid="inference-wizard-close"
            type="button"
            onClick={setup.cancelAndClose}
          >
            {setup.done ? "Done" : "Cancel"}
          </button>
        </footer>
      </div>
    </div>
  );
}
