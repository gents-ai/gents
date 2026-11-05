import { useEffect, type ReactNode } from "react";
import { ArrowLeft, ChevronDown } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Spinner } from "@gents/ui/components/spinner";
import type {
  AgentContext,
  AgentView,
  EventSource,
  PackReviewDocument,
  Schedule,
  SkillView,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { href } from "@/lib/router";
import type { PackReviewRequest } from "@/lib/packReview";
import type { NodeView } from "../../../hooks/fleetStore";
import { FactRow, LongFactRow } from "../agent/editors";
import { EventSourceEditor } from "../agent/EventSourcesPanel";
import { Group, ReadOnlyConfig } from "../agent/rows";
import { ScheduleEditor } from "../agent/SchedulesPanel";
import { SkillEditor } from "../agent/SkillsPanel";
import { kindOf } from "../agent/packContents";
import { shortName } from "./model";
import {
  docRef,
  parseDocRef,
  previewPage,
  readPackPreview,
  requestedName,
  usePackPreview,
} from "./preview";

type Fields = Record<string, unknown>;

const fieldsOf = (d: PackReviewDocument | undefined): Fields =>
  (d?.content as Fields | undefined) ?? {};

const str = (value: unknown) => (typeof value === "string" ? value : null);
const strings = (value: unknown) =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

/** A field's name in words: `display_name` is Display name. */
const labelOf = (field: string) => {
  const words = field.replace(/_/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
};

/** One field of a document, read-only: short text in its row, long text and
    structure under its label. */
function FieldRow({ field, value }: { field: string; value: unknown }) {
  const label = labelOf(field);
  if (typeof value === "boolean")
    return <FactRow label={label}>{value ? "Yes" : "No"}</FactRow>;
  if (typeof value === "number") return <FactRow label={label}>{value}</FactRow>;
  if (typeof value === "string")
    return value.length > 80 || value.includes("\n") ? (
      <LongFactRow label={label}>{value}</LongFactRow>
    ) : (
      <FactRow label={label} mono={field.endsWith("_id")}>
        {value}
      </FactRow>
    );
  if (Array.isArray(value) && value.every((v) => typeof v === "string"))
    return <FactRow label={label}>{value.length ? value.join(", ") : "None"}</FactRow>;
  return <LongFactRow label={label}>{JSON.stringify(value, null, 2)}</LongFactRow>;
}

/* the owner a document is written under is the node it installs on, not
   something the pack chose */
const SHOWN = (field: string) => field !== "node_did";

function FieldList({ title, fields }: { title?: string; fields: Fields }) {
  const shown = Object.entries(fields).filter(([field]) => SHOWN(field));
  return (
    <Group title={title}>
      {shown.length ? (
        shown.map(([field, value]) => (
          <FieldRow key={field} field={field} value={value} />
        ))
      ) : (
        <FactRow label="Fields">None set</FactRow>
      )}
    </Group>
  );
}

const agentView = (d: PackReviewDocument, nodeDid: string): AgentView => {
  const f = fieldsOf(d);
  return {
    agentId: d.id ?? "",
    nodeDid,
    displayName: str(f.display_name) ?? d.id ?? "",
    description: str(f.description),
    contextId: str(f.context_id),
    inferenceProfileId: str(f.inference_profile_id),
    enabled: f.enabled !== false,
    isDefault: false,
    tags: strings(f.tags),
    createdAt: null,
  };
};

const skillView = (d: PackReviewDocument, nodeDid: string): SkillView => {
  const f = fieldsOf(d);
  return {
    skillId: d.id ?? "",
    nodeDid,
    name: str(f.name),
    description: str(f.description),
    instructions: str(f.instructions),
    sourceDirectory: str(f.source_directory),
    toolRefs: strings(f.tool_refs),
    displayName: str(f.display_name),
    interfaceJson: str(f.interface_json),
    enabled: typeof f.enabled === "boolean" ? f.enabled : null,
    createdAt: null,
    tags: strings(f.tags),
  };
};

/** The document's own page in the configuration, read-only, where its editor
    reads a document handed to it; the rest list their fields. An agent's
    context, and the compaction that context uses, are part of how it runs,
    so they're listed with it. */
function DocumentBody({
  document: d,
  documents,
  node,
  pack,
  request,
}: {
  document: PackReviewDocument;
  documents: readonly PackReviewDocument[];
  node: NodeView | null;
  pack: string;
  request: PackReviewRequest;
}) {
  const fields = fieldsOf(d);
  if (node && d.collection === "Schedule")
    return <ScheduleEditor deployment={node} schedule={fields as Schedule} embedded />;
  if (node && d.collection === "EventSource")
    return (
      <EventSourceEditor deployment={node} source={fields as EventSource} embedded />
    );
  if (node && d.collection === "Skill") {
    /* the agents that would use it are the pack's own */
    const contexts = documents
      .filter((x) => x.collection === "AgentContext")
      .map((x) => fieldsOf(x) as AgentContext);
    const agents = documents
      .filter((x) => x.collection === "Agent")
      .map((x) => agentView(x, node.nodeDid));
    return (
      <SkillEditor
        deployment={{ ...node, contexts, agents }}
        skill={skillView(d, node.nodeDid)}
        preview={{
          pack,
          agentHref: (agentId) => href(previewPage(request, docRef("Agent", agentId))),
        }}
      />
    );
  }
  const context =
    d.collection === "Agent"
      ? documents.find(
          (x) => x.collection === "AgentContext" && x.id === str(fields.context_id),
        )
      : undefined;
  const compaction = context
    ? documents.find(
        (x) =>
          x.collection === "CompactionConfig" &&
          x.id === str(fieldsOf(context).compaction_id),
      )
    : undefined;
  return (
    <>
      <FieldList fields={fields} />
      {context && <FieldList title="Context" fields={fieldsOf(context)} />}
      {compaction && <FieldList title="Compaction" fields={fieldsOf(compaction)} />}
    </>
  );
}

/** One document a pack would add, on its own page before the pack is
    installed: the configuration's own view of it where it reads a handed
    document, read-only, or else its fields, then the whole document. */
export function PackItemView({
  request,
  doc,
  node,
  resume,
}: {
  request: PackReviewRequest;
  doc: string;
  node: NodeView | null;
  /** reopens the review this page was opened from, as it was left */
  resume: (() => void) | null;
}) {
  const { actions } = useApp();
  const read = usePackPreview(request);
  useEffect(() => {
    void readPackPreview(actions, request);
  }, [actions, request]);
  const report = read?.state === "read" ? read.report : null;
  const ref = parseDocRef(doc);
  const documents = report?.documents ?? [];
  const document = ref
    ? documents.find((d) => d.collection === ref.collection && d.id === ref.id)
    : undefined;
  const pack = report?.coordinate ?? report?.pack ?? requestedName(request);
  const name = document?.name ?? document?.id ?? ref?.id ?? doc;
  const kind = kindOf(ref?.collection ?? "");

  let body: ReactNode;
  if (!read || read.state === "reading")
    body = (
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        <Spinner /> Reading the pack
      </p>
    );
  else if (read.state === "unread")
    body = (
      <p role="alert" className="text-sm text-warning">
        Couldn't read this pack.{" "}
        <span className="text-muted-foreground">{read.error}</span>
      </p>
    );
  else if (!document)
    body = <p className="text-sm text-muted-foreground">This pack doesn't add it.</p>;
  else
    body = (
      <>
        <ReadOnlyConfig readOnly>
          <DocumentBody
            key={doc}
            document={document}
            documents={documents}
            node={node}
            pack={pack}
            request={request}
          />
        </ReadOnlyConfig>
        <Collapsible className="group/fields">
          <CollapsibleTrigger className="inline-flex cursor-pointer items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
            All fields
            <ChevronDown className="size-3 transition-transform duration-150 group-data-open/fields:rotate-180 motion-reduce:transition-none" />
          </CollapsibleTrigger>
          <CollapsibleContent>
            <ScrollArea className="mt-2 max-h-96 rounded-xl border border-border/60 bg-background">
              <pre className="px-3 py-2 font-mono text-[11px] leading-relaxed wrap-break-word whitespace-pre-wrap text-muted-foreground">
                {JSON.stringify(document.content ?? {}, null, 2)}
              </pre>
            </ScrollArea>
          </CollapsibleContent>
        </Collapsible>
      </>
    );

  return (
    <article className="grid gap-8" data-testid="pack-item-view">
      <a
        href={href(previewPage(request))}
        className="inline-flex w-fit items-center gap-1 text-xs text-muted-foreground hover:text-foreground"
      >
        <ArrowLeft className="size-3" /> {shortName(pack)}
      </a>
      <header className="flex items-start gap-4">
        <div className="grid min-w-0 flex-1 gap-1">
          <h1 className="font-heading text-lg font-medium wrap-break-word text-heading">
            {name}
          </h1>
          <p className="text-xs text-muted-foreground">
            {`${kind.charAt(0).toUpperCase()}${kind.slice(1)} · From ${shortName(pack)} · not installed`}
          </p>
        </div>
        {resume && (
          <Button size="sm" variant="brand" className="shrink-0" onClick={resume}>
            Back to install
          </Button>
        )}
      </header>
      <div className="grid gap-4">{body}</div>
    </article>
  );
}
