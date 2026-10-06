/* How the inference step connects to the chosen provider: its connection
   method, then a sign-in for a subscription or a key and endpoint, and the
   way on to its models. */
import type { Dispatch } from "react";
import { CircleCheck } from "lucide-react";
import type {
  InferenceAuthMethod,
  InferenceProviderOption,
} from "@source-inc/gents-desktop-client";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import { Spinner } from "@gents/ui/components/spinner";
import { openExternalUrl } from "../../../lib/externalLinks";
import { Field } from "./parts";
import {
  oauthProviderFor,
  type ConnectionDraft,
  type ProviderId,
  type SetupEvent,
  type SetupForm,
} from "./inferenceSetupForm";

const authLabel = (method: InferenceAuthMethod) =>
  ({
    chat_gpt_oauth: "ChatGPT sign-in",
    api_key: "API key",
    claude_oauth: "Claude sign-in",
    grok_oauth: "Grok sign-in",
    optional_api_key: "Endpoint + optional key",
  })[method];

export function ConnectionFields({
  form,
  dispatch,
  provider,
  connection,
  option,
  purpose,
  canRetrySave,
  ops,
}: {
  form: SetupForm;
  dispatch: Dispatch<SetupEvent>;
  provider: ProviderId;
  connection: ConnectionDraft | undefined;
  option: InferenceProviderOption | undefined;
  purpose: "onboarding" | "add-backend";
  /** the bridge can retry saving a sign-in it holds */
  canRetrySave: boolean;
  ops: {
    updateConnection: (
      changes: Partial<ConnectionDraft>,
      options?: { autoSignIn?: boolean },
    ) => void;
    signIn: () => void;
    cancelSignIn: () => void;
    retrySave: () => void;
    discover: () => void;
  };
}) {
  const busy = form.op !== null;
  const accountOp = form.op === "signIn" || form.op === "retrySave" ? form.op : null;
  const { accountLabel, signInHint, authUrl } = form;
  const { signedIn, pendingSave } = form.accounts;
  const discovery = form.model.discovery;
  const authOptions = option?.authOptions ?? [];
  const oauthProvider = connection ? oauthProviderFor(connection.authMethod) : null;
  const connectionReady = Boolean(
    connection?.endpoint.trim() &&
    (oauthProvider
      ? signedIn[provider]
      : connection.authMethod === "optional_api_key" || connection.apiKey.trim()),
  );
  return (
    <>
      <div className="grid gap-3 text-sm">
        {connection && authOptions.length > 1 ? (
          <Field label="Connection method">
            <Select
              items={authOptions.map((option) => ({
                value: option.method,
                label: option.displayName,
              }))}
              disabled={busy}
              value={connection.authMethod}
              onValueChange={(next) => {
                if (!next) return;
                const option = authOptions.find((item) => item.method === next);
                ops.updateConnection(
                  {
                    authMethod: next as InferenceAuthMethod,
                    endpoint: option?.defaultEndpoint ?? connection.endpoint,
                    apiKey: "",
                  },
                  { autoSignIn: true },
                );
              }}
            >
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {authOptions.map((option) => (
                  <SelectItem key={option.method} value={option.method}>
                    {option.displayName}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Field>
        ) : null}
        {oauthProvider ? (
          <div className="grid gap-3">
            {signedIn[provider] ? (
              <p className="flex items-center gap-2">
                <CircleCheck className="size-4" />
                Account connected
              </p>
            ) : (
              <>
                {purpose === "add-backend" ? (
                  <Field label="Account label">
                    <Input
                      disabled={busy}
                      value={accountLabel}
                      onChange={(event) =>
                        dispatch({ type: "labelEdited", label: event.target.value })
                      }
                      placeholder="Optional, e.g. Work"
                    />
                  </Field>
                ) : null}
                <div className="flex items-center justify-between gap-3">
                  <p className="text-muted-foreground">
                    {authLabel(connection!.authMethod)}
                  </p>
                  <span className="flex gap-2">
                    {accountOp === "signIn" && (
                      <Button variant="outline" onClick={ops.cancelSignIn}>
                        Cancel
                      </Button>
                    )}
                    {accountOp !== "signIn" &&
                      pendingSave[provider] &&
                      canRetrySave && (
                        <Button variant="brand" disabled={busy} onClick={ops.retrySave}>
                          {accountOp === "retrySave" && <Spinner />}
                          {accountOp === "retrySave" ? "Saving…" : "Retry save"}
                        </Button>
                      )}
                    <Button
                      variant={pendingSave[provider] && !busy ? "outline" : "brand"}
                      disabled={busy}
                      onClick={ops.signIn}
                    >
                      {accountOp === "signIn" && <Spinner />}
                      {accountOp === "signIn" ? "Waiting…" : "Sign in"}
                    </Button>
                  </span>
                </div>
              </>
            )}
            {signInHint ? <p className="text-muted-foreground">{signInHint}</p> : null}
            {authUrl ? (
              <button
                type="button"
                className="justify-self-start text-xs underline"
                onClick={() => void openExternalUrl(authUrl)}
              >
                Open the sign-in page
              </button>
            ) : null}
          </div>
        ) : (
          <>
            <Field
              label={
                connection?.authMethod === "optional_api_key"
                  ? "API key (optional)"
                  : "API key"
              }
            >
              <Input
                type="password"
                disabled={busy}
                value={connection?.apiKey ?? ""}
                onChange={(event) =>
                  ops.updateConnection({ apiKey: event.target.value })
                }
                placeholder="Stored only when you save"
              />
            </Field>
            <Field label="Endpoint">
              <Input
                disabled={busy}
                value={connection?.endpoint ?? ""}
                className="font-mono"
                onChange={(event) =>
                  ops.updateConnection({ endpoint: event.target.value })
                }
              />
            </Field>
          </>
        )}
      </div>
      <Button
        variant="outline"
        disabled={busy || !connectionReady}
        onClick={ops.discover}
      >
        {busy ? <Spinner /> : null}{" "}
        {discovery
          ? "Refresh models"
          : oauthProvider && signedIn[provider]
            ? "Find models"
            : "Connect and find models"}
      </Button>
    </>
  );
}
