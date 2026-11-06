import { useEffect, useRef, type ReactNode } from "react";
import { cn } from "@gents/ui/lib/utils";
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
  action,
  children,
}: {
  label: ReactNode;
  description?: ReactNode;
  htmlFor?: string;
  error?: ReactNode;
  /** a control at the label's right, for the field below */
  action?: ReactNode;
  children?: ReactNode;
}) {
  const content = (
    <FieldContent>
      <FieldLabel htmlFor={htmlFor}>{label}</FieldLabel>
      {description && <FieldDescription>{description}</FieldDescription>}
      {error && <FieldError>{error}</FieldError>}
    </FieldContent>
  );
  return (
    /* the kit's horizontal field aligns to the start once it has a label
       block, which would shrink what follows to its content */
    <FieldRow className="flex-col items-stretch gap-3 has-[>[data-slot=field-content]]:items-stretch">
      {action ? (
        <div className="flex items-start gap-3 *:data-[slot=field-content]:flex-1">
          {content}
          <div className="shrink-0">{action}</div>
        </div>
      ) : (
        content
      )}
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
      title={title ?? (typeof children === "string" ? children : undefined)}
      className={`max-w-[28rem] truncate text-sm text-muted-foreground max-md:max-w-full max-md:break-all max-md:whitespace-normal md:[[data-slot=field-row]>&]:max-w-[min(28rem,50%)] md:[[data-slot=field-row]>&]:shrink-0 ${mono ? "font-mono text-xs" : ""}`}
    >
      {children ?? "—"}
    </span>
  );
}

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

/** Fields that open on demand, such as a header's Edit: they grow from no
    height and fade in, take focus at their first field, and come into view
    if opening left them off screen. Closed, they stay in the page, inert, so
    closing animates too. */
/** Opens to `fieldId` once the fields are open: focus there, at the end of
    what it holds, brought into view. */
export function focusField(fieldId: string) {
  requestAnimationFrame(() => {
    const field = document.getElementById(fieldId);
    if (!(field instanceof HTMLElement)) return;
    field.focus({ preventScroll: true });
    field.scrollIntoView({ block: "nearest" });
  });
}

export function Unfold({
  open,
  id,
  children,
}: {
  open: boolean;
  id: string;
  children: ReactNode;
}) {
  const box = useRef<HTMLDivElement>(null);
  const was = useRef(open);
  useEffect(() => {
    const opening = open && !was.current;
    was.current = open;
    const el = box.current;
    if (!opening || !el) return;
    /* a field already chosen, as a header's tag chooses Tags, keeps focus */
    if (!el.contains(document.activeElement))
      el.querySelector<HTMLElement>("input, textarea, select, button")?.focus({
        preventScroll: true,
      });
    const still = matchMedia("(prefers-reduced-motion: reduce)").matches;
    /* once grown, so the whole group can come into view */
    const timer = setTimeout(
      () =>
        el.scrollIntoView({ block: "nearest", behavior: still ? "auto" : "smooth" }),
      still ? 0 : 210,
    );
    return () => clearTimeout(timer);
  }, [open]);
  return (
    <div
      id={id}
      inert={!open || undefined}
      className={cn(
        "grid transition-[grid-template-rows,opacity] duration-200 ease-out motion-reduce:transition-none",
        open ? "grid-rows-[1fr] opacity-100" : "grid-rows-[0fr] opacity-0",
      )}
    >
      <div ref={box} className="min-h-0 overflow-hidden">
        {children}
      </div>
    </div>
  );
}

/** Configuration shown read-only: values at full strength, and nothing
    offered that would change them. Editors mark what only changes things
    with `data-readonly-hide`. */
export function ReadOnlyConfig({
  readOnly,
  children,
}: {
  readOnly: boolean;
  children: ReactNode;
}) {
  return (
    <fieldset
      disabled={readOnly}
      data-readonly={readOnly ? "" : undefined}
      className="m-0 min-w-0 border-0 p-0 data-readonly:[&_*:disabled]:opacity-100! data-readonly:[&_[data-disabled]]:opacity-100! data-readonly:[&_[data-readonly-hide]]:hidden"
    >
      {children}
    </fieldset>
  );
}
