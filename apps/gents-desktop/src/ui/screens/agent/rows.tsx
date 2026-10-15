/* the settings-group pattern from the kit, with the pieces this screen repeats */
import type { ReactNode } from "react";
import { useHomeFolder } from "@/hooks/useHomeFolder";
import { shortPath } from "@/lib/paths";
import {
  FieldContent,
  FieldDescription,
  FieldError,
  FieldLabel,
  FieldLegend,
  FieldRow,
  FieldRows,
  FieldSet,
} from "@gents/ui/components/field";

export function Group({
  title,
  children,
  action,
}: {
  /* optional: a panel with one self-explanatory field needs no heading */
  title?: string;
  children: ReactNode;
  action?: ReactNode;
}) {
  return (
    <FieldSet className="mb-8">
      {(title || action) && (
        <div className="flex items-baseline justify-between">
          {title && <FieldLegend variant="eyebrow">{title}</FieldLegend>}
          {action}
        </div>
      )}
      <FieldRows>{children}</FieldRows>
    </FieldSet>
  );
}

export function Row({
  label,
  description,
  htmlFor,
  error,
  children,
}: {
  label: ReactNode;
  description?: ReactNode;
  htmlFor?: string;
  error?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <FieldRow className="max-md:flex-col max-md:items-stretch max-md:gap-3">
      <FieldContent>
        <FieldLabel htmlFor={htmlFor}>{label}</FieldLabel>
        {description && <FieldDescription>{description}</FieldDescription>}
        {error && <FieldError>{error}</FieldError>}
      </FieldContent>
      {children}
    </FieldRow>
  );
}

export function StackedRow({
  label,
  description,
  htmlFor,
  error,
  children,
}: {
  label: ReactNode;
  description?: ReactNode;
  htmlFor?: string;
  error?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <FieldRow className="flex-col items-stretch gap-3">
      <FieldContent>
        <FieldLabel htmlFor={htmlFor}>{label}</FieldLabel>
        {description && <FieldDescription>{description}</FieldDescription>}
        {error && <FieldError>{error}</FieldError>}
      </FieldContent>
      {children}
    </FieldRow>
  );
}

/* a read-only value, in mono when it is an identifier. Beside a row's
   description it keeps its own width, as far as half the row: a short value
   is never cut short for a long description, which wraps instead, and only
   a long one truncates. Stacked below md it takes the row's width. */
export function Fact({
  children,
  mono = false,
  title,
}: {
  children: ReactNode;
  mono?: boolean;
  title?: string;
}) {
  return (
    <span
      title={title}
      className={`max-w-[28rem] truncate text-sm text-muted-foreground max-md:max-w-full max-md:break-all max-md:whitespace-normal md:[[data-slot=field-row]>&]:max-w-[min(28rem,50%)] md:[[data-slot=field-row]>&]:shrink-0 ${mono ? "font-mono text-xs" : ""}`}
    >
      {children ?? "—"}
    </span>
  );
}

/* A path as it reads: from ~ when it is on this computer, shortened from
   the left so its own name stays, and whole in its title. */
export function PathFact({ path, local }: { path: string | null; local: boolean }) {
  const home = useHomeFolder();
  if (!path) return <Fact>{null}</Fact>;
  return (
    <Fact mono title={path}>
      {shortPath(path, { home: local ? home : null, max: 56 })}
    </Fact>
  );
}
