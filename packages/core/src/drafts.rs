//! 秘書 (Type II) が生成したトリガーの下書き (#61 / #55)。
//!
//! #55 の供給元 (c)。エンドユーザーがチャットで「毎朝 9 時に天気を教えて」と頼むと、
//! 秘書がトリガーパッケージ (`manifest.json` + `index.ts`) を書き、下書きとして置く。
//!
//! ```text
//! [頼む] chat_send ──▶ [申し出る] propose_trigger ──▶ [書く] 下書き ──▶ [見せる] 同意画面 ──▶ [入れる] register_trigger
//!         秘書が判断する      道具を 1 つだけ持たせる      <app_data>/trigger-drafts/    #58 と同じもの      #58 と同じ経路
//! ```
//!
//! # なぜ下書きをファイルに書くのか
//!
//! 生成物をメモリに持ったまま専用の登録経路を通すと、**#58 の検証を二重に持つ**ことに
//! なる。ここでフォルダに書き出してしまえば、そこから先 (下見 → 同意 → コピー) は
//! 「フォルダから追加…」と 1 バイトも変わらない。秘書が書いたという理由で緩む経路が
//! 生まれない — **書いたのが AI なら誰も中身を読んでいない**以上、機構側の制約だけが
//! 安全の担保になる (#56 / #57)。
//!
//! # 生成は 2 回目の呼び出しに分ける
//!
//! 秘書が持つ道具は「頼まれた内容」を渡すだけで、トリガーの中身は書かせない。仕様書
//! (#60) は 15 KB あり、会話のたびに system prompt へ載せるのは無駄が大きい。道具が
//! 呼ばれてから、仕様書を system prompt にした 2 回目の呼び出しで生成する。

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::history::{Activity, ActivityKind};
use crate::{ai, HistoryRef, TriggerCandidate, TriggerInfo};

/// 下書きの置き場 (`<app_data>/trigger-drafts/`)。
///
/// **`<app_data>/triggers/` の下には置かない。** discovery の走査先なので、同意を取る
/// 前の生成物がそこにあると、次の起動で勝手に動き出す。
const DRAFTS_DIR_NAME: &str = "trigger-drafts";

/// 秘書に持たせる道具の名前。
pub(crate) const PROPOSE_TRIGGER_TOOL: &str = "propose_trigger";

/// 生成物のエントリファイル名。仕様書 (#60) が `"index.ts"` に固定している。
const ENTRY_FILE: &str = "index.ts";

/// 生成に使うモデル。会話用と分けていない (分ける理由が出るまでは既定のまま)。
const GENERATION_MODEL: Option<&str> = None;

/// 下書きの置き場。[`crate::RegisteredDir`] と同じく**起動時に 1 回だけ解決する**。
///
/// 解決に失敗した環境では生成だけが使えなくなる (チャットも焼き込みトリガーも動く)。
pub(crate) struct DraftDir(Option<PathBuf>);

impl DraftDir {
    /// 解決してディレクトリを作り、**前回の残骸を捨てる**。
    ///
    /// 下書きは「同意画面に出ている間だけ意味のあるもの」で、チャットのカードが消えれば
    /// 二度と参照されない。再起動をまたいで残すと、消える機会が無いまま溜まり続ける。
    pub(crate) fn resolve(app: &AppHandle) -> Self {
        let Ok(base) = app.path().app_data_dir() else {
            eprintln!("failed to resolve app data dir for trigger drafts");
            return Self(None);
        };
        let dir = base.join(DRAFTS_DIR_NAME);
        let _ = std::fs::remove_dir_all(&dir);
        match std::fs::create_dir_all(&dir) {
            Ok(()) => Self(Some(dir)),
            Err(e) => {
                eprintln!("failed to create {}: {e}", dir.display());
                Self(None)
            }
        }
    }

    pub(crate) fn get(&self) -> Result<&Path, String> {
        self.0
            .as_deref()
            .ok_or_else(|| "下書きの置き場を用意できませんでした".to_string())
    }

    /// 与えられたパスがこの置き場の中か。**観測面の文言を分けるためだけに使う** —
    /// 判断 (何を許すか) には使わない。
    pub(crate) fn contains(&self, path: &Path) -> bool {
        self.0.as_deref().is_some_and(|dir| path.starts_with(dir))
    }
}

