import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ActivityEvent {
  ts: number;
  source: string;
  message: string;
}

export const chamberlainApi = {
  pauseSampleTrigger: () => invoke<void>("pause_sample_trigger"),
  resumeSampleTrigger: () => invoke<void>("resume_sample_trigger"),
  sampleTriggerPaused: () => invoke<boolean>("sample_trigger_status"),
  onActivity: (cb: (ev: ActivityEvent) => void): Promise<UnlistenFn> =>
    listen<ActivityEvent>("activity", (e) => cb(e.payload)),
};
