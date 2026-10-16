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

/* a read-only value, in mono when it is an identifier */
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
      className={`max-w-[28rem] truncate text-sm text-muted-foreground max-md:max-w-full max-md:break-all max-md:whitespace-normal ${mono ? "font-mono text-xs" : ""}`}
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

export function NameLinks({
  items,
  shown = 3,
}: {
  items: readonly { key: string; name: string; href: string }[];
  shown?: number;
}) {
  const named = items.length <= shown + 1 ? items.length : shown;
  return (
    <span className="flex flex-wrap justify-end gap-x-3 gap-y-1">
      {items.slice(0, named).map((item) => (
        <a
          key={item.key}
          href={item.href}
          title={item.name}
          className="max-w-48 truncate underline-offset-2 hover:text-foreground hover:underline"
        >
          {item.name}
        </a>
      ))}
      {items.length > named && <span>and {items.length - named} more</span>}
    </span>
  );
}
