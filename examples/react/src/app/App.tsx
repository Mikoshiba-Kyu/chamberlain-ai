import { useEffect, useState } from "react";
import { TriggersPanel } from "./panels/TriggersPanel";
import { ActivityPanel } from "./panels/ActivityPanel";
import { ChatPanel } from "./panels/ChatPanel";
import { SettingsPanel } from "./panels/SettingsPanel";
import { chamberlainApi, type ActivityEvent, type TriggerListItem } from "./api";

type TabId = "triggers" | "activity" | "chat" | "settings";

const TABS: { id: TabId; label: string }[] = [
  { id: "triggers", label: "トリガー" },
  { id: "activity", label: "アクティビティ" },
  { id: "chat", label: "チャット" },
  { id: "settings", label: "設定" },
];

const MAX_EVENTS = 200;

export function App() {
  const [active, setActive] = useState<TabId>("triggers");
  const [events, setEvents] = useState<ActivityEvent[]>([]);
  const [triggers, setTriggers] = useState<TriggerListItem[]>([]);

  const refreshTriggers = () => {
    chamberlainApi.listTriggers().then(setTriggers);
  };

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
    refreshTriggers();
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const toggleTrigger = async (id: string) => {
    const target = triggers.find((t) => t.id === id);
    if (!target) return;
    if (target.paused) {
      await chamberlainApi.resumeTrigger(id);
    } else {
      await chamberlainApi.pauseTrigger(id);
    }
    refreshTriggers();
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
          <TriggersPanel triggers={triggers} onToggle={toggleTrigger} />
        )}
        {active === "activity" && <ActivityPanel events={events} />}
        {active === "chat" && <ChatPanel />}
        {active === "settings" && <SettingsPanel />}
      </main>
    </div>
  );
}
