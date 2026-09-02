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
  const [selectedToolSelectionId, setSelectedToolSelectionId] = useState<string | null>(
    null,
  );
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
      selectedDeployment.behaviors.find((behavior) => behavior.isDefault) ??
      selectedDeployment.behaviors[0] ??
      null
    );
  }, [selectedBehaviorId, selectedConfigBehaviorId, selectedDeployment]);

  useEffect(() => {
    if (!selectedDeployment) {
      setSelectedConfigBehaviorId(null);
      setSelectedBackendId(null);
      setSelectedProfileId(null);
      setSelectedToolSelectionId(null);
      setSelectedToolServiceId(null);
      setSelectedSkillId(null);
      setSelectedTaskId(null);
      setSelectedScheduleId(null);
      setSelectedEventTriggerId(null);
      return;
    }

    ensureSelection(
      selectedConfigBehaviorId,
      selectedBehaviorId ??
        selectedDeployment.defaultBehaviorId ??
        selectedDeployment.behaviors.find((behavior) => behavior.isDefault)
          ?.behaviorId ??
        selectedDeployment.behaviors[0]?.behaviorId ??
        null,
      (id) =>
        selectedDeployment.behaviors.some((behavior) => behavior.behaviorId === id),
      setSelectedConfigBehaviorId,
    );
    ensureSelection(
      selectedBackendId,
      selectedBehavior?.backendId ??
        selectedDeployment.inferenceBackends[0]?.backendId ??
        null,
      (id) =>
        selectedDeployment.inferenceBackends.some(
          (backend) => backend.backendId === id,
        ),
      setSelectedBackendId,
    );
    ensureSelection(
      selectedProfileId,
      selectedBehavior?.inferenceProfileId ??
        selectedDeployment.inferenceProfiles[0]?.profileId ??
        null,
      (id) =>
        selectedDeployment.inferenceProfiles.some(
          (profile) => profile.profileId === id,
        ),
      setSelectedProfileId,
    );
    ensureSelection(
      selectedToolSelectionId,
      selectedBehavior?.toolSelectionId ??
        selectedDeployment.toolSelections[0]?.selectionId ??
        null,
      (id) =>
        selectedDeployment.toolSelections.some(
          (selection) => selection.selectionId === id,
        ),
      setSelectedToolSelectionId,
    );
    ensureSelection(
      selectedToolServiceId,
      selectedDeployment.toolServiceRegistries[0]?.serviceId ?? null,
      (id) =>
        selectedDeployment.toolServiceRegistries.some(
          (service) => service.serviceId === id,
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
    selectedToolSelectionId,
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
    if (
      behavior.backendId &&
      selectedDeployment.inferenceBackends.some(
        (backend) => backend.backendId === behavior.backendId,
      )
    ) {
      setSelectedBackendId(behavior.backendId);
    }
    if (
      behavior.inferenceProfileId &&
      selectedDeployment.inferenceProfiles.some(
        (profile) => profile.profileId === behavior.inferenceProfileId,
      )
    ) {
      setSelectedProfileId(behavior.inferenceProfileId);
    }
    if (
      behavior.toolSelectionId &&
      selectedDeployment.toolSelections.some(
        (selection) => selection.selectionId === behavior.toolSelectionId,
      )
    ) {
      setSelectedToolSelectionId(behavior.toolSelectionId);
    }
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
    selectedToolSelectionId,
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
    setSelectedToolSelectionId,
    setSelectedToolServiceId,
  };
}
