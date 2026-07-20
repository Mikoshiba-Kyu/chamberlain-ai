import { useEffect, useState } from "react";
import { chamberlainApi, type DeclaredSecretItem } from "../api";

interface SecretRow extends DeclaredSecretItem {
  isSet: boolean;
  draft: string;
  saving: boolean;
  error: string | null;
}

export function SettingsPanel() {
  const [rows, setRows] = useState<SecretRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setLoadError(null);
    try {
      const declared = await chamberlainApi.listDeclaredSecrets();
      const enriched = await Promise.all(
        declared.map(async (d): Promise<SecretRow> => {
          const isSet = await chamberlainApi.hasSecret(d.name).catch(() => false);
          return { ...d, isSet, draft: "", saving: false, error: null };
        }),
      );
      setRows(enriched);
    } catch (e) {
      setLoadError(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, []);

  const updateRow = (name: string, patch: Partial<SecretRow>) => {
    setRows((prev) =>
      prev.map((r) => (r.name === name ? { ...r, ...patch } : r)),
    );
  };

  const onSave = async (name: string) => {
    const row = rows.find((r) => r.name === name);
    if (!row || !row.draft) return;
    updateRow(name, { saving: true, error: null });
    try {
      await chamberlainApi.setSecret(name, row.draft);
      updateRow(name, { isSet: true, draft: "", saving: false });
    } catch (e) {
      updateRow(name, { saving: false, error: String(e) });
    }
  };

  const onDelete = async (name: string) => {
    updateRow(name, { saving: true, error: null });
    try {
      await chamberlainApi.deleteSecret(name);
      updateRow(name, { isSet: false, saving: false });
    } catch (e) {
      updateRow(name, { saving: false, error: String(e) });
    }
  };

  return (
    <section className="panel">
      <h1>設定</h1>
      <h2>シークレット</h2>
      <p className="placeholder">
        トリガーが要求している API キー / トークンをここで設定します。値は OS の
        credential manager (Windows Credential Manager / macOS Keychain / Linux
        Secret Service) に保存され、Chamberlain 上には残りません。
      </p>

      {loading && <p className="placeholder">読み込み中…</p>}
      {loadError && <p className="error">読み込み失敗: {loadError}</p>}
      {!loading && !loadError && rows.length === 0 && (
        <p className="placeholder">
          現在、シークレットを要求しているトリガーはありません。
        </p>
      )}

      <ul className="secrets">
        {rows.map((row) => (
          <li key={row.name} className="secret-row">
            <div className="secret-head">
              <span className="secret-name">{row.name}</span>
              <span
                className={row.isSet ? "secret-status set" : "secret-status unset"}
              >
                {row.isSet ? "設定済み" : "未設定"}
              </span>
            </div>
            <div className="secret-requiredby">
              要求元: {row.requiredBy.join(", ")}
            </div>
            <div className="secret-controls">
              <input
                type="password"
                value={row.draft}
                placeholder={row.isSet ? "新しい値を入力して更新" : "値を入力"}
                onChange={(e) => updateRow(row.name, { draft: e.target.value })}
                disabled={row.saving}
              />
              <button
                onClick={() => onSave(row.name)}
                disabled={row.saving || !row.draft}
              >
                {row.isSet ? "更新" : "保存"}
              </button>
              {row.isSet && (
                <button
                  onClick={() => onDelete(row.name)}
                  disabled={row.saving}
                  className="danger"
                >
                  削除
                </button>
              )}
            </div>
            {row.error && <div className="secret-error">エラー: {row.error}</div>}
          </li>
        ))}
      </ul>
    </section>
  );
}