/// 下書きを置いて下見するのに要る「周り」。
///
/// 3 つとも Tauri の State から取り出したもので、**必ず一緒に使う**。個別に手渡すと
/// 呼び出し側の引数が増えるだけで、増えた引数のどれかを間違えても意味は通ってしまう。
pub(crate) struct DraftSite<'a> {
    /// 下書きの置き場。解決できていない環境では `None` ([`DraftDir::get`] と同じ理由を
    /// 呼び出し側に判断させない — 生成を断る文言はここが持つ)。
    pub root: Option<&'a Path>,
    /// 起動時から居るトリガー。id の衝突判定に使う。
    pub triggers: &'a [TriggerInfo],
    /// 登録済みの置き場。衝突判定はディスク側も見る (登録直後・再起動前のもの)。
    pub registered_dir: Option<&'a Path>,
}

/// 道具が呼ばれたときの一切を引き受ける (#61)。
///
/// **返した時点でもまだ何も登録されていない。** 引数の解釈 → 生成 → 下書きを書く →
/// #58 の下見、で終わる。道具の引数の形 ([`propose_trigger_tool`]) をこのモジュールが
/// 決めている以上、それを読むのもここの仕事で、chat 側はキー名を知らない。
///
/// 下見 ([`crate::inspect_candidate`]) をここで通すのは、同意画面に出す内容を
/// 「フォルダから追加…」と完全に同じにするため。schedule の書式や `allowedHosts` の
/// 書き方が壊れていれば、ここで弾かれて秘書の口から理由が返る。
pub(crate) async fn propose(
    app: &AppHandle,
    history: &HistoryRef,
    api_key: &str,
    input: &serde_json::Value,
    site: DraftSite<'_>,
) -> Result<TriggerCandidate, String> {
    let root = site.root.ok_or("下書きの置き場を用意できませんでした")?;
    let request = input
        .get("request")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if request.trim().is_empty() {
        return Err("何を繰り返すのかを読み取れませんでした".to_string());
    }

    let generated = generate(api_key, request).await?;
    let path = write(root, &generated)?;

    // **AI がコードを書いた瞬間を残す。** 登録されなかった下書きも含めて後から追える
    // 必要がある — 同意画面を通ったものしか記録が無いと、「秘書が何を書こうとしたか」が
    // 観測面から消える (#61 の「誰も中身を読んでいない」前提)。
    //
    // 残すのは manifest の宣言だけで、**依頼の文言は書かない**。`[ai]` が prompt を
    // 残さないのと同じ理由で、履歴はエンドユーザーの手元に平文で溜まる。会話は
    // `chat-history.json` にあり、消す手段 (履歴クリア) もそちらにある。
    let record = |message: String| {
        crate::record_activity(
            app,
            history,
            &Activity::new(&generated.id, ActivityKind::Drafted, message),
        );
    };

    // 下見で断られた下書きは同意画面に出ないので、捨てる口 ([`discard`]) も呼ばれない。
    // ここで片付けないと、次の起動まで「見せてもいない生成物」がディスクに残る。
    //
    // **記録は残す。** 秘書が壊れたトリガーを書いたことこそ一番観測したい失敗で、
    // ここで黙ると「誰も中身を読んでいない」前提のまま痕跡だけが消える。
    let candidate = match crate::inspect_candidate(site.triggers, site.registered_dir, &path) {
        Ok(candidate) => candidate,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&path);
            record(format!("秘書が書いた下書きは使えませんでした: {e}"));
            return Err(e);
        }
    };

    record(format!(
        "秘書がトリガーの下書きを作りました: {} ({})",
        candidate.name, candidate.schedule
    ));
    Ok(candidate)
}

/// 秘書に渡す道具の定義。
///
/// **引数は「何を頼まれたか」1 つだけ。** schedule や manifest の形を秘書に組み立て
/// させると、仕様書を読んでいない側が仕様を書くことになる。生成は仕様書を読んだ
/// 2 回目の呼び出しの仕事。
pub(crate) fn propose_trigger_tool() -> ai::Tool<'static> {
    ai::Tool {
        name: PROPOSE_TRIGGER_TOOL,
        description:
            "ユーザーが「決まったタイミングで繰り返しやってほしいこと」を頼んだときに呼ぶ。\
            この道具はトリガー (定期実行される小さなプログラム) の下書きを作り、\
            ユーザーに確認画面を出す。登録するかどうかはユーザーが決めるので、\
            あなたが許可を求める必要はない。\
            会話や質問への回答で済むこと、今すぐ 1 回やるだけのことには使わない。",
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "string",
                    "description": "頼まれた内容を、いつ・何を確認して・何を知らせるかが分かる形で 1〜3 文にまとめたもの。\
                        会話の文脈で補った条件 (時刻、対象、通知の内容) も含める。日本語で書く。"
                }
            },
            "required": ["request"]
        }),
    }
}

