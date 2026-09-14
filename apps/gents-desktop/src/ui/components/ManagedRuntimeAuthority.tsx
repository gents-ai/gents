import { useEffect, useRef, useState } from "react";
import { FolderOpen } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import type { ManagedServerAuthorityInput } from "@source-inc/gents-desktop-client";
import { authoritySummary } from "@/lib/managedRuntimeAuthority";

type ToolCeiling = ManagedServerAuthorityInput["toolCeiling"];

export function ManagedRuntimeAuthorityPicker({
  home,
  toolCeiling,
  toolRoot,
  onCeilingChange,
  onRootChange,
  validateRoot,
  error,
  onError,
}: {
  home: string;
  toolCeiling: ToolCeiling;
  toolRoot: string | null;
  onCeilingChange: (ceiling: ToolCeiling) => void;
  onRootChange: (path: string | null) => void;
  validateRoot?: (path: string) => Promise<string>;
  error?: string | null;
  onError: (error: string | null) => void;
}) {
  const [validating, setValidating] = useState(false);
  const [directoryDraft, setDirectoryDraft] = useState(toolRoot ?? home);
  const input = useRef<HTMLInputElement>(null);
  const validationGeneration = useRef(0);
  useEffect(() => {
    if (toolRoot !== null) setDirectoryDraft(toolRoot);
  }, [toolRoot]);
  useEffect(
    () => () => {
      validationGeneration.current += 1;
    },
    [],
  );

  const validate = async (path: string) => {
    const generation = ++validationGeneration.current;
    setDirectoryDraft(path);
    onRootChange(null);
    onError(null);
    if (!validateRoot) {
      onError("Directory selection is unavailable on this host.");
      return;
    }
    setValidating(true);
    try {
      const canonical = await validateRoot(path);
      if (generation !== validationGeneration.current) return;
      setDirectoryDraft(canonical);
      onRootChange(canonical);
    } catch (cause) {
      if (generation !== validationGeneration.current) return;
      onError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (generation === validationGeneration.current) setValidating(false);
    }
  };
  const choose = async () => {
    if (!("__TAURI_INTERNALS__" in window)) {
      input.current?.focus();
      return;
    }
    const generation = validationGeneration.current;
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const selected = await open({
        directory: true,
        multiple: false,
        defaultPath: toolRoot ?? home,
        title: "Choose the managed runtime tool root",
      });
      if (generation !== validationGeneration.current) return;
      if (typeof selected === "string") await validate(selected);
    } catch (cause) {
      if (generation === validationGeneration.current) {
        onError(cause instanceof Error ? cause.message : String(cause));
      }
    }
  };
  return (
    <div className="grid gap-3">
      <label className="grid gap-1 text-xs text-muted-foreground">
        Tool ceiling
        <select
          className="h-9 w-full rounded-md border border-input bg-background px-3 text-sm text-foreground"
          value={toolCeiling}
          onChange={(event) => {
            onError(null);
            onCeilingChange(event.target.value as ToolCeiling);
          }}
        >
          <option value="readwrite">Read / write</option>
          <option value="readonly">Read only</option>
          <option value="meta-only">Metatools only</option>
        </select>
      </label>
      <div className="grid gap-1">
        <label className="text-xs text-muted-foreground" htmlFor="managed-tool-root">
          Tool root
        </label>
        <div className="flex gap-2">
          <Input
            ref={input}
            id="managed-tool-root"
            className="min-w-0 font-mono text-sm"
            value={directoryDraft}
            disabled={toolCeiling === "meta-only"}
            onChange={(event) => {
              validationGeneration.current += 1;
              setValidating(false);
              setDirectoryDraft(event.target.value);
              onRootChange(null);
              onError(null);
            }}
            onBlur={(event) => {
              const path = event.target.value.trim();
              if (path && path !== toolRoot) void validate(path);
            }}
            aria-invalid={Boolean(error)}
          />
          <Button
            type="button"
            variant="outline"
            disabled={toolCeiling === "meta-only"}
            onClick={() => void choose()}
            aria-label="Choose tool root folder"
          >
            <FolderOpen /> Choose…
          </Button>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        {toolCeiling === "readwrite"
          ? "Read and change files; run commands as you. The tool root is not a shell sandbox."
          : toolCeiling === "readonly"
            ? "Read files and run restricted read-only commands."
            : "No host files or commands. Configuration and separately enabled remote tools remain available."}{" "}
        Individual behaviors can use less access, never more.
      </p>
      {validating && toolCeiling !== "meta-only" ? (
        <p className="text-xs text-muted-foreground">Checking…</p>
      ) : null}
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
    </div>
  );
}

export function ManagedRuntimeAuthorityReview({
  authority,
}: {
  authority: ManagedServerAuthorityInput;
}) {
  return (
    <div className="grid gap-3 rounded-2xl border border-border/60 bg-raised p-4">
      <div>
        <p className="text-xs text-muted-foreground">Tool root</p>
        <p className="break-all font-mono text-sm">
          {authority.toolRoot ?? "No host path"}
        </p>
      </div>
      <div>
        <p className="text-xs text-muted-foreground">Process authority</p>
        <p className="text-sm">{authoritySummary(authority)}</p>
      </div>
      <p className="text-xs text-muted-foreground">
        Setup remains a narrow configurator. A behavior can reduce this ceiling but
        cannot expand it.
      </p>
    </div>
  );
}
