Please set up a small research desk for recurring briefing work. Configure it completely, but do not send a brief request, start the assistants, or run a demonstration. I want to review the setup before using it.

Create six assistants with their own instructions and appropriate tools:
- Planner breaks a topic into research questions.
- Researcher investigates an assigned question using local source files and cites evidence.
- Analyst gathers the findings and drafts a concise brief.
- Reviewer checks the draft against the findings.
- Publisher releases an approved brief.
- Coordinator handles revision requests and can delegate to Researcher and Reviewer. Make all six available for you to delegate to later.

Use our current model and connection, with high thinking effort, temperature 1 and top-p 0.95. Share suitable settings instead of making a copy for every assistant. Planner, Analyst and Coordinator may use 40 turns, 180,000 total tokens and 20 minutes per request; Researcher and Reviewer get 30 turns, 120,000 tokens and 15 minutes; Publisher gets 15 turns, 60,000 tokens and 10 minutes. Leave your own model settings and instructions unchanged.

Researcher may read local files; the others work from the records below. None may edit files, run commands or change configuration. Give each assistant only the record-writing and reading abilities needed for its role. Only Coordinator needs delegation tools.

Use these records; the field names are how my other systems exchange data:
```
BriefRequest { batch: string, topic: string, question_count: int }
ResearchAssignment { batch: string, assignment: string, question: string, expected_count: int }
ResearchFinding { batch: string, assignment: string, finding: string, sources: string, expected_count: int }
DraftBrief { batch: string, text: string }
ReviewDecision { batch: string, approved: bool, feedback: string }
PublishedBrief { batch: string, text: string }
```
Batch and assignment are business keys supplied by the caller, never generated request IDs. Preserve them through the workflow. Record tools should describe when to use them, require their listed inputs, and support looking up records by batch alone.

For record permissions:
- Planner: read BriefRequest; add ResearchAssignment.
- Researcher: read ResearchAssignment; add ResearchFinding.
- Analyst: read ResearchAssignment, ResearchFinding and DraftBrief; add DraftBrief.
- Reviewer: read DraftBrief and ResearchFinding; add ReviewDecision.
- Publisher: read DraftBrief, ReviewDecision and PublishedBrief; add PublishedBrief.
- Coordinator: read ResearchFinding, DraftBrief and ReviewDecision; add ResearchAssignment.

Wire the workflow so newly arriving records cause the next step:
- BriefRequest → Planner reads it and writes exactly question_count assignments, each carrying that count as expected_count.
- ResearchAssignment → Researcher reads the assignment, investigates it and writes one finding with the same batch, assignment and expected_count.
- ResearchFinding → Analyst looks up all findings for the batch. Wait until expected_count distinct assignments are present; then write one draft. Handle these arrivals one at a time so two arrivals cannot produce two drafts. Check for an existing draft before writing.
- DraftBrief → Reviewer reads the draft and supporting findings, then writes one decision with feedback.
- ReviewDecision → Publisher reads the decision and draft. Publish only if approved; otherwise leave publication to a later revision. Handle decisions one at a time and avoid duplicate publication.

Keep a completion record for each step. Other work can run independently. Coordinator is available for manual revision requests; do not launch it automatically. It may read findings, drafts and decisions, and write new research assignments.

Use clear instructions of your own, including how each assistant handles missing inputs. Do not give assistants broad database access. When finished, send me a mailbox notice titled Research desk setup receipt, naming the six assistants, the configured handoffs, and anything still missing. Do not claim the workflow has run.
