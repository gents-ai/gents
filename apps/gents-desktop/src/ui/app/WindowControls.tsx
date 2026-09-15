import { useEffect, useState, type ReactNode } from "react";
import { cn } from "@gents/ui/lib/utils";
import {
  closeWindow,
  minimizeWindow,
  onMaximizedChange,
  toggleMaximizeWindow,
} from "../../lib/windowControls";

export function WindowControls() {
  const [maximized, setMaximized] = useState(false);
  useEffect(() => onMaximizedChange(setMaximized), []);
  return (
    <div className="flex self-stretch" data-testid="window-controls">
      <CaptionButton label="Minimize" onClick={minimizeWindow}>
        <path d="M0 5.5h10" />
      </CaptionButton>
      <CaptionButton
        label={maximized ? "Restore" : "Maximize"}
        onClick={toggleMaximizeWindow}
      >
        {maximized ? (
          <path d="M2.5 2.5V.5h7v7h-2M.5 2.5h7v7h-7z" />
        ) : (
          <path d="M.5.5h9v9h-9z" />
        )}
      </CaptionButton>
      <CaptionButton label="Close" close onClick={closeWindow}>
        <path d="m.5.5 9 9m0-9-9 9" />
      </CaptionButton>
    </div>
  );
}

function CaptionButton({
  label,
  close = false,
  onClick,
  children,
}: {
  label: string;
  close?: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      onClick={onClick}
      className={cn(
        "grid w-[46px] cursor-default place-items-center text-foreground transition-colors",
        /* #c42b1c is Windows' own close red, not a brand colour */
        close ? "hover:bg-[#c42b1c] hover:text-[#fff]" : "hover:bg-accent",
      )}
    >
      <svg
        viewBox="0 0 10 10"
        className="size-2.5"
        fill="none"
        stroke="currentColor"
        aria-hidden="true"
      >
        {children}
      </svg>
    </button>
  );
}
