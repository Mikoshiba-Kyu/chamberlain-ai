import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ActivityEvent {
  ts: number;
  /** トリガー ID。トリガーに紐付かないものは `"__task__"`。 */
  source: string;
  /**
   * 種別の安定した識別子 (`"notify"` / `"skipped"` / `"expanded"` 等)。
   *
   * 0.3.0 で追加 (#42)。`message` の `[...]` プレフィックスはこれから組み立てられて
   * いるので、フィルタや表示の出し分けは文字列パースではなくこちらを見る。
   */
  kind: string;
  /** 表示用の 1 行。 */
  message: string;
  /** 元になったタスクのスナップショット。展開など、タスクに紐付かないイベントでは undefined。 */
  taskId?: string;
  taskOrigin?: "schedule" | "adhoc";
  /** 実行を意図された時刻。遅延は `ts - scheduledAt` で導出する。 */
  scheduledAt?: number;
}

export interface TriggerListItem {
  id: string;
  name: string;
  description: string | null;
  paused: boolean;
  /**
   * manifest に宣言された生の schedule 文字列 (`"@daily 09:00"` 等)。
   *
   * 0.2.0 で `scheduleType` は削除された。interval 系統が廃止されて wall-clock のみに
   * なったため、`nextFireAt` の意味論が分岐しなくなった (#26 決定事項 4)。
   */
  schedule: string;
  /**
   * タスクリスト上でこのトリガーに積まれている最も早い予定時刻 (ms since epoch)。
   *
   * これは**タスクリストの投影**であり、framework が別に持っている「次回発火予定」では
   * ない (#26 決定事項 2)。エンドユーザーがタスクを削除すればここも消える。
   * 展開前・構成エラー・全タスク削除済みの場合は null。
   */
  nextFireAt: number | null;
  /**
   * 起動時 discovery で見つかった構成エラー (例: schedule パース失敗)。
   * このフィールドが非 null の間、そのトリガーは load / 展開されない。
   * UI 上で「壊れてる」ことを可視化するのが目的 (activity は startup 時に UI 未接続で
   * 捨てられる可能性が高いため、ここが実質的な観測面)。
   */
  error: string | null;
}

/**
 * タスクリストの 1 件 = 「秘書がこれからやるつもりのこと」。
 *
 * pending のみが載る。終わったタスクは即座に消える (履歴は activity 側)。
 * #26 決定事項 1 の「未来への意図」と「過去の記録」の分離がこの型に現れている。
 */
export interface TaskListItem {
  id: string;
  /**
   * `"schedule"` = manifest の展開器が生成した / `"adhoc"` = 手動実行や
   * (将来) 秘書 AI・チャットが積んだもの。
   *
   * 遅れて due になったときの扱いが origin で違う: schedule 由来は破棄され、
   * ad-hoc は猶予 (24h) 内なら遅延を明示して実行される (#26 決定事項 8)。
   */
  origin: "schedule" | "adhoc";
  triggerId: string | null;
  /** 実行対象トリガーの表示名。解決できない場合は null。 */
  triggerName: string | null;
  /** 実行を意図された絶対時刻 (ms since epoch)。 */
  scheduledAt: number;
  /** リストに積まれた時刻 (ms since epoch)。 */
  createdAt: number;
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
  /**
   * トリガーを今すぐ 1 回実行する。実装は「即 due な ad-hoc タスクを積んで心拍を起こす」
   * なので、定期スケジュール (展開済み境界) は乱れない。
   */
  runTriggerNow: (id: string) => invoke<void>("run_trigger_now", { id }),
  onActivity: (cb: (ev: ActivityEvent) => void): Promise<UnlistenFn> =>
    listen<ActivityEvent>("activity", (e) => cb(e.payload)),
  /**
   * 保存済みの履歴を新しい順に取る (#42)。
   *
   * worker は `.setup()` 内で動き出すため、`[schedule error]` や `[expanded]` は
   * この webview のリスナーが繋がる前に emit されて捨てられる。**起動時のイベントを
   * 見るにはこれを読む必要がある。**
   */
  listActivity: (limit?: number) =>
    invoke<ActivityEvent[]>("list_activity", { limit }),

  listTasks: () => invoke<TaskListItem[]>("list_tasks"),
  /**
   * 予定を 1 件削除する。展開済み境界があるので、消したタスクが次の展開で
   * 復活することはない (#26 決定事項 3)。
   */
  deleteTask: (id: string) => invoke<void>("delete_task", { id }),

  listDeclaredSecrets: () => invoke<DeclaredSecretItem[]>("list_declared_secrets"),
  hasSecret: (name: string) => invoke<boolean>("has_secret", { name }),
  setSecret: (name: string, value: string) =>
    invoke<void>("set_secret", { name, value }),
  deleteSecret: (name: string) => invoke<void>("delete_secret", { name }),

  chatHistory: () => invoke<ChatMessage[]>("chat_history"),
  chatSend: (message: string) => invoke<ChatMessage>("chat_send", { message }),
  chatClear: () => invoke<void>("chat_clear"),
};
