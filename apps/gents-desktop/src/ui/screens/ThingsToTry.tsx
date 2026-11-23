import { ArrowUpRight, ChevronRight } from "lucide-react";
import type { DefaultPackState } from "@source-inc/gents-desktop-client";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@gents/ui/components/collapsible";
import { useDefaultPacks } from "./packs/useDefaultPacks";
import { TOPICS_TO_TRY, type TopicToTry } from "./topicsToTry";

/* what a topic's starter pack lacks, by the state the node's list gives it */
const LACKING: Record<Exclude<DefaultPackState, "installed">, string> = {
  missing: "isn’t installed on this node yet",
  not_enabled: "is installed on this node but not turned on",
  unavailable: "can’t be installed by this build of Gents",
  unknown: "is installed on this node, but its state could not be read",
};

/** A few things to try with the Engineer, each opening to a line about it
    and messages to start from. Choosing a message hands it to `onPick`. */
export function ThingsToTry({ onPick }: { onPick: (message: string) => void }) {
  const { packs, settled } = useDefaultPacks();
  /* said only when the node's own list says so; an unread list says nothing */
  const packNote = ({ pack }: TopicToTry) => {
    if (!pack || !settled) return null;
    const state = packs.find((one) => one.pack === pack.id)?.state;
    if (state === undefined || state === "installed") return null;
    return (
      <p className="text-xs text-warning">
        Works best with the {pack.name} starter pack, which {LACKING[state]}.
      </p>
    );
  };
  return (
    <ul aria-label="Things to try" className="divide-y divide-border/60">
      {TOPICS_TO_TRY.map((topic) => (
        <li key={topic.id} className="py-1">
          <Collapsible className="group/topic">
            <CollapsibleTrigger className="flex w-full min-w-0 cursor-pointer items-center gap-3 py-1.5 text-left">
              <topic.icon className="size-4 shrink-0 text-muted-foreground" />
              <span className="min-w-0 flex-1 truncate text-sm font-medium">
                {topic.title}
              </span>
              <ChevronRight className="size-3.5 shrink-0 text-muted-foreground transition-transform duration-150 group-data-open/topic:rotate-90 motion-reduce:transition-none" />
            </CollapsibleTrigger>
            <CollapsibleContent className="h-(--collapsible-panel-height) overflow-hidden pl-7 transition-[height,opacity] duration-200 ease-out data-ending-style:h-0 data-ending-style:opacity-0 data-starting-style:h-0 data-starting-style:opacity-0 motion-reduce:transition-none">
              <div className="grid gap-2 pb-2">
                <p className="text-xs text-muted-foreground">{topic.blurb}</p>
                {packNote(topic)}
                <ul aria-label={`Messages for ${topic.title}`} className="grid gap-1">
                  {topic.prompts.map((prompt) => (
                    <li key={prompt}>
                      <button
                        type="button"
                        onClick={() => onPick(prompt)}
                        className="group/prompt flex w-full cursor-pointer items-center gap-2 rounded-lg bg-muted/40 px-3 py-2 text-left text-sm hover:bg-accent"
                      >
                        <span className="min-w-0 flex-1">{prompt.trim()}</span>
                        <ArrowUpRight className="size-3.5 shrink-0 text-muted-foreground group-hover/prompt:text-foreground" />
                      </button>
                    </li>
                  ))}
                </ul>
              </div>
            </CollapsibleContent>
          </Collapsible>
        </li>
      ))}
    </ul>
  );
}
