import { useApp } from "@/app/AppContext";
import type { NodeView } from "../../../hooks/fleetStore";
import { setEnabled } from "../agent/enabled";
import { RowMenu } from "../agent/RowMenu";
import { taskDocument } from "../agent/TasksPanel";
import { dependentsWarning } from "../agent/dependents";
import { newId } from "../agent/draft";
import { MANUAL, type Automation } from "./model";

/** An automation's on/off switch and its menu: open, duplicate (off, like
    any copy) and delete. A trigger is switched, copied and deleted on its
    own; a hand-run automation is its task. */
export function AutomationMenu({
  node,
  automation: a,
}: {
  node: NodeView;
  automation: Automation;
}) {
  const {
    actions: { changeConfig },
  } = useApp();
  const taskId = a.trigger ? null : a.id.slice(MANUAL.length);
  return (
    <RowMenu
      name={a.name}
      base={{ name: "automations" }}
      id={a.id}
      enabled={{
        checked: a.on,
        onChange: (on) =>
          a.trigger
            ? setEnabled(
                changeConfig,
                node.nodeDid,
                "Trigger",
                a.trigger.config.trigger_id,
                on,
              )
            : setEnabled(changeConfig, node.nodeDid, "Task", taskId!, on),
      }}
      onDuplicate={async () => {
        if (a.trigger) {
          const trigger_id = newId("trig");
          await changeConfig("saveTriggerConfig", {
            document: {
              ...a.trigger.config,
              trigger_id,
              display_name: `${a.name} copy`,
              enabled: false,
              created_at: null,
              updated_at: null,
            },
          });
          return trigger_id;
        }
        const task_id = newId("task");
        await changeConfig("saveTaskConfig", {
          document: {
            ...taskDocument(node, a.task!),
            task_id,
            display_name: `${a.name} copy`,
          },
        });
        return `${MANUAL}${task_id}`;
      }}
      onDelete={() =>
        a.trigger
          ? changeConfig("deleteTriggerConfig", {
              triggerId: a.trigger.config.trigger_id,
              nodeDid: node.nodeDid,
            })
          : changeConfig("deleteTaskConfig", { taskId: taskId!, nodeDid: node.nodeDid })
      }
      warning={taskId ? dependentsWarning(node, "task", taskId) : undefined}
    />
  );
}
