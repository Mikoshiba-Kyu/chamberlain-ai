import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ActivityEvent {
  ts: number;
  source: string;
  message: string;
}

export interface TriggerListItem {
  id: string;
  name: string;
  description: string | null;
  paused: boolean;
}

export interface DeclaredSecretItem {
  name: string;
  requiredBy: string[];
}

export const chamberlainApi = {
  listTriggers: () => invoke<TriggerListItem[]>("list_triggers"),
  pauseTrigger: (id: string) => invoke<void>("pause_trigger", { id }),
  resumeTrigger: (id: string) => invoke<void>("resume_trigger", { id }),
  onActivity: (cb: (ev: ActivityEvent) => void): Promise<UnlistenFn> =>
    listen<ActivityEvent>("activity", (e) => cb(e.payload)),

  listDeclaredSecrets: () => invoke<DeclaredSecretItem[]>("list_declared_secrets"),
  hasSecret: (name: string) => invoke<boolean>("has_secret", { name }),
  setSecret: (name: string, value: string) =>
    invoke<void>("set_secret", { name, value }),
  deleteSecret: (name: string) => invoke<void>("delete_secret", { name }),
};
