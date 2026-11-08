import type { ComponentType } from "react";
import {
  Activity,
  Archive,
  BookOpen,
  CalendarCheck,
  FlaskConical,
  Inbox,
  Package,
  Sunrise,
} from "lucide-react";
import type { PRESETS } from "../agent/NewAutomationDialog";

/** A starting point for a new automation: its name, when it runs, and what
    it asks the agent, filled into the New automation dialog to change
    before saving. */
export type AutomationTemplate = {
  id: string;
  name: string;
  /** one line on the card */
  blurb: string;
  icon: ComponentType<{ className?: string }>;
  preset: (typeof PRESETS)[number]["id"];
  prompt: string;
};

export const TEMPLATES: readonly AutomationTemplate[] = [
  {
    id: "morning-summary",
    name: "Morning summary",
    blurb: "What finished, what failed and what's waiting on you, each weekday at 9.",
    icon: Sunrise,
    preset: "weekdays",
    prompt:
      "Summarize what happened in my sessions since yesterday morning: what finished, what failed or stopped, and what is waiting on me. Keep it short, one line each, and name the session for each.",
  },
  {
    id: "mailbox-triage",
    name: "Mailbox triage",
    blurb: "What in the mailbox needs you, most urgent first, each weekday at 9.",
    icon: Inbox,
    preset: "weekdays",
    prompt:
      "Go through my mailbox and list what needs me, most urgent first, with a one-line suggestion for each. Leave out what I've already answered.",
  },
  {
    id: "running-work",
    name: "Check on running work",
    blurb: "Sessions still running that look stuck, and what to do next, every hour.",
    icon: Activity,
    preset: "hourly",
    prompt:
      "Check the sessions that are still running. If one looks stuck or keeps failing, say which, why you think so, and what you'd do next. If all is well, say so in one line.",
  },
  {
    id: "weekly-review",
    name: "Weekly review",
    blurb: "Last week's work, what's still open and what kept failing, each Monday.",
    icon: CalendarCheck,
    preset: "weekly",
    prompt:
      "Look back over last week's sessions and write a short review: what got done, what is still open, and anything that failed more than once, with what you'd do about it.",
  },
  {
    id: "research-digest",
    name: "Research digest",
    blurb: "New work on the topics you follow, with sources, each Monday.",
    icon: BookOpen,
    preset: "weekly",
    prompt:
      "Find new articles, papers and releases from the past week on the topics I follow: [list your topics]. Summarize the few that matter most, with a link to each source.",
  },
  {
    id: "test-run",
    name: "Test run",
    blurb: "Run the project's tests and say what broke, each weekday at 9.",
    icon: FlaskConical,
    preset: "weekdays",
    prompt:
      "In the project folder, run the test suite. If anything fails, say which tests, the likely cause, and the smallest fix. If everything passes, say so in one line.",
  },
  {
    id: "tidy-sessions",
    name: "Tidy up sessions",
    blurb: "Finished sessions to archive, and ones that need a follow-up, each Monday.",
    icon: Archive,
    preset: "weekly",
    prompt:
      "Look at my sessions from the past week. List the finished ones I can archive, and the ones that ended with something left to do, with the next step for each.",
  },
  {
    id: "pack-updates",
    name: "Pack updates",
    blurb: "Installed packs with a newer version and what changed, each Monday.",
    icon: Package,
    preset: "weekly",
    prompt:
      "Check the packs installed on this node for newer versions. For each one, say what changed and whether it's worth updating. Don't install anything.",
  },
];
