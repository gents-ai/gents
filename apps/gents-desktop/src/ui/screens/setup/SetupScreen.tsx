import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

import { InferenceSetup } from "./InferenceSetup";
import type { ProviderId } from "./inferenceSetupForm";
import { OnboardingWizard } from "./OnboardingWizard";

export function SetupScreen({
  onDone,
  initialStep,
  purpose = "onboarding",
  nodeDid,
  onCancel,
  provider,
}: {
  onDone: (snapshot: DesktopClientSnapshot, nodeDid?: string) => void;
  initialStep?: "welcome" | "starting" | "inference";
  purpose?: "onboarding" | "add-backend";
  nodeDid?: string;
  onCancel?: () => void;
  provider?: ProviderId;
}) {
  if (purpose === "add-backend")
    return (
      <InferenceSetup
        purpose="add-backend"
        checkRuntime
        onDone={onDone}
        nodeDid={nodeDid}
        onCancel={onCancel}
        provider={provider}
      />
    );
  return (
    <OnboardingWizard
      onDone={onDone}
      initialStep={initialStep}
      nodeDid={nodeDid}
      onCancel={onCancel}
      provider={provider}
    />
  );
}
