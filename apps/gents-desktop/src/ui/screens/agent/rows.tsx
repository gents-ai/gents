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
