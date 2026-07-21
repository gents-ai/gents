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
TASK_TEXT = "Map a CrewAI multi-agent task to Gents projection fields."

HIERARCHICAL_CONTEXT_ID = "context-crewai-hierarchical-docker-1"
HIERARCHICAL_REQUEST_ID = "req-crewai-hierarchical-docker-1"
HIERARCHICAL_RESEARCH_REQUEST_ID = "req-crewai-hierarchical-research-docker-1"
HIERARCHICAL_REVIEW_REQUEST_ID = "req-crewai-hierarchical-review-docker-1"
HIERARCHICAL_TASK_TEXT = (
    "Manage a CrewAI hierarchical crew for Gents projection fields."
)


AGENTS = [
    {
        "name": "planner",
        "role": "planner",
        "title": "CrewAI Planner",
        "agent_did": "did:test:crewai-planner",
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
        "agent_did": "did:test:crewai-researcher",
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
        "agent_did": "did:test:crewai-reviewer",
        "behavior_id": "crewai.reviewer",
        "goal": "Review and approve adapter output.",
        "backstory": "Approves the final mapped multi-agent task projection.",
        "response": "APPROVE: CrewAI projection output is ready",
    },
]

HIERARCHICAL_MANAGER = {
    "name": "manager",
    "role": "manager",
    "title": "CrewAI Manager",
    "agent_did": "did:test:crewai-manager",
    "behavior_id": "crewai.manager",
}

HIERARCHICAL_AGENTS = [
    {
        "name": "researcher",
        "role": "researcher",
        "title": "CrewAI Hierarchical Researcher",
        "agent_did": "did:test:crewai-hierarchical-researcher",
        "behavior_id": "crewai.hierarchical_researcher",
        "goal": "Research hierarchical CrewAI delegation behavior.",
        "backstory": "Receives delegated work from a hierarchical crew manager.",
        "request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
        "response": (
            "HIERARCHICAL_RESEARCH: manager delegation reached the researcher "
            "with task and context payloads."
        ),
    },
    {
        "name": "reviewer",
        "role": "reviewer",
        "title": "CrewAI Hierarchical Reviewer",
        "agent_did": "did:test:crewai-hierarchical-reviewer",
        "behavior_id": "crewai.hierarchical_reviewer",
        "goal": "Review hierarchical projection evidence.",
        "backstory": "Receives delegated review work from the manager.",
        "request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
        "response": (
            "HIERARCHICAL_REVIEW: manager delegation reached the reviewer "
            "after research context was available."
        ),
    },
]

HIERARCHICAL_MANAGER_RESPONSES = [
    (
        "Thought: Delegate the research step to the specialist.\n"
        "Action: delegate_work_to_coworker\n"
        "Action Input: {"
        '"coworker":"CrewAI Hierarchical Researcher",'
        '"task":"Research CrewAI hierarchical process evidence.",'
        '"context":"We need manager-to-worker child request evidence for the '
        'Gents multi_agent_task projection."}'
    ),
    (
        "Thought: The researcher returned the required evidence.\n"
        "Final Answer: HIERARCHICAL_MANAGER_RESEARCH: delegated research accepted"
    ),
    (
        "Thought: Delegate review to the reviewer with the research result.\n"
        "Action: delegate_work_to_coworker\n"
        "Action Input: {"
        '"coworker":"CrewAI Hierarchical Reviewer",'
        '"task":"Review CrewAI hierarchical projection evidence.",'
        '"context":"Research output is HIERARCHICAL_MANAGER_RESEARCH; verify '
        'that the projection keeps manager and child request boundaries."}'
    ),
    (
        "Thought: The reviewer approved the hierarchical projection evidence.\n"
        "Final Answer: HIERARCHICAL_APPROVE: delegated review accepted"
    ),
]


