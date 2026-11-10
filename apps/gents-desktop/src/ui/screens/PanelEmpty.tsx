import type { ComponentType } from "react";

/** A dock panel with nothing to show yet: its icon, what is missing, and what
    will appear there, in the middle of the panel. */
export function PanelEmpty({
  icon: Icon,
  title,
  children,
}: {
  icon: ComponentType<{ className?: string }>;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex h-full min-h-0 overflow-y-auto p-6">
      <div className="m-auto grid max-w-72 justify-items-center gap-1.5 text-center">
        <Icon className="mb-1 size-5 text-muted-foreground" />
        <p className="text-sm font-medium text-foreground">{title}</p>
        <p className="text-xs text-pretty text-muted-foreground">{children}</p>
      </div>
    </div>
  );
}
