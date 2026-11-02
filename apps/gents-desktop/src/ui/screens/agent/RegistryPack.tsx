import { useEffect, useId, useState } from "react";
import { ChevronRight, Download } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import { Skeleton } from "@gents/ui/components/skeleton";
import { cn } from "@gents/ui/lib/utils";
import type { FoundPack, PackInfo } from "@source-inc/gents-desktop-client";
import { useApp } from "@/app/AppContext";
import { errorText } from "../../../lib/errorText";
import { isExternalUrl } from "../../../lib/externalLinks";

type Version = {
  version: string;
  published_at?: string | null;
  size_bytes?: number | null;
  yanked?: boolean;
};

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

const text = (value: unknown): string | null =>
  typeof value === "string" && value.trim() ? value : null;

const versionsOf = (info: PackInfo): Version[] =>
  Array.isArray(info.versions)
    ? info.versions.filter(
        (v): v is Version =>
          typeof v === "object" &&
          v !== null &&
          typeof (v as Version).version === "string",
      )
    : [];

/* plugins are named as strings or as objects with a name, by registry version */
const pluginNames = (value: unknown): string[] =>
  Array.isArray(value)
    ? value
        .map((p) =>
          typeof p === "string"
            ? p
            : typeof p === "object" &&
                p &&
                typeof (p as { name?: unknown }).name === "string"
              ? (p as { name: string }).name
              : null,
        )
        .filter((p): p is string => p !== null)
    : [];

const count = new Intl.NumberFormat(undefined, { notation: "compact" });

function bytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

const day = (at: string) =>
  new Date(at).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
    year: "numeric",
  });

function Keyword({ word, onPick }: { word: string; onPick: (word: string) => void }) {
  return (
    <button
      type="button"
      onClick={() => onPick(word)}
      title={`Search for ${word}`}
      className="cursor-pointer rounded-md bg-muted px-1.5 py-0.5 text-xs text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
    >
      {word}
    </button>
  );
}

/** A registry pack's line, which opens on its page: owner, the latest
    version's date and size, its plugins and how many versions came before. */
export function RegistryPack({
  pack,
  installed,
  installing,
  onInstall,
  onKeyword,
  open,
  onToggle,
}: {
  pack: FoundPack;
  installed: boolean;
  installing: boolean;
  onInstall: () => void;
  onKeyword: (word: string) => void;
  open: boolean;
  onToggle: () => void;
}) {
  const name = `${pack.namespace}/${pack.name}`;
  const description = text(pack.description);
  const keywords = strings(pack.keywords);
  const downloads = typeof pack.downloads === "number" ? pack.downloads : null;
  const detailId = useId();
  return (
    <li className="@container">
      <div className="flex h-11 items-center gap-3 px-5">
        <button
          type="button"
          aria-expanded={open}
          aria-controls={detailId}
          onClick={onToggle}
          className="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 text-left text-sm outline-none focus-visible:underline"
        >
          <ChevronRight
            aria-hidden
            className={cn(
              "size-3.5 shrink-0 text-muted-foreground transition-transform",
              open && "rotate-90",
            )}
          />
          <span className="min-w-0 truncate">
            <span className="font-medium text-foreground">{name}</span>
            {description && (
              <span className="ml-2 text-xs text-muted-foreground">{description}</span>
            )}
          </span>
        </button>
        {keywords.length > 0 && (
          <span className="hidden shrink-0 gap-1 @lg:flex">
            {keywords.slice(0, 2).map((word) => (
              <Keyword key={word} word={word} onPick={onKeyword} />
            ))}
          </span>
        )}
        {downloads !== null && (
          <span
            title={`${downloads.toLocaleString()} downloads`}
            className="flex shrink-0 items-center gap-1 text-xs text-muted-foreground tabular-nums"
          >
            <Download aria-hidden className="size-3" />
            <span className="sr-only">Downloads: </span>
            {count.format(downloads)}
          </span>
        )}
        {installed ? (
          <span className="shrink-0 text-xs text-muted-foreground">Installed</span>
        ) : (
          <Button
            size="sm"
            variant="outline"
            className="shrink-0"
            disabled={installing}
            onClick={onInstall}
          >
            Install
          </Button>
        )}
      </div>
      {open && (
        <div id={detailId} className="px-5 pb-3 pl-10">
          <PackDetail
            name={name}
            description={description}
            keywords={keywords}
            onKeyword={onKeyword}
          />
        </div>
      )}
    </li>
  );
}

function PackDetail({
  name,
  description,
  keywords,
  onKeyword,
}: {
  name: string;
  description: string | null;
  keywords: string[];
  onKeyword: (word: string) => void;
}) {
  const { actions } = useApp();
  const [info, setInfo] = useState<PackInfo | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    actions.packInfo(name).then(
      (read) => live && setInfo(read),
      (error: unknown) => live && setFailed(errorText(error)),
    );
    return () => {
      live = false;
    };
  }, [actions, name]);
  const versions = info ? versionsOf(info).filter((v) => !v.yanked) : [];
  const latest =
    versions.find((v) => v.version === info?.latest) ?? versions[versions.length - 1];
  const older = latest ? versions.filter((v) => v !== latest).length : 0;
  const owner = info ? text(info.owner) : null;
  const page = info ? text(info.homepage) : null;
  const homepage = page && isExternalUrl(page) ? page : null;
  const plugins = info ? pluginNames(info.plugins) : [];
  const facts = latest
    ? [
        owner && `by ${owner}`,
        [latest.version, latest.published_at && `published ${day(latest.published_at)}`]
          .filter(Boolean)
          .join(", "),
        typeof latest.size_bytes === "number" && bytes(latest.size_bytes),
        plugins.length > 0 &&
          `${plugins.length} ${plugins.length === 1 ? "plugin" : "plugins"}: ${plugins.join(", ")}`,
        older > 0 && `${older} older ${older === 1 ? "version" : "versions"}`,
      ].filter((f): f is string => Boolean(f))
    : [];
  return (
    <div className="grid gap-2 text-xs">
      {description && <p className="text-sm text-foreground/80">{description}</p>}
      {failed ? (
        <p className="text-destructive">Couldn't read this pack's page: {failed}</p>
      ) : info === null ? (
        <div aria-busy="true" className="grid gap-1.5">
          <Skeleton className="h-3 w-72 max-w-full bg-foreground/10" />
        </div>
      ) : (
        facts.length > 0 && (
          <p className="text-muted-foreground">
            {facts.join(" · ")}
            {homepage && (
              <>
                {" · "}
                <a
                  href={homepage}
                  target="_blank"
                  rel="noreferrer"
                  className="text-foreground underline-offset-2 hover:underline"
                >
                  Homepage
                </a>
              </>
            )}
          </p>
        )
      )}
      {keywords.length > 0 && (
        <div className="flex flex-wrap gap-1">
          {keywords.map((word) => (
            <Keyword key={word} word={word} onPick={onKeyword} />
          ))}
        </div>
      )}
    </div>
  );
}
