import { useCallback, useEffect, useState } from "react";
import { TriggersPanel } from "./panels/TriggersPanel";
import { TasksPanel } from "./panels/TasksPanel";
import { ActivityPanel } from "./panels/ActivityPanel";
import { ChatPanel } from "./panels/ChatPanel";
import { SettingsPanel } from "./panels/SettingsPanel";
import {
  chamberlainApi,
  type ActivityEvent,
  type TaskListItem,
  type TriggerListItem,
} from "./api";

type TabId = "triggers" | "tasks" | "activity" | "chat" | "settings";

const TABS: { id: TabId; label: string }[] = [
  { id: "triggers", label: "トリガー" },
  { id: "tasks", label: "予定" },
  { id: "activity", label: "アクティビティ" },
  { id: "chat", label: "チャット" },
  { id: "settings", label: "設定" },
];

const MAX_EVENTS = 200;

/**
 * 予定リストの再取得間隔。心拍 (prod 1m) で due なタスクが消えていくので、
 * 開いている間はゆるくポーリングして「今何が積まれているか」を追う。
 */
const TASKS_POLL_MS = 15_000;

export function App() {
  const [active, setActive] = useState<TabId>("triggers");
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [triggers, setTriggers] = useState<TriggerListItem[]>([]);
  const [tasks, setTasks] = useState<TaskListItem[]>([]);

  // トリガーの nextFireAt は予定リストの投影なので、常に両方まとめて取り直す。
  const refresh = useCallback(() => {
    chamberlainApi.listTriggers().then(setTriggers);
    chamberlainApi.listTasks().then(setTasks);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    chamberlainApi
      .onActivity((ev) => {
        setEvents((prev) => [ev, ...prev].slice(0, MAX_EVENTS));
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });
    refresh();
    const timer = setInterval(refresh, TASKS_POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
      unlisten?.();
    };
  }, [refresh]);

  const toggleTrigger = async (id: string) => {
    const target = triggers.find((t) => t.id === id);
    if (!target) return;
    if (target.paused) {
      await chamberlainApi.resumeTrigger(id);
    } else {
      await chamberlainApi.pauseTrigger(id);
    }
    refresh();
  };

  const runTriggerNow = async (id: string) => {
    await chamberlainApi.runTriggerNow(id);
    refresh();
  };

  const deleteTask = async (id: string) => {
    await chamberlainApi.deleteTask(id);
    refresh();
  };

  return (
    <div className="app">
      <nav className="sidebar">
        <div className="brand">Chamberlain</div>
        <ul className="tabs">
          {TABS.map((tab) => (
            <li key={tab.id}>
              <button
                className={active === tab.id ? "tab active" : "tab"}
                onClick={() => setActive(tab.id)}
              >
                {tab.label}
              </button>
            </li>
          ))}
        </ul>
      </nav>
      <main className="content">
        {active === "triggers" && (
          <TriggersPanel
            triggers={triggers}
            onToggle={toggleTrigger}
            onRunNow={runTriggerNow}
          />
        )}
        {active === "tasks" && (
          <TasksPanel tasks={tasks} onDelete={deleteTask} />
        )}
        {active === "activity" && <ActivityPanel events={events} />}
        {active === "chat" && <ChatPanel />}
        {active === "settings" && <SettingsPanel />}
      </main>
    </div>
  );
}
