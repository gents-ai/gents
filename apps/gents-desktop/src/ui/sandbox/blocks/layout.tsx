/* A plugin page draws edge to edge in a frame whose body does not scroll
   (App.css), and no plugin's classes reach Tailwind, so the page's own
   scroll and its columns are blocks rather than CSS the plugin writes. */
import type { ReactNode } from "react";

export interface PageProps {
  title: ReactNode;
  description?: ReactNode;
  /** buttons at the right of the title */
  actions?: ReactNode;
  children: ReactNode;
}

export function Page({ title, description, actions, children }: PageProps) {
  return (
    <div data-slot="page" className="h-full overflow-y-auto thin-scrollbar">
      <div className="mx-auto flex w-full max-w-6xl flex-col gap-4 p-5 md:p-6">
        <header className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <h1 className="font-heading text-lg font-medium text-heading">{title}</h1>
            {description && (
              <p className="text-sm text-muted-foreground">{description}</p>
            )}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </header>
        {children}
      </div>
    </div>
  );
}

export interface GridProps {
  children: ReactNode;
  /** the narrowest a column may get, in px; default 220 */
  minWidth?: number;
}

export function Grid({ children, minWidth = 220 }: GridProps) {
  return (
    <div
      data-slot="grid"
      className="grid gap-3"
      style={{
        gridTemplateColumns: `repeat(auto-fit, minmax(min(100%, ${minWidth}px), 1fr))`,
      }}
    >
      {children}
    </div>
  );
}
