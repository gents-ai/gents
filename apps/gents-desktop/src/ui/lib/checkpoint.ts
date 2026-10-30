/* A compaction summary as its sections. The runtime writes the summary from
   a structured checkpoint in one fixed shape (`format_summary` in
   gents-loop): a `#` title, a `##` heading per non-empty field, `###`
   headings under Progress, and `- ` or `1. ` items. A summary in any other
   shape has no sections and is shown as written. */

export type CheckpointSection = {
  title: string;
  /* prose under the heading, as for the goal */
  text: string | null;
  items: string[];
  ordered: boolean;
};

const HEADING = /^(#{1,3})\s+(.+?)\s*$/;
const ITEM = /^\s*(?:[-*]|(\d+)\.)\s+(.*)$/;

export function checkpointSections(summary: string): CheckpointSection[] | null {
  const sections: CheckpointSection[] = [];
  let current: CheckpointSection | null = null;
  let prose: string[] = [];
  const close = () => {
    if (current) current.text = prose.join(" ") || null;
    prose = [];
  };
  for (const line of summary.split("\n")) {
    const heading = HEADING.exec(line);
    if (heading) {
      close();
      current =
        heading[1].length === 1
          ? null
          : { title: heading[2], text: null, items: [], ordered: false };
      if (current) sections.push(current);
      continue;
    }
    const item = ITEM.exec(line);
    if (current && item) {
      current.items.push(item[2].trim());
      current.ordered = item[1] !== undefined;
    } else if (current && line.trim()) {
      prose.push(line.trim());
    }
  }
  close();
  /* Progress holds only its Done, In progress and Blocked headings */
  const filled = sections.filter((section) => section.text || section.items.length);
  return filled.length ? filled : null;
}

/* the runtime's headings, said the way the panel labels them */
const LABELS: Record<string, string> = {
  Goal: "Goal",
  "Constraints and preferences": "Constraints",
  Done: "Done",
  "In progress": "In progress",
  Blocked: "Blocked",
  "Current work": "Now",
  "Key decisions": "Decisions",
  "Errors and fixes": "Fixes",
  Verification: "Checked",
  Uncertainties: "Unsure",
  "Next actions": "Next",
  "Critical context": "Keep",
  "Files read": "Read",
  "Files modified": "Changed",
};

export const sectionLabel = (title: string) => LABELS[title] ?? title;

/* what shows before the rest is opened: where the agent is headed, and what
   it may have wrong or be stuck on */
const LEADING = new Set(["Goal", "Next actions", "Next", "Uncertainties", "Blocked"]);

export const isLeadingSection = (section: CheckpointSection) =>
  LEADING.has(section.title);
