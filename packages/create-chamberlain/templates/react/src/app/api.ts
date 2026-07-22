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
  /**
   * schedule DSL の系統。`nextFireAt` の意味論がこれで分岐する:
   * - `"interval"`: `last_fire + duration`。過去値になり得る (missed-fire は catch-up される)
   * - `"wall-clock"`: 常に未来 (missed-fire は skip される)
   *
   * UI で「遅延中バッジ」を出す/出さないを判断するのに使う。
   */
  scheduleType: "interval" | "wall-clock";
  /**
   * 次回発火予定時刻 (ms since epoch)。
   * まだ 1 度も fire していないトリガー / error 付きは null。
   * framework が持っている情報を露出しているだけで、UI 側の表示レイアウトは #17 Phase 1 外。
   */
  nextFireAt: number | null;
  /**
   * 起動時 discovery で見つかった構成エラー (例: schedule パース失敗・下限違反)。
   * このフィールドが非 null の間、そのトリガーは load/tick されない。
   * UI 上で「壊れてる」ことを可視化するのが目的 (activity は startup 時に UI 未接続で
   * 捨てられる可能性が高いため、ここが実質的な観測面)。
   */
  error: string | null;
}

export interface DeclaredSecretItem {
  name: string;
  requiredBy: string[];
}

export interface ChatMessage {
  role: "user" | "assistant";
  content: string;
  ts: number;
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

  chatHistory: () => invoke<ChatMessage[]>("chat_history"),
  chatSend: (message: string) => invoke<ChatMessage>("chat_send", { message }),
  chatClear: () => invoke<void>("chat_clear"),
};
