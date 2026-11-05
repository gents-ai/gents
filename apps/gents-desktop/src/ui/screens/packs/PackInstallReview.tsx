import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Checkbox } from "@gents/ui/components/checkbox";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@gents/ui/components/dialog";
import { RadioGroup, RadioGroupItem } from "@gents/ui/components/radio-group";
import { Spinner } from "@gents/ui/components/spinner";
import type {
  InstalledPack,
  PackEditedChoice,
  PackInstallReport,
} from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import type { PackInstallChoice } from "../agent/usePackActions";
import { ItemIcon } from "./PackPage";
import { Section } from "./PackCard";
import {
  bytes,
  otherLabel,
  partLabel,
  readPackInfo,
  shortName,
  sizeOf,
  useInstalledPacks,
} from "./model";
import type { NodeView } from "../../../hooks/fleetStore";
import {
  fileName,
  overlapsOf,
  previewParts,
  previewPlugins,
  refusalOf,
} from "./review";
import type { PackReviewRequest } from "@/lib/packReview";
import { docRef, previewTree, requestedName, type SetAsideReview } from "./preview";

type Read =
  | { state: "reading" }
  | { state: "read"; report: PackInstallReport }
  | { state: "unread"; error: string };

/** The `NAME=PROFILE` bindings an install request carries, in a stable order. */
const slotBindings = (chosen: Record<string, string>) =>
  Object.entries(chosen)
    .filter(([, profile]) => profile)
    .map(([slot, profile]) => `${slot}=${profile}`)
    .sort();

const DT = "text-muted-foreground";

/** The review every install passes through, whether the pack comes from the
    registry, a file or a link: it previews the install, which writes
    nothing, and shows what that reports (the pack, its version and digest,
    what it adds, the collections its schemas declare, the authority its
    plugins ask for, the models its slots need) and says what the preview
    can't know. Choosing a model previews again with it. Install writes
    exactly the archive previewed, by its digest, with the models previewed;
    the person allows any authority, agrees to replace documents they made
    with the same ids, and says what happens to edited documents here; a
    document another pack installed refuses the install, and the review says
    so before it's tried. Each document it adds opens its own page, where
    the host offers that: the review is set aside with everything chosen in
    it, and `resume` opens it again as it was left. */
