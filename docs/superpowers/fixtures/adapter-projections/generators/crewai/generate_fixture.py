#!/usr/bin/env python3
from __future__ import annotations

import argparse
import importlib.metadata
import json
import os
from pathlib import Path
from typing import Any, Optional, Union

from crewai import Agent, BaseLLM, Crew, Process, Task


CONTEXT_ID = "context-crewai-docker-1"
REQUEST_ID = "req-crewai-docker-1"
RESEARCH_REQUEST_ID = "req-crewai-research-docker-1"
REVIEW_REQUEST_ID = "req-crewai-review-docker-1"
TASK_TEXT = "Map a CrewAI multi-agent task to Defra Agent projection fields."


AGENTS = [
    {
        "name": "planner",
        "role": "planner",
        "title": "CrewAI Planner",
        "agent_did": "did:defra-agent:crewai-planner",
        "behavior_id": "crewai.planner",
        "goal": "Plan adapter interoperability work.",
        "backstory": "Plans multi-agent work and passes scoped tasks to researchers.",
        "response": (
            "PLAN: define adapter mapping milestones; "
            f"child_request={RESEARCH_REQUEST_ID}"
        ),
    },
    {
        "name": "researcher",
        "role": "researcher",
        "title": "CrewAI Researcher",
        "agent_did": "did:defra-agent:crewai-researcher",
        "behavior_id": "crewai.researcher",
        "goal": "Research CrewAI task and context behavior.",
        "backstory": "Finds framework-specific evidence for projection compatibility.",
        "response": (
            "RESEARCH: CrewAI sequential tasks carry agent assignment and context; "
            f"child_request={REVIEW_REQUEST_ID}"
        ),
    },
    {
        "name": "reviewer",
        "role": "reviewer",
        "title": "CrewAI Reviewer",
        "agent_did": "did:defra-agent:crewai-reviewer",
        "behavior_id": "crewai.reviewer",
        "goal": "Review and approve adapter output.",
        "backstory": "Approves the final mapped multi-agent task projection.",
        "response": "APPROVE: CrewAI projection output is ready",
    },
]


class ScriptedLLM(BaseLLM):
    def __init__(self, agent_name: str, responses: list[str]) -> None:
        super().__init__(model=f"defra-scripted-{agent_name}", temperature=0)
        self.agent_name = agent_name
        self.responses = responses
        self.calls: list[dict[str, Any]] = []

    def call(
        self,
        messages: Union[str, list[dict[str, str]]],
        tools: Optional[list[dict[str, Any]]] = None,
        callbacks: Optional[list[Any]] = None,
        available_functions: Optional[dict[str, Any]] = None,
    ) -> str:
        self.calls.append(
            {
                "messages": to_jsonable(messages),
                "tools": to_jsonable(tools),
                "available_functions": sorted((available_functions or {}).keys()),
            }
        )
        index = min(len(self.calls) - 1, len(self.responses) - 1)
        return f"Final Answer: {self.responses[index]}"

    def supports_function_calling(self) -> bool:
        return False

    def supports_stop_words(self) -> bool:
        return False

    def get_context_window_size(self) -> int:
        return 8192


def build_crewai_objects() -> tuple[Crew, list[Task], dict[str, ScriptedLLM]]:
    llms = {
        definition["name"]: ScriptedLLM(definition["name"], [definition["response"]])
        for definition in AGENTS
    }
    agents = {
        definition["name"]: Agent(
            role=definition["title"],
            goal=definition["goal"],
            backstory=definition["backstory"],
            llm=llms[definition["name"]],
            allow_delegation=False,
            verbose=False,
            max_iter=1,
        )
        for definition in AGENTS
    }
    plan_task = Task(
        description=TASK_TEXT,
        expected_output="A concise adapter interop plan.",
        agent=agents["planner"],
    )
    research_task = Task(
        description="Research the CrewAI-specific task and context fields.",
        expected_output="CrewAI framework-specific projection evidence.",
        agent=agents["researcher"],
        context=[plan_task],
    )
    review_task = Task(
        description="Review the adapter mapping and approve the projection.",
        expected_output="An approval decision for the generated projection fixture.",
        agent=agents["reviewer"],
        context=[plan_task, research_task],
    )
    tasks = [plan_task, research_task, review_task]
    crew = Crew(
        agents=list(agents.values()),
        tasks=tasks,
        process=Process.sequential,
        verbose=False,
        memory=False,
    )
    return crew, tasks, llms


def run_crewai_capture() -> tuple[Any, list[Task], dict[str, ScriptedLLM]]:
    crew, tasks, llms = build_crewai_objects()
    result = crew.kickoff()
    return result, tasks, llms


def task_output_text(task: Task, fallback: str) -> str:
    output = getattr(task, "output", None)
    if output is None:
        return fallback
    raw = getattr(output, "raw", None)
    if raw:
        return str(raw)
    return str(output)


