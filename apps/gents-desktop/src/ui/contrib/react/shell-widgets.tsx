/* The shell's own drawings of a plugin's shell-rendered contributions: a
   pane-bar button and a transcript chip. The plugin supplied data; the
   click crosses to its frame. Nothing of the plugin runs here. */
import { Button } from "@gents/ui/components/button";
import { createElement } from "react";

import { pluginIcon } from "../icons";

export function HeaderActionButton({
  label,
  title,
  icon,
  onClick,
}: {
  label: string;
  title?: string;
  icon: string | null;
  onClick: () => void;
}) {
  const Icon = icon ? pluginIcon(icon) : null;
  return (
    <Button
      size="sm"
      variant="ghost"
      title={title ?? label}
      aria-label={label}
      onClick={onClick}
      data-testid="ux-header-action"
    >
      {Icon ? createElement(Icon, { className: "size-4" }) : null}
      {Icon ? <span className="sr-only">{label}</span> : label}
    </Button>
  );
}

/* `{attr}` in the label reads the directive's attribute; an attribute the
   model did not give reads as empty, never as the placeholder */
export function chipLabel(
  template: string,
  attrs: Readonly<Record<string, string>>,
): string {
  return template.replace(
    /\{([a-zA-Z0-9_-]+)\}/g,
    (_, name: string) => attrs[name] ?? "",
  );
}

export function DirectiveChip({
  label,
  icon,
  attrs,
  onClick,
}: {
  label: string;
  icon: string | null;
  attrs: Readonly<Record<string, string>>;
  onClick: () => void;
}) {
  const Icon = icon ? pluginIcon(icon) : null;
  return (
    <Button
      size="sm"
      variant="outline"
      onClick={onClick}
      data-testid="ux-directive-chip"
    >
      {Icon ? createElement(Icon, { className: "size-4" }) : null}
      {chipLabel(label, attrs)}
    </Button>
  );
}