/// 生成 AI に渡す出力形式の指示。仕様書 (#60) の後ろに継ぎ足す。
///
/// 仕様書は「フォルダとファイルを作れ」と書いてあるが、ここではファイルを作るのは
/// core の仕事なので、その一点だけ上書きする。
const OUTPUT_CONTRACT: &str = "\
---

# 出力形式 (この呼び出しに限る)

上の仕様書は「フォルダを作る」「2 つのファイルの中身を示す」と書いていますが、\
**この呼び出しでは JSON オブジェクトを 1 つだけ返してください。** ファイルは呼び出し側が作ります。

```json
{\"manifest\": { ... manifest.json の中身 ... }, \"indexTs\": \"... index.ts の全文 ...\"}
```

- 前置きも説明もコードフェンスも付けず、JSON だけを返してください。
- `indexTs` は index.ts の全文を 1 つの文字列にしたものです (改行は \\n)。
- `manifest` の `id` は ASCII 英数と `-` `_` だけで、内容が分かる名前にしてください。
- 上の §8 チェックリストを自分で確認してから返してください。
- 情報が足りない部分は、ユーザーに聞き返すのではなく**妥当な既定値**で埋めてください \
(このトリガーは確認画面を経てから登録され、後から作り直せます)。
- 通信が要るなら `allowedHosts` に、鍵が要るなら `requiredSecrets` に必ず宣言してください。\
宣言していないものは実行時に拒否されます。";

/// 生成された 2 ファイル。
#[derive(Debug)]
pub(crate) struct GeneratedTrigger {
    pub id: String,
    pub manifest: serde_json::Value,
    pub index_ts: String,
}

#[derive(Deserialize)]
struct GeneratedPayload {
    manifest: serde_json::Value,
    #[serde(rename = "indexTs")]
    index_ts: String,
}

/// 仕様書を system prompt にして 1 回だけ生成させる。
async fn generate(api_key: &str, request: &str) -> Result<GeneratedTrigger, String> {
    let system = format!("{}\n\n{OUTPUT_CONTRACT}", crate::TRIGGER_SPEC);
    let messages = [ai::Message {
        role: ai::Role::User,
        content: format!("次の依頼に応えるトリガーを 1 つ作ってください。\n\n{request}"),
    }];
    let raw = ai::complete(api_key, GENERATION_MODEL, Some(&system), &messages).await?;
    parse_generated(&raw)
}

/// モデルの応答から 2 ファイルを取り出す。
///
/// **前置きとコードフェンスを許す。** 出力形式は指示してあるが、そこを外したときに
/// 「生成できませんでした」で終わるより、最初の `{` から最後の `}` までを読む方が実用的。
///
/// 応答が `max_tokens` (4096) で切れた場合もここで JSON パースに失敗する。**切り捨てを
/// 切り捨てと言えない** (`stop_reason` を見ていない) ので、エンドユーザーには読めない
/// 文言が出る。#68 で扱う。
fn parse_generated(raw: &str) -> Result<GeneratedTrigger, String> {
    let start = raw
        .find('{')
        .ok_or_else(|| "生成結果に JSON が含まれていません".to_string())?;
    let end = raw
        .rfind('}')
        .ok_or_else(|| "生成結果の JSON が閉じていません".to_string())?;
    if end < start {
        return Err("生成結果の JSON が閉じていません".to_string());
    }

    let payload: GeneratedPayload = serde_json::from_str(&raw[start..=end])
        .map_err(|e| format!("生成結果を読めませんでした: {e}"))?;

    // id は下書きのディレクトリ名になる。ここで取れないと置き場所が決まらないので、
    // manifest の他の不備 (schedule の書式など) より先に見る。
    let id = payload
        .manifest
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "生成された manifest に id がありません".to_string())?
        .to_string();
    crate::registry::validate_trigger_id(&id)?;

    if payload.index_ts.trim().is_empty() {
        return Err("生成された index.ts が空です".to_string());
    }

    // entry は書き手ではなく**書き出す側**が決める。仕様書が `"index.ts"` に固定して
    // いる項目なので、モデルが別の名前を宣言してきたときに従うと、宣言と実体が食い違う
    // 下書き (同意画面まで通ってから load error) ができる。
    let mut manifest = payload.manifest;
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("entry".to_string(), serde_json::json!(ENTRY_FILE));
    }

    Ok(GeneratedTrigger {
        id,
        manifest,
        index_ts: payload.index_ts,
    })
}

