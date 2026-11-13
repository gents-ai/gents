import { History, LayoutGrid, Package, Users, Zap } from "lucide-react";

export type TopicToTry = {
  id: string;
  icon: typeof Zap;
  title: string;
  /** what it is, in a line that also reads well cut short */
  blurb: string;
  /** messages to start from; each is put in the composer, never sent */
  prompts: string[];
  /** the starter pack it cannot work without */
  pack?: { id: string; name: string };
};

/** The things to try first with the Engineer on a new node: what a node of
    one's own makes possible, not what any chat does. Each is what a fresh
    Engineer does with no further setup, bar the one that names the starter
    pack it needs. */
export const TOPICS_TO_TRY: TopicToTry[] = [
  {
    id: "build",
    icon: LayoutGrid,
    title: "Build an app that lives on your node",
    blurb:
      "The Engineer builds it live: collections with a schema, tools that read and write them, and a page in this app. No repo and no deploy; it all runs from your node's database.",
    prompts: [
      "Build an issue tracker: an issues collection with status and priority, tools to file and close one, and a board page",
      "Build a release log: a collection of releases per service, a tool to record one, and a page of the latest for each",
    ],
  },
  {
    id: "react",
    icon: Zap,
    title: "Have agents act on their own",
    blurb:
      "An agent can run on a schedule, or whenever something new lands in a collection. What it finds, and anything it needs from you, waits in your Mailbox.",
    prompts: [
      "Whenever an issue is filed in my tracker, have an agent triage it: set a priority and say why",
      "Every weekday at 9, tell me what my agents did yesterday and what needs my answer",
    ],
  },
  {
    id: "team",
    icon: Users,
    title: "Put a team of agents on a job",
    blurb:
      "Give each kind of work its own agent, with its own instructions, tools and model. Agents start one another's sessions and hand work on.",
    prompts: [
      "Create a researcher and an editor, and have the researcher hand each draft to the editor",
      "Make a code reviewer agent with read-only access to my repo, and let the Engineer call on it",
    ],
  },
  {
    id: "history",
    icon: History,
    title: "Ask what your agents did",
    blurb:
      "Every session, tool call and result is a document on your node, so what happened is something you can ask about.",
    prompts: [
      "Which tool calls failed this week, and in which sessions?",
      "Summarise what each of my agents worked on in the last three days",
    ],
  },
  {
    id: "packs",
    icon: Package,
    title: "Add to it with packs",
    blurb:
      "A pack adds tools, agents, skills and pages to a node. The Engineer can find and install one, showing you what it asks for first, or package what it has built as a pack of your own.",
    prompts: [
      "What packs are installed on this node, and what does each add?",
      "Package my issue tracker as a pack I could install on another node",
    ],
    pack: { id: "gents/pack_builder", name: "pack_builder" },
  },
];