class ScriptedLLM(BaseLLM):
    def __init__(
        self,
        agent_name: str,
        responses: list[str],
        wrap_final_answer: bool = True,
    ) -> None:
        super().__init__(model=f"gents-scripted-{agent_name}", temperature=0)
        self.agent_name = agent_name
        self.responses = responses
        self.wrap_final_answer = wrap_final_answer
        self.calls: list[dict[str, Any]] = []

    def call(
        self,
        messages: Union[str, list[dict[str, str]]],
        tools: Optional[list[dict[str, Any]]] = None,
        callbacks: Optional[list[Any]] = None,
        available_functions: Optional[dict[str, Any]] = None,
        **kwargs: Any,
    ) -> str:
        index = min(len(self.calls), len(self.responses) - 1)
        raw_response = self.responses[index]
        response = (
            f"Final Answer: {raw_response}"
            if self.wrap_final_answer
            else raw_response
        )
        self.calls.append(
            {
                "messages": to_jsonable(messages),
                "tools": to_jsonable(tools),
                "available_functions": sorted((available_functions or {}).keys()),
                "execution": execution_kwargs_to_json(kwargs),
                "response": response,
            }
        )
        return response

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


def build_crewai_hierarchical_objects() -> tuple[Crew, list[Task], dict[str, ScriptedLLM]]:
    llms = {
        definition["name"]: ScriptedLLM(definition["name"], [definition["response"]])
        for definition in HIERARCHICAL_AGENTS
    }
    llms["manager"] = ScriptedLLM(
        "manager",
        HIERARCHICAL_MANAGER_RESPONSES,
        wrap_final_answer=False,
    )
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
        for definition in HIERARCHICAL_AGENTS
    }
    research_task = Task(
        description="Manager delegates research for adapter projection evidence.",
        expected_output="Hierarchical research evidence.",
        agent=agents["researcher"],
    )
    review_task = Task(
        description="Manager delegates review for adapter projection evidence.",
        expected_output="Hierarchical review approval.",
        agent=agents["reviewer"],
        context=[research_task],
    )
    tasks = [research_task, review_task]
    crew = Crew(
        agents=list(agents.values()),
        tasks=tasks,
        process=Process.hierarchical,
        manager_llm=llms["manager"],
        verbose=False,
        memory=False,
        tracing=False,
    )
    return crew, tasks, llms


def run_crewai_capture() -> tuple[Any, list[Task], dict[str, ScriptedLLM]]:
    crew, tasks, llms = build_crewai_objects()
    result = crew.kickoff()
    return result, tasks, llms


def run_crewai_hierarchical_capture() -> tuple[Any, list[Task], dict[str, ScriptedLLM]]:
    crew, tasks, llms = build_crewai_hierarchical_objects()
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


