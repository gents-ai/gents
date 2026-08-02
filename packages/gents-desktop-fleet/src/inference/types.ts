import type {
  BackendSaveRequest,
  BehaviorSaveRequest,
  CodexLoginResult,
  DeploymentView,
  GrokLoginResult,
  InferenceProbeResult,
} from "@source-inc/gents-desktop-client";

export type InferenceSetupOptions = {
  deployment: DeploymentView;
  onClose: () => void;
  onSaveBackendConfig: (request: BackendSaveRequest) => Promise<unknown>;
  onSaveBehaviorConfig: (request: BehaviorSaveRequest) => Promise<unknown>;
  onProbeInferenceEndpoint: (endpoint: string) => Promise<InferenceProbeResult>;
  onCodexLogin: (agentDid: string) => Promise<CodexLoginResult>;
  /** Abort a ChatGPT sign-in whose browser was closed, so it does not hang. */
  onCancelCodexLogin?: () => Promise<unknown>;
  onCodexLoginUrl?: (
    onUrl: (url: string | null) => void,
  ) => Promise<() => void>;
  onGrokLogin?: (agentDid: string) => Promise<GrokLoginResult>;
  onCancelGrokLogin?: () => Promise<unknown>;
  onGrokLoginUrl?: (
    onUrl: (url: string | null) => void,
  ) => Promise<() => void>;
};
