import { useEffect, useRef, useState } from "react";
import {
  chamberlainApi,
  type ChatMessage,
  type TriggerCandidate,
} from "../api";
import { ConsentCard, RestartNotice } from "./ConsentCard";
import { useBusyAction } from "./useBusyAction";

interface Props {
  /**
   * 登録済みだがまだ反映されていないものがあるか (#58)。
   *
   * TriggersPanel と同じく **App が持つ**。タブを切り替えるとパネルは unmount される
   * ので、ここに置くと「入れたのに一覧に出ず、案内も消えた」状態が作れてしまう。
   */
  restartPending: boolean;
  onRegistered: () => void;
}

export function ChatPanel({ restartPending, onRegistered }: Props) {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [loading, setLoading] = useState(true);
  // 秘書が用意したトリガーの下書き (#61)。null の間は登録の口が閉じている。
  const [draft, setDraft] = useState<TriggerCandidate | null>(null);
  // 送信も登録も「二重に押させない・失敗を画面に出す」は同じ扱いなので共有する。
  const { busy, error, notice, setError, setNotice, run } = useBusyAction();
  const logRef = useRef<HTMLDivElement>(null);

  const load = async () => {
    setLoading(true);
    try {
      const history = await chamberlainApi.chatHistory();
      setMessages(history);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  useEffect(() => {
    logRef.current?.scrollTo({ top: logRef.current.scrollHeight });
  }, [messages]);

  const onSend = async () => {
    const text = input.trim();
    if (!text || sending) return;
    setError(null);
    setNotice(null);
    setSending(true);

    // Optimistic: 先に user メッセージを表示。Assistant の応答は send 完了後に届く。
    const userMsg: ChatMessage = { role: "user", content: text, ts: Date.now() };
    setMessages((prev) => [...prev, userMsg]);
    setInput("");

    try {
      const turn = await chamberlainApi.chatSend(text);
      setMessages((prev) => [...prev, turn.message]);
      // 下書きが付いてくるかは秘書が決める。付いてこなければ今のカードはそのまま
      // (会話の途中で別のことを話しても、確認待ちのものが黙って消えない)。
      if (turn.draft) setDraft(turn.draft);
    } catch (e) {
      setError(String(e));
      // 失敗時は user メッセージを取り消す (履歴側にも保存されていないので UI から抜くだけ)
      setMessages((prev) => prev.slice(0, -1));
      setInput(text);
    } finally {
      setSending(false);
    }
  };

  const onClear = async () => {
    if (!confirm("会話履歴を全部消しますか?")) return;
    try {
      await chamberlainApi.chatClear();
      setMessages([]);
      // 会話ごと消した以上、その会話で出た下書きも残さない。残すと、元になった
      // やりとりが画面から消えたまま確認カードだけが宙に浮く。
      if (draft) {
        const { id } = draft;
        setDraft(null);
        await chamberlainApi.discardTriggerDraft(id);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const confirmRegister = () =>
    run(async () => {
      if (!draft) return;
      // 経路は #58 と同じ。core は UI の言うことを信じず検証をやり直す。
      const registered = await chamberlainApi.registerTrigger(draft.path);
      setDraft(null);
      onRegistered();
      setNotice(`${registered.name} (${registered.id}) を登録しました。`);
    });

  const cancelDraft = () =>
    run(async () => {
      if (!draft) return;
      const { id } = draft;
      setDraft(null);
      // 何も登録されていないので取り消しではない。下書きのファイルを片付けるだけ。
      await chamberlainApi.discardTriggerDraft(id);
    });

  const onKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    // Cmd/Ctrl+Enter で送信
    if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
      e.preventDefault();
      onSend();
    }
  };

  return (
    <section className="panel chat-panel">
      <div className="chat-head">
        <h1>チャット</h1>
        {messages.length > 0 && (
          <button className="chat-clear" onClick={onClear}>
            履歴クリア
          </button>
        )}
      </div>

      {loading ? (
        <p className="placeholder">読み込み中…</p>
      ) : messages.length === 0 ? (
        <p className="placeholder">
          Chamberlain と会話を始めましょう。「毎朝 9 時に〜を教えて」のように頼むと、
          そのためのトリガーを用意します。API キーの設定が済んでいない場合は
          「設定」タブで anthropic_api_key を入力してください。
        </p>
      ) : (
        <div className="chat-log" ref={logRef}>
          {messages.map((m, i) => (
            <div key={i} className={`chat-msg chat-msg-${m.role}`}>
              <div className="chat-msg-role">
                {m.role === "user" ? "あなた" : "Chamberlain"}
              </div>
              <div className="chat-msg-body">{m.content}</div>
            </div>
          ))}
        </div>
      )}

      {error && <div className="chat-error">エラー: {error}</div>}
      {notice && <p className="notice">{notice}</p>}
      {restartPending && <RestartNotice />}

      {draft && (
        <ConsentCard
          candidate={draft}
          heading="秘書がこのトリガーを用意しました。登録しますか？"
          busy={busy}
          onConfirm={confirmRegister}
          onCancel={cancelDraft}
        />
      )}

      <div className="chat-input">
        <textarea
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="メッセージを入力… (Cmd/Ctrl+Enter で送信)"
          disabled={sending}
          rows={3}
        />
        <button onClick={onSend} disabled={sending || !input.trim()}>
          {sending ? "送信中…" : "送信"}
        </button>
      </div>
    </section>
  );
}
