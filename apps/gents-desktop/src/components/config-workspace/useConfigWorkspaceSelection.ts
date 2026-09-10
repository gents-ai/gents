import { useEffect, useMemo, useState } from "react";

import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { ensureSelection, type ConfigTab } from "./model";

export function useConfigWorkspaceSelection(
  selectedDeployment: DeploymentView | null,
  selectedBehaviorId: string | null,
  initialTab: ConfigTab = "behavior",
) {
  const [activeTab, setActiveTab] = useState<ConfigTab>(initialTab);
  const [selectedConfigBehaviorId, setSelectedConfigBehaviorId] = useState<
    string | null
  >(null);
  const [selectedBackendId, setSelectedBackendId] = useState<string | null>(null);
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(null);
  const [selectedToolsId, setSelectedToolsId] = useState<string | null>(null);
  const [selectedToolServiceId, setSelectedToolServiceId] = useState<string | null>(
    null,
  );
  const [selectedSkillId, setSelectedSkillId] = useState<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [selectedScheduleId, setSelectedScheduleId] = useState<string | null>(null);
  const [selectedEventTriggerId, setSelectedEventTriggerId] = useState<string | null>(
    null,
  );
  const [savedStatus, setSavedStatus] = useState<string | null>(null);

  const selectedBehavior = useMemo(() => {
    if (!selectedDeployment) {
      return null;
    }
    return (
      selectedDeployment.behaviors.find(
        (behavior) => behavior.behaviorId === selectedConfigBehaviorId,
      ) ??
      selectedDeployment.behaviors.find(
        (behavior) => behavior.behaviorId === selectedBehaviorId,
      ) ??
      selectedDeployment.behaviors.find(
        (behavior) =>
          behavior.behaviorId === selectedDeployment.agentPrincipal.defaultBehaviorId,
      ) ??
      null
    );
  }, [selectedBehaviorId, selectedConfigBehaviorId, selectedDeployment]);

  useEffect(() => {
    if (!selectedDeployment) {
      setSelectedConfigBehaviorId(null);
      setSelectedBackendId(null);
      setSelectedProfileId(null);
      setSelectedToolsId(null);
      setSelectedToolServiceId(null);
      setSelectedSkillId(null);
      setSelectedTaskId(null);
      setSelectedScheduleId(null);
      setSelectedEventTriggerId(null);
      return;
    }

    ensureSelection(
      selectedConfigBehaviorId,
      selectedBehaviorId ?? selectedDeployment.agentPrincipal.defaultBehaviorId ?? null,
      (id) =>
        selectedDeployment.behaviors.some((behavior) => behavior.behaviorId === id),
      setSelectedConfigBehaviorId,
    );
    ensureSelection(
      selectedBackendId,
      selectedDeployment.inferenceProfiles.find(
        (profile) => profile.profile_id === selectedBehavior?.inferenceProfileId,
      )?.backend_id ?? null,
      (id) =>
        selectedDeployment.inferenceBackends.some(
          (backend) => backend.backendId === id,
        ),
      setSelectedBackendId,
    );
    ensureSelection(
      selectedProfileId,
      selectedBehavior?.inferenceProfileId ?? null,
      (id) =>
        selectedDeployment.inferenceProfiles.some(
          (profile) => profile.profile_id === id,
        ),
      setSelectedProfileId,
    );
    ensureSelection(
      selectedToolsId,
      selectedDeployment.contexts.find(
        (context) => context.context_id === selectedBehavior?.contextId,
      )?.tools_id ?? null,
      (id) => selectedDeployment.tools.some((selection) => selection.tools_id === id),
      setSelectedToolsId,
    );
    ensureSelection(
      selectedToolServiceId,
      selectedDeployment.toolServiceRegistries[0]?.serviceId ?? null,
      (id) =>
        selectedDeployment.toolServiceRegistries.some(
          (service) => service.service_id === id,
        ),
      setSelectedToolServiceId,
    );
    ensureSelection(
      selectedSkillId,
      (selectedDeployment.skills ?? [])[0]?.skillId ?? null,
      (id) => (selectedDeployment.skills ?? []).some((skill) => skill.skillId === id),
      setSelectedSkillId,
    );
    ensureSelection(
      selectedTaskId,
      selectedDeployment.tasks[0]?.taskId ?? null,
      (id) => selectedDeployment.tasks.some((task) => task.taskId === id),
      setSelectedTaskId,
    );
    ensureSelection(
      selectedScheduleId,
      selectedDeployment.schedules[0]?.scheduleId ?? null,
      (id) =>
        selectedDeployment.schedules.some((schedule) => schedule.scheduleId === id),
      setSelectedScheduleId,
    );
    ensureSelection(
      selectedEventTriggerId,
      selectedDeployment.eventTriggers[0]?.triggerId ?? null,
      (id) =>
        selectedDeployment.eventTriggers.some((trigger) => trigger.triggerId === id),
      setSelectedEventTriggerId,
    );
  }, [
    selectedBackendId,
    selectedBehavior,
    selectedBehaviorId,
    selectedConfigBehaviorId,
    selectedDeployment,
    selectedEventTriggerId,
    selectedProfileId,
    selectedScheduleId,
    selectedSkillId,
    selectedTaskId,
    selectedToolsId,
    selectedToolServiceId,
  ]);

  function selectConfigBehavior(behaviorId: string | null) {
    setSelectedConfigBehaviorId(behaviorId);
    if (behaviorId == null) {
      return;
    }
    const behavior = selectedDeployment?.behaviors.find(
      (candidate) => candidate.behaviorId === behaviorId,
    );
    if (!behavior || !selectedDeployment) {
      return;
    }
    const backendId = selectedDeployment.inferenceProfiles.find(
      (profile) => profile.profile_id === behavior.inferenceProfileId,
    )?.backend_id;
    if (
      backendId &&
      selectedDeployment.inferenceBackends.some(
        (backend) => backend.backendId === backendId,
      )
    )
      setSelectedBackendId(backendId);
    if (
      behavior.inferenceProfileId &&
      selectedDeployment.inferenceProfiles.some(
        (profile) => profile.profile_id === behavior.inferenceProfileId,
      )
    ) {
      setSelectedProfileId(behavior.inferenceProfileId);
    }
    const toolsId = selectedDeployment.contexts.find(
      (context) => context.context_id === behavior.contextId,
    )?.tools_id;
    if (toolsId && selectedDeployment.tools.some((tools) => tools.tools_id === toolsId))
      setSelectedToolsId(toolsId);
  }

  return {
    activeTab,
    savedStatus,
    selectConfigBehavior,
    selectedBackendId,
    selectedBehavior,
    selectedConfigBehaviorId,
    selectedEventTriggerId,
    selectedProfileId,
    selectedScheduleId,
    selectedSkillId,
    selectedTaskId,
    selectedToolsId,
    selectedToolServiceId,
    setActiveTab,
    setSavedStatus,
    setSelectedBackendId,
    setSelectedConfigBehaviorId,
    setSelectedEventTriggerId,
    setSelectedProfileId,
    setSelectedScheduleId,
    setSelectedSkillId,
    setSelectedTaskId,
    setSelectedToolsId,
    setSelectedToolServiceId,
  };
}