/// 下書きを `<app_data>/trigger-drafts/<id>/` に書き、そのパスを返す。
///
/// 同じ id の下書きは作り直し。「やっぱり 8 時にして」は差分ではなく**丸ごと再生成**
/// なので、前回の残りが混ざらないよう先に消す。
fn write(root: &Path, generated: &GeneratedTrigger) -> Result<PathBuf, String> {
    let path = root.join(&generated.id);
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).map_err(|e| format!("{} を作れません: {e}", path.display()))?;

    let manifest = serde_json::to_string_pretty(&generated.manifest)
        .map_err(|e| format!("manifest を書けません: {e}"))?;
    std::fs::write(path.join(crate::MANIFEST_FILE), manifest)
        .map_err(|e| format!("manifest を書けません: {e}"))?;
    std::fs::write(path.join(ENTRY_FILE), &generated.index_ts)
        .map_err(|e| format!("index.ts を書けません: {e}"))?;
    Ok(path)
}

/// 下書きを捨てる (同意画面で「やめる」を押されたとき)。
///
/// 残っていても次の起動で消えるが、**捨てたつもりのものがディスクに残る**状態を
/// 作らない。実体が無いこと自体はエラーにしない。
pub(crate) fn discard(root: &Path, id: &str) -> Result<(), String> {
    crate::registry::validate_trigger_id(id)?;
    let path = root.join(id);
    if !path.is_dir() {
        return Ok(());
    }
    std::fs::remove_dir_all(&path).map_err(|e| format!("{} を削除できません: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 指示どおりの応答。
    #[test]
    fn parses_a_bare_json_object() {
        let generated = parse_generated(
            r#"{"manifest":{"id":"weather","name":"天気","entry":"index.ts","schedule":"@daily 09:00"},"indexTs":"export function tick() {}"}"#,
        )
        .unwrap();
        assert_eq!(generated.id, "weather");
        assert_eq!(generated.index_ts, "export function tick() {}");
    }

    /// 前置きとコードフェンスが付いてきても読む。ここで諦めると、生成のたびに
    /// 「作れませんでした」がユーザーに返る。
    #[test]
    fn tolerates_a_preamble_and_code_fences() {
        let generated = parse_generated(
            "承知しました。\n```json\n{\"manifest\":{\"id\":\"a\"},\"indexTs\":\"export function tick() {}\"}\n```\n以上です。",
        )
        .unwrap();
        assert_eq!(generated.id, "a");
    }

    /// id は下書きのディレクトリ名になる。生成 AI の出力をそのままパスに使わない。
    #[test]
    fn rejects_ids_that_escape_the_draft_directory() {
        let err = parse_generated(
            r#"{"manifest":{"id":"../escape"},"indexTs":"export function tick() {}"}"#,
        )
        .unwrap_err();
        assert!(err.contains("トリガー ID"), "{err}");
    }

    /// entry は書き出す側が決める。宣言と実体が食い違う下書きを作らせない。
    #[test]
    fn entry_is_forced_to_the_file_we_write() {
        let generated = parse_generated(
            r#"{"manifest":{"id":"a","entry":"src/main.ts"},"indexTs":"export function tick() {}"}"#,
        )
        .unwrap();
        assert_eq!(generated.manifest["entry"], "index.ts");
    }

    #[test]
    fn rejects_incomplete_payloads() {
        for raw in [
            "作れませんでした",
            r#"{"manifest":{"name":"id がない"},"indexTs":"x"}"#,
            r#"{"manifest":{"id":"a"},"indexTs":"   "}"#,
            r#"{"manifest":{"id":"a"}}"#,
        ] {
            assert!(parse_generated(raw).is_err(), "{raw:?} は弾くはず");
        }
    }
}
