import type { ReactNode } from "react";
import { Check, Copy, MoreHorizontal } from "lucide-react";
import { cn } from "@gents/ui/lib/utils";
import { Button } from "@gents/ui/components/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuTrigger,
} from "@gents/ui/components/dropdown-menu";
import {
  FieldContent,
  FieldDescription,
  FieldLegend,
  FieldRow,
  FieldRows,
  FieldSet,
  FieldTitle,
} from "@gents/ui/components/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import {
  DEFAULT_LEASE_SECONDS,
  MAX_LEASE_SECONDS,
  type InviteKind,
  type ShareScope,
  type SharingParty,
} from "../../../hooks/sharing";
import { useCopied } from "@/lib/clipboard";

export const DAY_SECONDS = 24 * 60 * 60;

/** how long an approval or a renewal lasts, up to the longest the runtime
    accepts */
export const LEASES = [
  7 * DAY_SECONDS,
  DEFAULT_LEASE_SECONDS,
  60 * DAY_SECONDS,
  MAX_LEASE_SECONDS,
].map((seconds) => ({
  value: String(seconds),
  label: `${seconds / DAY_SECONDS} days`,
}));

export const shortDid = (did: string) =>
  did.length > 24 ? `${did.slice(0, 14)}…${did.slice(-6)}` : did;

/** a party as lists name it: by the name its node goes by here, else its DID */
export const partyName = (party: SharingParty) => party.name ?? shortDid(party.did);

/** what a share covers, in a few words */
export function scopeText(scope: ShareScope): string {
  if (scope.kind === "node") return "Whole node";
  if (scope.kind === "pack") return `${scope.name} pack`;
  return `${scope.name} agent`;
}

/** what a share covers, inside a sentence */
export const scopeInSentence = (scope: ShareScope) =>
  scope.kind === "node" ? "the whole node" : `the ${scopeText(scope)}`;

export const kindText = (kind: InviteKind) =>
  kind === "device" ? "Your device" : "A person";

export const errorMessage = (error: unknown) =>
  error instanceof Error ? error.message : String(error);

/** A lease to choose, in days. */
export function LeaseSelect({
  value,
  onChange,
  label,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <Select
      items={LEASES}
      value={value}
      onValueChange={(next) => next && onChange(next)}
    >
      <SelectTrigger size="sm" className="w-28" aria-label={label} disabled={disabled}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {LEASES.map((option) => (
          <SelectItem key={option.value} value={option.value}>
            {option.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

export function CopyButton({
  text,
  label,
  title = label,
}: {
  text: string;
  label: string;
  title?: string;
}) {
  const { copied, copy } = useCopied();
  return (
    <Button
      variant="quiet"
      size="icon-xs"
      aria-label={label}
      title={title}
      onClick={() => copy(text)}
    >
      {copied ? <Check /> : <Copy />}
    </Button>
  );
}

/** A party's name and DID, with the DID to copy. */
export function Party({ party }: { party: SharingParty }) {
  return (
    <span className="flex min-w-0 items-center gap-1">
      <span className="truncate text-sm font-medium" title={party.did}>
        {partyName(party)}
      </span>
      {party.name !== null && (
        <span
          className="truncate font-mono text-xs text-muted-foreground"
          title={party.did}
        >
          {shortDid(party.did)}
        </span>
      )}
      <CopyButton text={party.did} label={`Copy ${party.did}`} title="Copy DID" />
    </span>
  );
}

/** A group of the Sharing section, laid out like the configuration's other
    groups: an eyebrow title with an optional action over a card of rows.
    `note` replaces the rows when there are none to list, or when the
    runtime doesn't offer them. */
export function ListGroup({
  title,
  label = title,
  action,
  note,
  children,
}: {
  title: string;
  label?: string;
  action?: ReactNode;
  note?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <FieldSet className="mb-8" aria-label={title}>
      <div className="flex items-baseline justify-between gap-3">
        <FieldLegend variant="eyebrow">{title}</FieldLegend>
        {action}
      </div>
      {note ? (
        <FieldRows>
          <FieldRow>
            <FieldDescription>{note}</FieldDescription>
          </FieldRow>
        </FieldRows>
      ) : (
        <FieldRows role="list" aria-label={label}>
          {children}
        </FieldRows>
      )}
    </FieldSet>
  );
}

/** One row of a sharing list: what it is and a line about it, its actions
    at the right, and anything it opens below. The line's parts read apart
    with a dot between them. Actions drop below on a narrow page unless
    they are `inline`, as a ⋯ menu is. */
export function ItemRow({
  label,
  title,
  description,
  actions,
  below,
  error,
  inline = false,
}: {
  /** the row's accessible name */
  label: string;
  title: ReactNode;
  description?: ReactNode;
  actions?: ReactNode;
  below?: ReactNode;
  error?: string | null;
  inline?: boolean;
}) {
  return (
    <div role="listitem" aria-label={label}>
      <FieldRow
        className={cn(
          "py-3",
          !inline && "max-md:flex-col max-md:items-stretch max-md:gap-3",
        )}
      >
        <FieldContent className="min-w-0 gap-0.5">
          <FieldTitle className="w-full min-w-0">{title}</FieldTitle>
          {description && (
            <FieldDescription className="flex flex-wrap gap-x-1.5 text-xs [&>*+*]:before:mr-1.5 [&>*+*]:before:content-['·']">
              {description}
            </FieldDescription>
          )}
        </FieldContent>
        {actions && (
          <div className="flex shrink-0 flex-wrap items-center gap-2">{actions}</div>
        )}
      </FieldRow>
      {(below || error) && (
        <div className="grid gap-3 px-5 pb-4">
          {below}
          {error && (
            <p role="alert" className="text-sm break-words text-destructive">
              {error}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

/** A row's own actions, behind ⋯. */
export function MoreMenu({ label, children }: { label: string; children: ReactNode }) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        render={<Button variant="quiet" size="icon-sm" aria-label={label} />}
      >
        <MoreHorizontal />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-auto min-w-40">
        {children}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
