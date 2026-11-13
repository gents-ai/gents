import { ArrowLeft, ArrowRight, Circle, CircleCheck, Moon, Sun } from "lucide-react";
import type { Server } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Spinner } from "@gents/ui/components/spinner";
import { cn } from "@gents/ui/lib/utils";
import { ScrollArea } from "@gents/ui/components/scroll-area";
import { Tooltip, TooltipContent, TooltipTrigger } from "@gents/ui/components/tooltip";
import { preferences, useTheme } from "@/preferences";
import { InfoTip } from "../InfoTip";

export function Frame({
  children,
  embedded = false,
  paired = false,
  aside = false,
  onBack,
}: {
  children: React.ReactNode;
  embedded?: boolean;
  /** the column may gain an equal one to its right, so it is no wider
      than half the window less the gap between the two */
  paired?: boolean;
  /** that second column is there, so this one moves over by half its own
      width and the gap to centre the pair */
  aside?: boolean;
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
      className="viewport-frame bg-background text-foreground"
      data-testid="setup-screen"
    >
      <div className="px-8">
        <div
          className={cn(
            "mx-auto w-full max-w-xl min-w-0 pt-[10vh] pb-8 transition-transform duration-300 ease-out motion-reduce:transition-none",
            paired && "min-[900px]:max-w-[min(36rem,calc((100vw-5rem)/2))]",
            aside && "min-[900px]:-translate-x-[calc(50%+0.5rem)]",
          )}
        >
          {children}
          <div className="mt-6 flex justify-end">
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label="Toggle theme"
              onClick={flip}
            >
              {theme === "dark" ? <Sun /> : <Moon />}
            </Button>
          </div>
        </div>
      </div>
    </ScrollArea>
  );
}

export function Title({ children, note }: { children: React.ReactNode; note: string }) {
  return (
    <div className="mb-6">
      <h1 className="font-heading text-xl font-medium text-heading">{children}</h1>
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
        "min-w-0 rounded-2xl border bg-raised transition-[border-color,box-shadow] duration-200 motion-reduce:transition-none",
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
        <div className="grid min-w-0 grid-cols-1 gap-3 border-t border-border/60 px-4 py-3">
          {children}
        </div>
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
  info,
  htmlFor,
  children,
}: {
  label: string;
  info?: React.ReactNode;
  htmlFor?: string;
  children: React.ReactNode;
}) {
  if (info && htmlFor) {
    return (
      <div className="grid min-w-0 grid-cols-1 gap-1">
        <span className="flex items-center gap-1.5">
          <label htmlFor={htmlFor} className="text-xs text-muted-foreground">
            {label}
          </label>
          <InfoTip about={label}>{info}</InfoTip>
        </span>
        {children}
      </div>
    );
  }
  return (
    <label className="grid min-w-0 grid-cols-1 gap-1">
      <span className="text-xs text-muted-foreground">{label}</span>
      {children}
    </label>
  );
}

/** a small action shown as its icon, named by its tooltip */
export function IconAction({
  label,
  icon: Icon,
  onClick,
  disabled,
}: {
  label: string;
  icon: typeof Server;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label={label}
            disabled={disabled}
            onClick={onClick}
            className="text-muted-foreground hover:text-foreground"
          >
            <Icon />
          </Button>
        }
      />
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}
