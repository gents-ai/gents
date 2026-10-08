import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { InferenceSetup } from "./InferenceSetup";
import type { ProviderId } from "./inferenceSetupForm";
import { OnboardingWizard } from "./OnboardingWizard";

/** Setup, in either of its flows: first run (from where the agent lives
    through its first provider, or straight to the provider when the agent
    exists), or adding a backend to an agent from its settings. */
export function SetupScreen({
  onDone,
  initialStep,
  purpose = "onboarding",
  agentDid,
  onCancel,
  provider,
}: {
  onDone: (snapshot: DesktopClientSnapshot) => void;
  initialStep?: "welcome" | "starting" | "inference";
  purpose?: "onboarding" | "add-backend";
  agentDid?: string;
  onCancel?: () => void;
  /* a catalog row was chosen, so the form is that provider's inputs only */
  provider?: ProviderId;
}) {
  if (purpose === "add-backend")
    return (
      <InferenceSetup
        purpose="add-backend"
        checkRuntime
        onDone={onDone}
        agentDid={agentDid}
        onCancel={onCancel}
        provider={provider}
      />
    );
  return (
    <OnboardingWizard
      onDone={onDone}
      initialStep={initialStep}
      agentDid={agentDid}
      onCancel={onCancel}
      provider={provider}
    />
  );
}