def llm_response_text(llm: ScriptedLLM, fallback: str) -> str:
    if not llm.calls:
        return fallback
    response = str(llm.calls[-1].get("response", fallback))
    prefix = "Final Answer: "
    if response.startswith(prefix):
        return response[len(prefix) :]
    return response


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
        "source_agent_did": "did:test:crewai-crew",
        "source_behavior_id": "crewai.sequential_crew",
        "redaction_mode": "full",
        "provenance": {
            "runtime": "gents",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:test:crewai-fixture-reader",
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
                        "agent_did": "did:test:crewai-researcher",
                        "behavior_id": "crewai.researcher",
                        "status": "completed",
                    },
                    {
                        "parent_request_id": RESEARCH_REQUEST_ID,
                        "child_request_id": REVIEW_REQUEST_ID,
                        "parent_tool_call_id": "crewai:context:researcher-to-reviewer",
                        "agent_did": "did:test:crewai-reviewer",
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


def build_hierarchical_projection(
    result: Any,
    tasks: list[Task],
    llms: dict[str, ScriptedLLM],
) -> dict[str, Any]:
    task_outputs = [
        task_output_text(task, HIERARCHICAL_MANAGER_RESPONSES[index * 2 + 1])
        for index, task in enumerate(tasks)
    ]
    researcher_output = llm_response_text(
        llms["researcher"],
        HIERARCHICAL_AGENTS[0]["response"],
    )
    reviewer_output = llm_response_text(
        llms["reviewer"],
        HIERARCHICAL_AGENTS[1]["response"],
    )
    return {
        "projection_id": "multi_agent_task",
        "projection_version": "v1",
        "source_request_id": HIERARCHICAL_REQUEST_ID,
        "source_session_id": HIERARCHICAL_CONTEXT_ID,
        "source_agent_did": HIERARCHICAL_MANAGER["agent_did"],
        "source_behavior_id": HIERARCHICAL_MANAGER["behavior_id"],
        "redaction_mode": "full",
        "provenance": {
            "runtime": "gents",
            "source_projection_id": "run_timeline",
            "source_projection_version": "v1",
            "actor_did": "did:test:crewai-fixture-reader",
        },
        "output": {
            "adapter": "multi_agent_task",
            "projection": {
                "task_id": HIERARCHICAL_REQUEST_ID,
                "context_id": HIERARCHICAL_CONTEXT_ID,
                "status": crew_status(result),
                "participants": [
                    {
                        "agent_did": HIERARCHICAL_MANAGER["agent_did"],
                        "behavior_id": HIERARCHICAL_MANAGER["behavior_id"],
                        "role": HIERARCHICAL_MANAGER["role"],
                    },
                    *[
                        {
                            "agent_did": definition["agent_did"],
                            "behavior_id": definition["behavior_id"],
                            "role": definition["role"],
                        }
                        for definition in HIERARCHICAL_AGENTS
                    ],
                ],
                "messages": [
                    {
                        "id": "crewai:hierarchical:manager:delegate-research",
                        "request_id": HIERARCHICAL_REQUEST_ID,
                        "role": HIERARCHICAL_MANAGER["role"],
                        "content": HIERARCHICAL_MANAGER_RESPONSES[0],
                    },
                    {
                        "id": "crewai:hierarchical:researcher:response",
                        "request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
                        "role": "researcher",
                        "content": researcher_output,
                    },
                    {
                        "id": "crewai:hierarchical:manager:research-final",
                        "request_id": HIERARCHICAL_REQUEST_ID,
                        "role": HIERARCHICAL_MANAGER["role"],
                        "content": task_outputs[0],
                    },
                    {
                        "id": "crewai:hierarchical:manager:delegate-review",
                        "request_id": HIERARCHICAL_REQUEST_ID,
                        "role": HIERARCHICAL_MANAGER["role"],
                        "content": HIERARCHICAL_MANAGER_RESPONSES[2],
                    },
                    {
                        "id": "crewai:hierarchical:reviewer:response",
                        "request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
                        "role": "reviewer",
                        "content": reviewer_output,
                    },
                    {
                        "id": "crewai:hierarchical:manager:review-final",
                        "request_id": HIERARCHICAL_REQUEST_ID,
                        "role": HIERARCHICAL_MANAGER["role"],
                        "content": task_outputs[1],
                    },
                ],
                "delegations": [
                    {
                        "parent_request_id": HIERARCHICAL_REQUEST_ID,
                        "child_request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
                        "parent_tool_call_id": (
                            "crewai:hierarchical:delegate:manager-to-researcher"
                        ),
                        "agent_did": HIERARCHICAL_AGENTS[0]["agent_did"],
                        "behavior_id": HIERARCHICAL_AGENTS[0]["behavior_id"],
                        "status": "completed",
                    },
                    {
                        "parent_request_id": HIERARCHICAL_REQUEST_ID,
                        "child_request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
                        "parent_tool_call_id": (
                            "crewai:hierarchical:delegate:manager-to-reviewer"
                        ),
                        "agent_did": HIERARCHICAL_AGENTS[1]["agent_did"],
                        "behavior_id": HIERARCHICAL_AGENTS[1]["behavior_id"],
                        "status": "completed",
                    },
                ],
                "tool_events": [
                    {
                        "id": "crewai:hierarchical:event:delegate-research",
                        "request_id": HIERARCHICAL_REQUEST_ID,
                        "tool_name": "delegate_work_to_coworker",
                        "status": "completed",
                        "child_request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
                    },
                    {
                        "id": "crewai:hierarchical:event:research-response",
                        "request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
                        "tool_name": "delegated_task_response",
                        "status": "completed",
                    },
                    {
                        "id": "crewai:hierarchical:event:delegate-review",
                        "request_id": HIERARCHICAL_REQUEST_ID,
                        "tool_name": "delegate_work_to_coworker",
                        "status": "completed",
                        "child_request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
                    },
                    {
                        "id": "crewai:hierarchical:event:review-response",
                        "request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
                        "tool_name": "delegated_task_response",
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


def execution_kwargs_to_json(kwargs: dict[str, Any]) -> dict[str, Any]:
    payload: dict[str, Any] = {}
    from_task = kwargs.get("from_task")
    if from_task is not None:
        payload["from_task"] = {
            "description": getattr(from_task, "description", None),
            "expected_output": getattr(from_task, "expected_output", None),
        }
    from_agent = kwargs.get("from_agent")
    if from_agent is not None:
        payload["from_agent"] = {
            "role": getattr(from_agent, "role", None),
            "goal": getattr(from_agent, "goal", None),
            "allow_delegation": getattr(from_agent, "allow_delegation", None),
        }
    response_model = kwargs.get("response_model")
    if response_model is not None:
        payload["response_model"] = str(response_model)
    return payload


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


def sequential_mapping(result: Any) -> dict[str, Any]:
    return {
        "projection": "multi_agent_task",
        "scenario_id": "crewai.sequential_crew",
        "request_id": REQUEST_ID,
        "session_id": CONTEXT_ID,
        "agent_did": "did:test:crewai-crew",
        "behavior_id": "crewai.sequential_crew",
        "actor_did": "did:test:crewai-fixture-reader",
        "status": crew_status(result),
        "participants": [
            {
                "native_name": definition["title"],
                "role": definition["role"],
                "agent_did": definition["agent_did"],
                "behavior_id": definition["behavior_id"],
                "request_id": request_id_for_agent(definition["name"]),
            }
            for definition in AGENTS
        ],
        "delegations": [
            {
                "parent_request_id": REQUEST_ID,
                "child_request_id": RESEARCH_REQUEST_ID,
                "parent_tool_call_id": "crewai:context:planner-to-researcher",
                "tool_name": "task_context",
                "agent_did": "did:test:crewai-researcher",
                "behavior_id": "crewai.researcher",
                "status": "completed",
            },
            {
                "parent_request_id": RESEARCH_REQUEST_ID,
                "child_request_id": REVIEW_REQUEST_ID,
                "parent_tool_call_id": "crewai:context:researcher-to-reviewer",
                "tool_name": "task_context",
                "agent_did": "did:test:crewai-reviewer",
                "behavior_id": "crewai.reviewer",
                "status": "completed",
            },
        ],
        "tool_events": [
            {
                "id": "crewai:event:review:approval",
                "request_id": REVIEW_REQUEST_ID,
                "tool_name": "review",
                "status": "completed",
            }
        ],
    }


def hierarchical_mapping(result: Any) -> dict[str, Any]:
    return {
        "projection": "multi_agent_task",
        "scenario_id": "crewai.hierarchical_crew",
        "request_id": HIERARCHICAL_REQUEST_ID,
        "session_id": HIERARCHICAL_CONTEXT_ID,
        "agent_did": HIERARCHICAL_MANAGER["agent_did"],
        "behavior_id": HIERARCHICAL_MANAGER["behavior_id"],
        "actor_did": "did:test:crewai-fixture-reader",
        "status": crew_status(result),
        "participants": [
            {
                "native_name": HIERARCHICAL_MANAGER["title"],
                "role": HIERARCHICAL_MANAGER["role"],
                "agent_did": HIERARCHICAL_MANAGER["agent_did"],
                "behavior_id": HIERARCHICAL_MANAGER["behavior_id"],
                "request_id": HIERARCHICAL_REQUEST_ID,
            },
            *[
                {
                    "native_name": definition["title"],
                    "role": definition["role"],
                    "agent_did": definition["agent_did"],
                    "behavior_id": definition["behavior_id"],
                    "request_id": definition["request_id"],
                }
                for definition in HIERARCHICAL_AGENTS
            ],
        ],
        "delegations": [
            {
                "parent_request_id": HIERARCHICAL_REQUEST_ID,
                "child_request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
                "parent_tool_call_id": (
                    "crewai:hierarchical:delegate:manager-to-researcher"
                ),
                "tool_name": "delegate_work_to_coworker",
                "agent_did": HIERARCHICAL_AGENTS[0]["agent_did"],
                "behavior_id": HIERARCHICAL_AGENTS[0]["behavior_id"],
                "status": "completed",
            },
            {
                "parent_request_id": HIERARCHICAL_REQUEST_ID,
                "child_request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
                "parent_tool_call_id": (
                    "crewai:hierarchical:delegate:manager-to-reviewer"
                ),
                "tool_name": "delegate_work_to_coworker",
                "agent_did": HIERARCHICAL_AGENTS[1]["agent_did"],
                "behavior_id": HIERARCHICAL_AGENTS[1]["behavior_id"],
                "status": "completed",
            },
        ],
        "tool_events": [
            {
                "id": "crewai:hierarchical:event:research-response",
                "request_id": HIERARCHICAL_RESEARCH_REQUEST_ID,
                "tool_name": "delegated_task_response",
                "status": "completed",
            },
            {
                "id": "crewai:hierarchical:event:review-response",
                "request_id": HIERARCHICAL_REVIEW_REQUEST_ID,
                "tool_name": "delegated_task_response",
                "status": "completed",
            },
        ],
    }


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
            "capture": os.environ.get("GENTS_FIXTURE_CAPTURE", "local"),
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
        "mapping": sequential_mapping(result),
        "envelope": build_projection(result, tasks),
    }
    path = out_dir / "multi_agent_task.crewai_sequential.capture.json"
    path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return path


def write_hierarchical_fixture(
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
            "capture": os.environ.get("GENTS_FIXTURE_CAPTURE", "local"),
            "api": [
                "Agent",
                "Task",
                "Crew",
                "Process.hierarchical",
                "BaseLLM",
                "Crew.kickoff",
                "delegate_work_to_coworker",
            ],
        },
        "native": {
            "crew": {
                "process": str(Process.hierarchical),
                "task_count": len(tasks),
                "manager": {
                    "name": HIERARCHICAL_MANAGER["name"],
                    "role": HIERARCHICAL_MANAGER["role"],
                    "behavior_id": HIERARCHICAL_MANAGER["behavior_id"],
                },
                "agents": [
                    {
                        "name": definition["name"],
                        "role": definition["role"],
                        "goal": definition["goal"],
                    }
                    for definition in HIERARCHICAL_AGENTS
                ],
            },
            "task": HIERARCHICAL_TASK_TEXT,
            "manager_responses": HIERARCHICAL_MANAGER_RESPONSES,
            "result": to_jsonable(result),
            "tasks": [
                native_task(task, index, tasks)
                for index, task in enumerate(tasks)
            ],
            "llm_calls": {name: llm.calls for name, llm in llms.items()},
        },
        "mapping": hierarchical_mapping(result),
        "envelope": build_hierarchical_projection(result, tasks, llms),
    }
    path = out_dir / "multi_agent_task.crewai_hierarchical.capture.json"
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
    hierarchical_result, hierarchical_tasks, hierarchical_llms = (
        run_crewai_hierarchical_capture()
    )
    for path in [
        write_fixture(args.out_dir, result, tasks, llms),
        write_hierarchical_fixture(
            args.out_dir,
            hierarchical_result,
            hierarchical_tasks,
            hierarchical_llms,
        ),
    ]:
        print(path)


if __name__ == "__main__":
    main()
