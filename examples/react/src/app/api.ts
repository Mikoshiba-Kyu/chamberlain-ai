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

export const chamberlainApi = {
  listTriggers: () => invoke<TriggerListItem[]>("list_triggers"),
  pauseTrigger: (id: string) => invoke<void>("pause_trigger", { id }),
  resumeTrigger: (id: string) => invoke<void>("resume_trigger", { id }),
  onActivity: (cb: (ev: ActivityEvent) => void): Promise<UnlistenFn> =>
    listen<ActivityEvent>("activity", (e) => cb(e.payload)),
};