export function PackInstallReview({
  request,
  node,
  installed,
  onCancel,
  onInstall,
  resume = null,
  onInspect,
  inline = false,
}: {
  request: PackReviewRequest | null;
  /** the node it installs on, whose documents it may write over */
  node: NodeView | null;
  installed: readonly InstalledPack[] | null;
  onCancel: () => void;
  onInstall: (request: PackReviewRequest, choice: PackInstallChoice) => Promise<void>;
  /** the review this one was set aside as, to start from */
  resume?: SetAsideReview | null;
  /** opens one document it adds (`<collection>:<id>`), setting the review aside */
  onInspect?: (doc: string, review: Omit<SetAsideReview, "from">) => void;
  /** renders as the body of the dialog it's placed in, without its own */
  inline?: boolean;
}) {
  const { actions } = useApp();
  const { info } = useInstalledPacks();
  const [attempt, setAttempt] = useState(0);
  /* the models the person chose, by slot */
  const [chosen, setChosen] = useState<Record<string, string>>(
    () => resume?.chosen ?? {},
  );
  const bindings = slotBindings(chosen).join("\n");
  /* a read belongs to the request, attempt and models it answers; until one
     for the current ones settles, the review is reading */
  const [answer, setAnswer] = useState<{
    request: PackReviewRequest;
    attempt: number;
    bindings: string;
    read: Read;
  } | null>(() =>
    resume && request && resume.bindings !== null
      ? {
          request,
          attempt: 0,
          bindings: resume.bindings,
          read: { state: "read", report: resume.report },
        }
      : null,
  );
  /* a review taken back shows what it showed, and reads again only once
     something in it changes */
  const resumed = useRef(answer);
  /* the request's last report, kept on show while another choice of models
     is previewed or refused, so the choice can still be changed */
  const [kept, setKept] = useState<{
    request: PackReviewRequest;
    report: PackInstallReport;
  } | null>(() => (resume && request ? { request, report: resume.report } : null));
  const read: Read =
    answer &&
    answer.request === request &&
    answer.attempt === attempt &&
    answer.bindings === bindings
      ? answer.read
      : { state: "reading" };
  const [allow, setAllow] = useState(resume?.allow ?? false);
  const [replace, setReplace] = useState(resume?.replace ?? false);
  const [edited, setEdited] = useState<PackEditedChoice>(resume?.edited ?? "keep");
  const [installing, setInstalling] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);

  useEffect(() => {
    if (!request) return;
    const taken = resumed.current;
    resumed.current = null;
    if (
      taken &&
      taken.request === request &&
      taken.attempt === attempt &&
      taken.bindings === bindings
    )
      return;
    let current = true;
    const settle = (read: Read) => {
      if (!current) return;
      setAnswer({ request, attempt, bindings, read });
      if (read.state === "read") setKept({ request, report: read.report });
    };
    setRefusal(null);
    actions
      .installPack({
        package: request.package,
        preview: true,
        expectDigest: request.digest ?? null,
        inferenceSlots: bindings ? bindings.split("\n") : [],
      })
      .then(
        (report) => settle({ state: "read", report }),
        (error) => settle({ state: "unread", error: errorText(error) }),
      );
    return () => {
      current = false;
    };
  }, [actions, request, attempt, bindings]);

  /* another request starts afresh; the first is where it starts */
  const shownRequest = useRef(request);
  useEffect(() => {
    if (shownRequest.current === request) return;
    shownRequest.current = request;
    setAllow(false);
    setReplace(false);
    setEdited("keep");
    setChosen({});
  }, [request]);

  const report =
    read.state === "read"
      ? read.report
      : kept && kept.request === request && Object.keys(chosen).length > 0
        ? kept.report
        : null;
  /* a report shown while the current choice of models is still previewing,
     or was refused */
  const stale = report !== null && read.state !== "read";
  const needs = report?.inference_needs ?? null;
  const usable = needs?.profiles.filter((profile) => profile.usable) ?? [];
  const unbound = needs?.unbound ?? [];
  /* with one model to choose from, it's the choice for every slot that needs one */
  const only = usable.length === 1 ? usable[0]!.profile_id : null;
  const unboundSlots = unbound.join("\n");
  useEffect(() => {
    if (!only || !unboundSlots) return;
    setChosen((chosen) => {
      const missing = unboundSlots.split("\n").filter((slot) => !chosen[slot]);
      return missing.length
        ? { ...chosen, ...Object.fromEntries(missing.map((slot) => [slot, only])) }
        : chosen;
    });
  }, [only, unboundSlots]);
  const pack = report?.coordinate ?? report?.pack ?? request?.package ?? "";
  const version = report?.version ?? null;
  const asks = report?.authority ?? [];
  const already = installed?.find((p) => p.pack === pack) ?? null;
  const why = refusal ? refusalOf(refusal) : null;
  const choosesEdits = already !== null || why === "edited";
  const needsAllow = asks.length > 0 || why === "authority";
  const overlaps =
    report?.documents && node
      ? overlapsOf(report.documents, node, report.pack, installed ?? [])
      : [];
  const yours = overlaps.filter((overlap) => overlap.owner === null);
  const others = overlaps.filter((overlap) => overlap.owner !== null);
  /* a pack already installed shares its own collections */
  const shared = already ? [] : (report?.shared_collections ?? []);
  const registry = info[pack];
  const size =
    request?.source !== "file" && registry && registry !== "unread"
      ? sizeOf(registry as { versions?: unknown }, version)
      : null;
  const parts = report?.documents ? previewParts(report.documents) : [];
  /* contexts, compactions and targets are part of what they belong to; only
     what belongs to nothing it adds is listed, as other settings */
  const other = report?.documents ? previewTree(report).other : [];
  const { plugins, pages } = report
    ? previewPlugins(report)
    : { plugins: [], pages: [] };
  const groups = [
    ...parts.map((part) => ({
      key: part.one,
      label: partLabel(part),
      section: part.section,
      items: part.items.map((item) => ({ ...item, key: item.id })),
    })),
    ...[
      { key: "plugin", one: "plugin", many: "plugins", names: plugins },
      { key: "page", one: "interface plugin", many: "interface plugins", names: pages },
    ]
      .filter((g) => g.names.length > 0)
      .map((g) => ({
        key: g.key,
        label: `${g.names.length} ${g.names.length === 1 ? g.one : g.many}`,
        section: null,
        items: g.names.map((name) => ({
          id: name,
          key: name,
          name,
          detail: undefined as string | undefined,
          collection: undefined as string | undefined,
        })),
      })),
    ...(other.length
      ? [
          {
            key: "other",
            label: otherLabel(other.length),
            section: null,
            items: other.map((r) => ({
              id: r.id,
              key: `${r.collection}:${r.id}`,
              name: r.name,
              detail: r.kind as string | undefined,
              collection: r.collection,
            })),
          },
        ]
      : []),
  ];
  /* the registry lists a version's size; a file's isn't known before it installs */
  const fromRegistry = report !== null && request?.source !== "file";
  useEffect(() => {
    if (fromRegistry) void readPackInfo(actions, pack);
  }, [actions, fromRegistry, pack]);
  const linkMismatch =
    read.state === "unread" && request?.digest && refusalOf(read.error) === "changed";
  const ready = read.state === "read" && unbound.length === 0;

  /* the review steps aside, keeping what it showed and what was chosen */
  function inspect(collection: string, id: string) {
    if (!request || !report || !onInspect) return;
    onInspect(docRef(collection, id), {
      request,
      report,
      bindings: read.state === "read" ? bindings : null,
      chosen,
      allow,
      replace,
      edited,
    });
  }

  async function install() {
    if (!request || !report || !ready) return;
    setInstalling(true);
    setRefusal(null);
    try {
      await onInstall(request, {
        digest: report.digest,
        pack,
        edited: choosesEdits ? edited : "refuse",
        grantAuthority: allow,
        inferenceSlots: (needs?.slots ?? [])
          .filter((slot) => slot.profile)
          .map((slot) => `${slot.name}=${slot.profile}`),
      });
    } catch (error) {
      setRefusal(errorText(error));
    } finally {
      setInstalling(false);
    }
  }

  const from =
    request?.source === "file"
      ? fileName(request.package)
      : request?.source === "link"
        ? "A link"
        : "The registry";

  const Root = inline ? InlineRoot : Dialog;
  const Content = inline ? InlineContent : DialogContent;
  return (
    <Root
      open={request !== null}
      onOpenChange={(open) => {
        if (!open && !installing) onCancel();
      }}
    >
      <Content className="sm:max-w-xl" data-testid="pack-install-review">
        <DialogHeader>
          <DialogTitle>
            {report
              ? `Install ${shortName(pack)}?`
              : request
                ? `Install ${requestedName(request)}?`
                : "Install a pack?"}
          </DialogTitle>
          <DialogDescription>
            {report?.description ??
              (read.state === "reading"
                ? `Reading ${request?.source === "file" ? fileName(request.package) : pack}…`
                : "Nothing is installed until you choose Install.")}
          </DialogDescription>
        </DialogHeader>

        {read.state === "reading" && !stale && (
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner /> Reading the pack
          </div>
        )}

        {read.state === "unread" && !stale && (
          <p role="alert" className="text-sm text-warning">
            {linkMismatch
              ? "This isn't the pack the link names: its digest is different, so it isn't offered. "
              : "Couldn't read this pack. "}
            <span className="text-muted-foreground">{read.error}</span>
          </p>
        )}

        {report && (
          <div className="grid max-h-[60vh] gap-5 overflow-y-auto">
            <p className="text-sm text-muted-foreground tabular-nums">
              {[
                version ? `Version ${version}` : null,
                already &&
                  (already.installed === version
                    ? "already installed"
                    : `replaces ${already.installed}`),
                request?.source === "file"
                  ? `from ${from}`
                  : request?.source === "link"
                    ? "from a link"
                    : "from the registry",
              ]
                .filter(Boolean)
                .join(" · ")}
            </p>

            {needs && needs.slots.length > 0 && (
              <Section
                title="Models"
                action={
                  stale &&
                  read.state === "reading" && (
                    <Spinner aria-label="Checking the models" />
                  )
                }
              >
                <p className="text-sm text-muted-foreground">
                  {usable.length === 0
                    ? "This node has no model to run it yet. Set up a model first, then install this pack."
                    : "Choose the model each part of it runs on."}
                </p>
                {usable.length > 0 && (
                  <ul className="divide-y divide-border/60">
                    {needs.slots.map((slot) => (
                      <SlotRow
                        key={slot.name}
                        name={slot.name}
                        description={slot.description}
                        value={chosen[slot.name] ?? slot.profile ?? ""}
                        optional={slot.optional}
                        disabled={installing}
                        onChange={(profile) =>
                          setChosen((chosen) => ({ ...chosen, [slot.name]: profile }))
                        }
                        items={usable.map((profile) => ({
                          value: profile.profile_id,
                          label: profile.display_name || profile.model_name,
                        }))}
                      />
                    ))}
                  </ul>
                )}
                {stale && read.state === "unread" && (
                  <p role="alert" className="text-sm text-warning">
                    Couldn't use these models.{" "}
                    <span className="text-muted-foreground">{read.error}</span>
                  </p>
                )}
              </Section>
            )}

            {(overlaps.length > 0 || shared.length > 0) && (
              <Section title="Already on this node">
                {overlaps.length > 0 && (
                  <ul className="grid gap-1 text-sm">
                    {yours.map((overlap) => (
                      <li key={overlap.key}>
                        Replaces your own {overlap.kind} “{overlap.name}”
                      </li>
                    ))}
                    {others.map((overlap) => (
                      <li key={overlap.key}>
                        {`${overlap.kind.charAt(0).toUpperCase()}${overlap.kind.slice(1)} “${overlap.name}”`}{" "}
                        <span className="text-muted-foreground">
                          Already from {overlap.owner}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
                {others.length > 0 && (
                  <p className="text-sm text-warning">
                    It can't install: a pack can't replace what another pack installed.
                    Remove that pack first.
                  </p>
                )}
                {yours.length > 0 && others.length === 0 && (
                  <label className="flex items-start gap-2 text-sm">
                    <Checkbox
                      className="mt-0.5"
                      checked={replace}
                      disabled={installing}
                      onCheckedChange={(checked) => setReplace(checked === true)}
                    />
                    Replace them
                  </label>
                )}
                {shared.length > 0 && (
                  <p className="text-sm text-muted-foreground">
                    Shares {shared.join(", ")} with what's already on this node.
                  </p>
                )}
              </Section>
            )}

            {needsAllow && (
              <Section title="Access it asks for">
                {asks.length > 0 ? (
                  <ul className="grid gap-1 text-sm">
                    {asks.map((ask) => (
                      <li key={ask.plugin}>
                        <span className="font-medium">{ask.plugin}</span>{" "}
                        <span className="text-muted-foreground">{ask.asks}</span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="text-sm text-muted-foreground">
                    A plugin it installs asks for more than the review could list.
                  </p>
                )}
                <label className="flex items-start gap-2 text-sm">
                  <Checkbox
                    className="mt-0.5"
                    checked={allow}
                    disabled={installing}
                    onCheckedChange={(checked) => setAllow(checked === true)}
                  />
                  Allow this access
                </label>
              </Section>
            )}

            {choosesEdits && (
              <Section title="Your edits">
                <p className="text-sm text-muted-foreground">
                  {why === "edited"
                    ? "Some documents it installed were edited since."
                    : "If you've edited documents it installed:"}
                </p>
                <RadioGroup
                  value={edited}
                  onValueChange={(value) => setEdited(value as PackEditedChoice)}
                  aria-label="Your edits"
                  className="grid gap-2 text-sm"
                >
                  <label className="flex items-center gap-2">
                    <RadioGroupItem value="keep" disabled={installing} /> Keep my edits
                  </label>
                  <label className="flex items-center gap-2">
                    <RadioGroupItem value="overwrite" disabled={installing} /> Replace
                    them with the pack's
                  </label>
                </RadioGroup>
              </Section>
            )}

            <Section title="What it adds">
              {groups.length ? (
                <ul className="divide-y divide-border/60">
                  {groups.map((group) => (
                    <li key={group.key}>
                      <Collapsible className="group/kind">
                        <CollapsibleTrigger className="flex w-full cursor-pointer items-center gap-2 py-2 text-left text-sm">
                          <ChevronRight className="size-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-data-open/kind:rotate-90 motion-reduce:transition-none" />
                          <span className="flex-1">{group.label}</span>
                        </CollapsibleTrigger>
                        <CollapsibleContent>
                          <ul className="grid gap-2 pb-3 pl-5.5">
                            {group.items.map((item) => {
                              const inner = (
                                <>
                                  {group.section && (
                                    <ItemIcon
                                      section={group.section}
                                      name={item.name}
                                    />
                                  )}
                                  <span className="min-w-0 flex-1">
                                    <span className="block wrap-break-word">
                                      {item.name}
                                    </span>
                                    {item.detail && (
                                      <span className="block text-xs wrap-break-word text-muted-foreground">
                                        {item.detail}
                                      </span>
                                    )}
                                  </span>
                                </>
                              );
                              const opens =
                                onInspect && item.collection && item.id && !installing;
                              return (
                                <li key={item.key} title={item.id}>
                                  {opens ? (
                                    <button
                                      type="button"
                                      onClick={() => inspect(item.collection!, item.id)}
                                      className="-mx-2 flex w-[calc(100%+1rem)] cursor-pointer items-start gap-3 rounded-lg px-2 py-1 text-left text-sm transition-colors outline-none hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring/50"
                                    >
                                      {inner}
                                    </button>
                                  ) : (
                                    <span className="flex items-start gap-3 text-sm">
                                      {inner}
                                    </span>
                                  )}
                                </li>
                              );
                            })}
                          </ul>
                        </CollapsibleContent>
                      </Collapsible>
                    </li>
                  ))}
                </ul>
              ) : (
                <p className="text-sm text-muted-foreground">
                  {report.documents
                    ? "No documents"
                    : "Not reported for this kind of pack"}
                </p>
              )}
            </Section>

            <Collapsible className="group/details">
              <CollapsibleTrigger className="inline-flex cursor-pointer items-center gap-1 text-xs text-muted-foreground hover:text-foreground">
                Details
                <ChevronDown className="size-3 transition-transform duration-150 group-data-open/details:rotate-180 motion-reduce:transition-none" />
              </CollapsibleTrigger>
              <CollapsibleContent>
                <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-6 gap-y-1.5 text-sm">
                  <dt className={DT}>Pack</dt>
                  <dd className="wrap-break-word">{pack}</dd>
                  <dt className={DT}>From</dt>
                  <dd className="wrap-break-word">
                    {request?.source === "registry" ? from : request?.package}
                  </dd>
                  <dt className={DT}>Digest</dt>
                  <dd className="font-mono text-xs break-all select-all">
                    {report.digest}
                  </dd>
                  {size ? (
                    <>
                      <dt className={DT}>Size</dt>
                      <dd className="tabular-nums">{bytes(size)}</dd>
                    </>
                  ) : null}
                  {report.collections === null ? (
                    <>
                      <dt className={DT}>Collections</dt>
                      <dd>Not known before it installs: it changes existing ones</dd>
                    </>
                  ) : report.collections?.length ? (
                    <>
                      <dt className={DT}>Collections</dt>
                      <dd className="wrap-break-word">
                        {report.collections.join(", ")}
                      </dd>
                    </>
                  ) : null}
                </dl>
              </CollapsibleContent>
            </Collapsible>

            {refusal && (
              <p role="alert" className="text-sm text-warning">
                {why === "changed"
                  ? "The pack changed since you reviewed it. Nothing was installed."
                  : why === "authority"
                    ? "It needs the access above to install. Nothing was installed."
                    : why === "edited"
                      ? "Choose what happens to your edits, then install."
                      : `Couldn't install: ${refusal}`}
              </p>
            )}
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" disabled={installing} onClick={onCancel}>
            Cancel
          </Button>
          {why === "changed" ? (
            <Button onClick={() => setAttempt((n) => n + 1)}>Review again</Button>
          ) : (
            <Button
              variant="brand"
              disabled={
                !ready ||
                installing ||
                (needsAllow && !allow) ||
                others.length > 0 ||
                (yours.length > 0 && !replace)
              }
              onClick={() => void install()}
            >
              {installing ? "Installing…" : "Install"}
            </Button>
          )}
        </DialogFooter>
      </Content>
    </Root>
  );
}

const InlineRoot = ({ open, children }: { open?: boolean; children?: ReactNode }) =>
  open ? <>{children}</> : null;

const InlineContent = ({ children }: { className?: string; children?: ReactNode }) => (
  <div className="grid gap-6" data-testid="pack-install-review">
    {children}
  </div>
);

/** One model a pack needs: its name and what it's for on one line, and the
    profile it runs on. */
function SlotRow({
  name,
  description,
  value,
  optional,
  disabled,
  onChange,
  items,
}: {
  name: string;
  description: string | null | undefined;
  value: string;
  optional: boolean;
  disabled: boolean;
  onChange: (profile: string) => void;
  items: { value: string; label: string }[];
}) {
  const id = `pack-slot-${name}`;
  const all = optional ? [{ value: "", label: "Not set" }, ...items] : items;
  return (
    <li className="flex items-center gap-4 py-2">
      <span className="grid min-w-0 flex-1">
        <label htmlFor={id} className="text-sm first-letter:uppercase">
          {name}
        </label>
        {description && (
          <span className="truncate text-xs text-muted-foreground" title={description}>
            {description}
          </span>
        )}
      </span>
      <Select
        items={all}
        value={value === "" && !optional ? null : value}
        onValueChange={(v) => onChange(v ?? "")}
        disabled={disabled}
      >
        <SelectTrigger id={id} size="sm" className="w-48 shrink-0">
          <SelectValue placeholder="Choose a model" />
        </SelectTrigger>
        <SelectContent>
          {all.map((item) => (
            <SelectItem key={item.value || "∅"} value={item.value}>
              {item.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </li>
  );
}
