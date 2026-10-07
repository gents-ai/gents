/* The setup flows' frame and controls: the page (or the panel when
   embedded), its title, a choice row, the step navigation and a field. */
import { ArrowLeft, ArrowRight, Circle, CircleCheck, Moon, Sun } from "lucide-react";
import type { Server } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { preferences, useTheme } from "@/preferences";

export function Frame({
  children,
  embedded = false,
  onBack,
}: {
  children: React.ReactNode;
  embedded?: boolean;
  onBack?: () => void;
}) {
  const theme = useTheme();
  const flip = () => preferences.setTheme(theme === "dark" ? "light" : "dark");
  if (embedded)
    return (
      <div className="w-full min-w-0" data-testid="inference-setup-panel">
        {onBack && (
          <button
            type="button"
            onClick={onBack}
            className="mb-6 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-3.5" /> Back
          </button>
        )}
        {children}
      </div>
    );
  return (
    <ScrollArea
      className="viewport-frame relative bg-background text-foreground"
      data-testid="setup-screen"
    >
      <div className="px-8">
        {/* anchored a fixed way down, not centered: a step can grow or shrink without moving its title */}
        <div className="mx-auto w-full max-w-xl pt-[10vh] pb-8">{children}</div>
        <Button
          variant="ghost"
          size="icon-sm"
          className="absolute right-6 bottom-6"
          aria-label="Toggle theme"
          onClick={flip}
        >
          {theme === "dark" ? <Sun /> : <Moon />}
        </Button>
      </div>
    </ScrollArea>
  );
}

export function Title({ children, note }: { children: React.ReactNode; note: string }) {
  return (
    <div className="mb-6">
      <h1 className="font-heading text-2xl font-medium text-heading">{children}</h1>
      <p className="mt-1.5 text-sm text-muted-foreground">{note}</p>
    </div>
  );
}

export function Option({
  selected,
  onSelect,
  title,
  hint,
  icon: Icon,
  logo,
  testId,
  children,
  disabled,
}: {
  selected: boolean;
  onSelect: () => void;
  title: string;
  hint?: string;
  icon: typeof Server;
  logo?: string;
  testId?: string;
  children?: React.ReactNode;
  disabled?: boolean;
}) {
  return (
    <div
      className={cn(
        "rounded-2xl border bg-raised",
        selected ? "border-brand ring-1 ring-brand" : "border-border/60",
      )}
    >
      <button
        type="button"
        role="radio"
        disabled={disabled}
        aria-checked={selected}
        data-testid={testId}
        onClick={onSelect}
        className="flex w-full items-center gap-3 rounded-2xl px-4 py-3.5 text-left hover:bg-accent"
      >
        {selected ? (
          <CircleCheck className="size-4 shrink-0 text-foreground" />
        ) : (
          <Circle className="size-4 shrink-0 text-muted-foreground" />
        )}
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-medium">{title}</span>
          {hint && (
            <span className="block truncate text-xs text-muted-foreground">{hint}</span>
          )}
        </span>
        {logo ? (
          <img
            src={logo}
            alt=""
            className="size-5 shrink-0 object-contain dark:invert"
          />
        ) : (
          <Icon className="size-5 shrink-0 text-heading" />
        )}
      </button>
      {selected && children ? (
        <div className="grid gap-3 border-t border-border/60 px-4 py-3">{children}</div>
      ) : null}
    </div>
  );
}

export function Nav({
  onBack,
  next,
  nextLabel = "Next",
  busy,
  disabled,
}: {
  onBack?: () => void;
  next?: () => void;
  nextLabel?: string;
  busy?: boolean;
  disabled?: boolean;
}) {
  return (
    <div className="mt-8 flex items-center justify-between">
      {onBack ? (
        <button
          type="button"
          onClick={onBack}
          className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3.5" /> Back
        </button>
      ) : (
        <span />
      )}
      {next && (
        <Button
          variant="brand"
          onClick={next}
          disabled={disabled || busy}
          data-testid="setup-next"
        >
          {busy ? <Spinner /> : null} {nextLabel} <ArrowRight />
        </Button>
      )}
    </div>
  );
}

export function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <label className="grid gap-1">
      <span className="text-xs text-muted-foreground">{label}</span>
      {children}
    </label>
  );
}
