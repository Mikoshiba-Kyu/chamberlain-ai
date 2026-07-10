import { useState } from "react";
import { TriggersPanel } from "./panels/TriggersPanel";
import { ChatPanel } from "./panels/ChatPanel";
import { SettingsPanel } from "./panels/SettingsPanel";

type TabId = "triggers" | "chat" | "settings";

const TABS: { id: TabId; label: string }[] = [
  { id: "triggers", label: "トリガー" },
  { id: "chat", label: "チャット" },
  { id: "settings", label: "設定" },
];

export function App() {
  const [active, setActive] = useState<TabId>("triggers");

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
        {active === "triggers" && <TriggersPanel />}
        {active === "chat" && <ChatPanel />}
        {active === "settings" && <SettingsPanel />}
      </main>
    </div>
  );
}