def build_projection(result: Any, tasks: list[Task]) -> dict[str, Any]:
    task_outputs = [
        task_output_text(task, AGENTS[index]["response"])
        for index, task in enumerate(tasks)
    ]
    return {
        "projection_id": "multi_agent_task",
        "projection_version": "v1",
        "source_request_id": REQUEST_ID,
        "source_session_id": CONTEXT_ID,
        "source_agent_did": "did:defra-agent:crewai-crew",
        "source_behavior_id": "crewai.sequential_crew",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "defra-agent",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:defra-agent:crewai-fixture-reader",
        },
        "output": {
            "adapter": "multi_agent_task",
            "projection": {
                "task_id": REQUEST_ID,
                "context_id": CONTEXT_ID,
                "status": crew_status(result),
                "participants": [
                    {
                        "agent_did": definition["agent_did"],
                        "behavior_id": definition["behavior_id"],
                        "role": definition["role"],
                    }
                    for definition in AGENTS
                ],
                "messages": [
                    {
                        "id": f"crewai:task-output:{definition['name']}",
                        "request_id": request_id_for_agent(definition["name"]),
                        "role": definition["role"],
                        "content": task_outputs[index],
                    }
                    for index, definition in enumerate(AGENTS)
                ],
                "delegations": [
                    {
                        "parent_request_id": REQUEST_ID,
                        "child_request_id": RESEARCH_REQUEST_ID,
                        "parent_tool_call_id": "crewai:context:planner-to-researcher",
                        "agent_did": "did:defra-agent:crewai-researcher",
                        "behavior_id": "crewai.researcher",
                        "status": "completed",
                    },
                    {
                        "parent_request_id": RESEARCH_REQUEST_ID,
                        "child_request_id": REVIEW_REQUEST_ID,
                        "parent_tool_call_id": "crewai:context:researcher-to-reviewer",
                        "agent_did": "did:defra-agent:crewai-reviewer",
                        "behavior_id": "crewai.reviewer",
                        "status": "completed",
                    },
                ],
                "tool_events": [
                    {
                        "id": "crewai:event:context:planner-to-researcher",
                        "request_id": REQUEST_ID,
                        "tool_name": "task_context",
                        "status": "completed",
                        "child_request_id": RESEARCH_REQUEST_ID,
                    },
                    {
                        "id": "crewai:event:context:researcher-to-reviewer",
                        "request_id": RESEARCH_REQUEST_ID,
                        "tool_name": "task_context",
                        "status": "completed",
                        "child_request_id": REVIEW_REQUEST_ID,
                    },
                    {
                        "id": "crewai:event:review:approval",
                        "request_id": REVIEW_REQUEST_ID,
                        "tool_name": "review",
                        "status": "completed",
                    },
                ],
            },
        },
    }


def crew_status(result: Any) -> str:
    raw = str(getattr(result, "raw", result))
    if "APPROVE" in raw:
        return "completed"
    return "stopped"


def request_id_for_agent(agent_name: str) -> str:
    if agent_name == "researcher":
        return RESEARCH_REQUEST_ID
    if agent_name == "reviewer":
        return REVIEW_REQUEST_ID
    return REQUEST_ID


def native_task(task: Task, index: int, all_tasks: list[Task]) -> dict[str, Any]:
    agent = getattr(task, "agent", None)
    context_value = getattr(task, "context", None)
    context = context_value if isinstance(context_value, (list, tuple)) else []
    task_indexes = {id(other): other_index for other_index, other in enumerate(all_tasks)}
    return {
        "index": index,
        "description": getattr(task, "description", None),
        "expected_output": getattr(task, "expected_output", None),
        "agent": {
            "role": getattr(agent, "role", None),
            "goal": getattr(agent, "goal", None),
        },
        "context_task_indexes": [
            task_indexes[id(other)]
            for other in context
            if isinstance(other, Task) and id(other) in task_indexes
        ],
        "output": to_jsonable(getattr(task, "output", None)),
    }


def to_jsonable(value: Any) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, dict):
        return {str(key): to_jsonable(item) for key, item in value.items()}
    if isinstance(value, (list, tuple, set)):
        return [to_jsonable(item) for item in value]
    if hasattr(value, "model_dump"):
        return to_jsonable(value.model_dump())
    if hasattr(value, "_asdict"):
        return to_jsonable(value._asdict())
    return str(value)


def crewai_version() -> str:
    return importlib.metadata.version("crewai")


def write_fixture(
    out_dir: Path,
    result: Any,
    tasks: list[Task],
    llms: dict[str, ScriptedLLM],
) -> Path:
    out_dir.mkdir(parents=True, exist_ok=True)
    payload = {
        "source": {
            "system": "crewai",
            "package": "crewai",
            "package_version": crewai_version(),
            "generator": "adapter-projections/generators/crewai",
            "capture": os.environ.get("DEFRA_FIXTURE_CAPTURE", "local"),
            "api": [
                "Agent",
                "Task",
                "Crew",
                "Process.sequential",
                "BaseLLM",
                "Crew.kickoff",
            ],
        },
        "native": {
            "crew": {
                "process": str(Process.sequential),
                "task_count": len(tasks),
                "agents": [
                    {
                        "name": definition["name"],
                        "role": definition["role"],
                        "goal": definition["goal"],
                    }
                    for definition in AGENTS
                ],
            },
            "task": TASK_TEXT,
            "result": to_jsonable(result),
            "tasks": [
                native_task(task, index, tasks)
                for index, task in enumerate(tasks)
            ],
            "llm_calls": {name: llm.calls for name, llm in llms.items()},
        },
        "envelope": build_projection(result, tasks),
    }
    path = out_dir / "multi_agent_task.crewai_sequential.capture.json"
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--out-dir", type=Path, default=Path("/out"))
    args = parser.parse_args()

    result, tasks, llms = run_crewai_capture()
    print(write_fixture(args.out_dir, result, tasks, llms))


if __name__ == "__main__":
    main()
