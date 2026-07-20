import { useEffect, useRef, useState } from "react";
import { chamberlainApi, type ChatMessage } from "../api";

export function ChatPanel() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
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
    setSending(true);

    // Optimistic: 先に user メッセージを表示。Assistant の応答は send 完了後に届く。
    const userMsg: ChatMessage = { role: "user", content: text, ts: Date.now() };
    setMessages((prev) => [...prev, userMsg]);
    setInput("");

    try {
      const assistantMsg = await chamberlainApi.chatSend(text);
      setMessages((prev) => [...prev, assistantMsg]);
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
    } catch (e) {
      setError(String(e));
    }
  };

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
          Chamberlain と会話を始めましょう。API キーの設定が済んでいない場合は
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
