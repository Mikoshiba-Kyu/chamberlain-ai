import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface ActivityEvent {
  /**
   * 履歴上の行 id。live emit と `listActivity()` の結果を突き合わせる同一性に使う。
   * 履歴 DB が開けなかった環境では undefined になる。
   */
  id?: number;
  ts: number;
  /** トリガー ID。トリガーに紐付かないものは `"__task__"`。 */
  source: string;
  /**
   * 種別の安定した識別子 (`"notify"` / `"skipped"` / `"expanded"` 等)。
   *
   * `message` の `[...]` プレフィックスはこれから組み立てられているので、フィルタや
   * 表示の出し分けは文字列パースではなくこちらを見る。
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

/**
 * トリガーの出どころ (#58)。
 *
 * `"bundled"` = アプリに焼き込まれたもの (エージェント開発者のもの)。
 * `"registered"` = 実行時に `<app_data>/triggers/` へ登録されたもの。
 *
 * エンドユーザーが外せるのは後者だけ。「アプリの形」と「秘書にさせる仕事」の線引き。
 */
export type TriggerSource = "bundled" | "registered";

export interface TriggerListItem {
  id: string;
  name: string;
  description: string | null;
  paused: boolean;
  /** manifest に宣言された生の schedule 文字列 (`"@daily 09:00"` 等)。 */
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
   * 起動時 discovery で見つかった構成エラー (例: schedule パース失敗、allowedHosts の
   * 書式不正)。このフィールドが非 null の間、そのトリガーは load / 展開されない。
   * UI 上で「壊れてる」ことを可視化するのが目的。
   */
  error: string | null;
  /**
   * このトリガーが読める secret 名 (#56)。
   *
   * **表示用の飾りではなく、実際に効いている制限そのもの。** ここに無い名前を
   * `getSecret` に渡しても null しか返らない。
   */
  requiredSecrets: string[];
  /**
   * このトリガーが `http.fetch` で出られる宛先 (#57)。`"api.github.com"` や
   * `"*.example.com"` (サブドメインのみ)。空なら一切ネットワークに出られない。
   *
   * requiredSecrets と合わせて「このトリガーは何を読み、どこへ出るのか」になる。
   * 実行時登録 (#58) の同意画面はこの 2 つを見せる。
   */
  allowedHosts: string[];
  /** 焼き込みか実行時登録か (#58)。解除できるのは `"registered"` だけ。 */
  source: TriggerSource;
}

/**
 * 登録しようとしているトリガーの下見 (#58)。**同意画面に出す内容そのもの。**
 *
 * `requiredSecrets` / `allowedHosts` は core が実際に強制している宣言なので (#56 / #57)、
 * ここに出た以上のことはそのトリガーにはできない。
 */
export interface TriggerCandidate {
  /** 選ばれたフォルダの絶対パス。`registerTrigger()` にそのまま渡す。 */
  path: string;
  id: string;
  name: string;
  description: string | null;
  schedule: string;
  tz: string | null;
  requiredSecrets: string[];
  allowedHosts: string[];
  /**
   * 同じ id が既にある場合の相手。`"bundled"` は登録できない (同梱トリガーは
   * 乗っ取らせない)、`"registered"` は置き換え = 配布物の更新になる。
   */
  conflict: TriggerSource | null;
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
   * worker は `.setup()` 内で動き出すため、`[config error]` や `[expanded]` は
   * この webview のリスナーが繋がる前に emit される。**起動時のイベントを見るには
   * これを読む必要がある。**
   */
  listActivity: (limit?: number) =>
    invoke<ActivityEvent[]>("list_activity", { limit }),

  listTasks: () => invoke<TaskListItem[]>("list_tasks"),
  /**
   * 予定を 1 件削除する。展開済み境界があるので、消したタスクが次の展開で
   * 復活することはない (#26 決定事項 3)。
   */
  deleteTask: (id: string) => invoke<void>("delete_task", { id }),

  /**
   * フォルダを選ばせて中身を下見する (#58)。**この時点では何も入らない。**
   *
   * ダイアログは core が Rust 側で開く (フロントに dialog プラグインを足さないため)。
   * キャンセルは `null`、トリガーとして読めないフォルダは reject。
   */
  pickTriggerFolder: () => invoke<TriggerCandidate | null>("pick_trigger_folder"),
  /**
   * 下見したフォルダを実際に取り込む。**反映は再起動から** (#58)。
   *
   * 同じ id の登録済みトリガーがあれば置き換わる。同梱トリガーと衝突する場合は失敗する。
   */
  registerTrigger: (path: string) =>
    invoke<TriggerCandidate>("register_trigger", { path }),
  /**
   * 登録されたトリガーを外す。**こちらは再起動を待たずに効く。**
   *
   * 積まれていた予定も同時に消える。同梱トリガーは外せない (停止はできる)。
   */
  unregisterTrigger: (id: string) => invoke<void>("unregister_trigger", { id }),
  /** アプリを再起動する。登録を反映させるための口 (#58)。 */
  restartApp: () => invoke<void>("restart_app"),
  /**
   * トリガーの書き方を skill として書き出す (#60)。フォルダを選ばせ、書いた場所を返す
   * (キャンセルは `null`)。本文を返す口はない — 理由は core の `save_trigger_skill`。
   */
  saveTriggerSkill: () => invoke<string | null>("save_trigger_skill"),

  listDeclaredSecrets: () => invoke<DeclaredSecretItem[]>("list_declared_secrets"),
  hasSecret: (name: string) => invoke<boolean>("has_secret", { name }),
  setSecret: (name: string, value: string) =>
    invoke<void>("set_secret", { name, value }),
  deleteSecret: (name: string) => invoke<void>("delete_secret", { name }),

  chatHistory: () => invoke<ChatMessage[]>("chat_history"),
  chatSend: (message: string) => invoke<ChatMessage>("chat_send", { message }),
  chatClear: () => invoke<void>("chat_clear"),
};
