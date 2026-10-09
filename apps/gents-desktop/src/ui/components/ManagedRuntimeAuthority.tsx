import { useEffect, useRef, useState } from "react";
import { FolderOpen } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Input } from "@gents/ui/components/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import type { ManagedServerAuthorityInput } from "@source-inc/gents-desktop-client";
import { authoritySummary } from "@/lib/managedRuntimeAuthority";
import { pickDirectory } from "../../lib/nativeShell";

type ToolCeiling = ManagedServerAuthorityInput["toolCeiling"];

const CEILINGS: { value: ToolCeiling; label: string }[] = [
  { value: "readwrite", label: "Read / write" },
  { value: "readonly", label: "Read only" },
  { value: "meta-only", label: "Metatools only" },
];

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
  /* a committed folder becomes the draft; while one is being validated
     (null) the draft stays as typed */
  const [committedRoot, setCommittedRoot] = useState(toolRoot);
  if (toolRoot !== committedRoot) {
    setCommittedRoot(toolRoot);
    if (toolRoot !== null) setDirectoryDraft(toolRoot);
  }
  const input = useRef<HTMLInputElement>(null);
  const validationGeneration = useRef(0);
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
    const generation = validationGeneration.current;
    try {
      const selected = await pickDirectory({
        defaultPath: toolRoot ?? home,
        title: "Choose the managed runtime tool root",
      });
      if (generation !== validationGeneration.current) return;
      if (selected) await validate(selected);
      else input.current?.focus();
    } catch (cause) {
      if (generation === validationGeneration.current) {
        onError(cause instanceof Error ? cause.message : String(cause));
      }
    }
  };
  return (
    <div className="grid gap-3">
      <div className="grid gap-1">
        <span id="managed-tool-ceiling" className="text-xs text-muted-foreground">
          Tool ceiling
        </span>
        <Select
          items={CEILINGS}
          value={toolCeiling}
          onValueChange={(next) => {
            if (!next) return;
            onError(null);
            onCeilingChange(next as ToolCeiling);
          }}
        >
          <SelectTrigger className="w-full" aria-labelledby="managed-tool-ceiling">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {CEILINGS.map((ceiling) => (
              <SelectItem key={ceiling.value} value={ceiling.value}>
                {ceiling.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
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
        Individual agents can use less access, never more.
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
        Setup remains a narrow configurator. An agent can reduce this ceiling but cannot
        expand it.
      </p>
    </div>
  );
}
