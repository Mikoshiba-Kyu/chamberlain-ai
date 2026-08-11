//! manifest の宣言を**実行時の権限**として強制する層 (#56 / #55)。
//!
//! 0.2.0 までの `requiredSecrets` は [`crate::list_declared_secrets`] が Settings UI に
//! 「そのキーが未設定です」を出すために集約するだけの表示用データで、実行時の強制力を
//! 持っていなかった。`requiredSecrets: []` と宣言したトリガーが
//! `chamberlain.getSecret("anthropic_api_key")` を呼べる状態である。
//!
//! 焼き込みだけの今は「トリガーの作者 = アプリの作者」なので信頼で閉じているが、#55 で
//! 実行時登録を開くと**宣言と実際の権限が乖離したまま他人のコードを受け入れる**ことに
//! なる。ここはその前提を先に潰しておくための層である。
//!
//! # 呼び出し元の識別を JS 側に名乗らせない
//!
//! op に trigger id を引数で渡すと自己申告になり、他人のコードに対する強制力が消える。
//! JS 実行は単一スレッド上で直列 ([`crate::worker`] モジュール doc) なので、Rust 側が
//! 「今どのトリガーを実行中か」を [`TriggerPermissions::current`] に 1 つ持てば足りる。
//! `TauriHost` が JS を動かす前後で [`TriggerPermissions::enter`] /
//! [`TriggerPermissions::leave`] を呼び、op はそれを読むだけで呼び出し元を知る。
//!
//! # 既定は拒否
//!
//! 実行文脈の外 (`current` が `None`) からの呼び出しは [`Denial::NoContext`] で拒否する。
//! トリガーが `tick()` の中で await しなかった promise が後から解決した場合など、
//! framework が意図していない経路はここに落ちる。
//!
//! **既知の限界**: 全トリガーが 1 つの isolate を共有しているため、トリガー A が投げっぱなしに
//! した promise がトリガー B の実行中に解決すると、B の権限で `getSecret` が通る。塞ぐには
//! トリガーごとの isolate 分離が要るが、#59 の判断と同じ理由で割に合わないため採らない。
//! 拒否も許可も観測面には残るので、事後に追える状態は保つ。

use std::collections::{BTreeMap, BTreeSet};

use crate::secrets::{store as secret_store, ANTHROPIC_API_KEY_NAME};

/// framework が所有する secret か。**名前の綴りではなく解決先で判定する。**
///
/// [`secret_store::get`] は keyring を引く前に `CHAMBERLAIN_SECRET_<UPPERCASE>` を見る。
/// その名前の正規化は多対 1 で、`ANTHROPIC_API_KEY` も `anthropic-api-key` も
/// `anthropic.api.key` も `anthropic_api_key` と同じ env var に落ちる。したがって
/// **綴りの一致で弾くと、別綴りで宣言したトリガーに framework のキーがそのまま渡る**
/// (`requiredSecrets: ["ANTHROPIC_API_KEY"]` が宣言として通り、store が値を返す)。
/// Windows の Credential Manager はターゲット名が大文字小文字を区別しないので、keyring
/// 経路でも同じことが起きうる。
fn is_framework_secret(name: &str) -> bool {
    secret_store::env_var_name(name) == secret_store::env_var_name(ANTHROPIC_API_KEY_NAME)
}

/// 1 トリガーが manifest で宣言した権限。
///
/// 今は `requiredSecrets` だけ。#57 で `http.fetch` の宛先ホスト宣言がここに増える。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TriggerGrants {
    /// manifest の `requiredSecrets`。
    pub secrets: BTreeSet<String>,
}

impl TriggerGrants {
    pub(crate) fn with_secrets(secrets: impl IntoIterator<Item = String>) -> Self {
        Self {
            secrets: secrets.into_iter().collect(),
        }
    }
}

/// 権限チェックが落ちた理由。
///
/// 「宣言し忘れ」([`Denial::Undeclared`]) と「framework の持ち物への接近」
/// ([`Denial::FrameworkOwned`]) を同じ見た目にしないために分けてある。前者はエージェント
/// 開発者のデバッグ対象、後者は設計上通らない経路である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Denial {
    /// トリガーの実行文脈の外から呼ばれた。
    NoContext,
    /// framework が所有する secret。宣言の有無に関わらず渡さない。
    FrameworkOwned,
    /// manifest の `requiredSecrets` に無い名前。
    Undeclared,
}

impl Denial {
    /// `getSecret` の拒否として観測面に出す本文。`[denied]` プレフィックスは
    /// [`crate::history::ActivityKind`] が付ける。
    ///
    /// 理由 ([`Denial`]) は capability に依らないが**本文は依る**ので、名前で縛っておく。
    /// #57 が宛先ホストの拒否を足すときは、同じ [`Denial`] に対する別の本文になる。
    fn message_for_secret(&self, name: &str) -> String {
        match self {
            Self::NoContext => format!(
                "getSecret('{name}') was called outside of a trigger execution; \
                 the caller could not be identified"
            ),
            Self::FrameworkOwned => format!(
                "secret '{name}' belongs to the framework and is never handed to triggers; \
                 use chamberlain.ai.complete instead"
            ),
            Self::Undeclared => {
                format!("secret '{name}' is not declared in the manifest's requiredSecrets")
            }
        }
    }
}

/// `getSecret(name)` を通してよいかの判断。副作用を持たない。
///
/// `grants` が `None` は「実行文脈の外」を意味する (トリガーが宣言を持たない場合は
/// 空の [`TriggerGrants`] が渡る)。
///
/// framework の `anthropic_api_key` は**宣言されていても拒否する**。Type I が AI を使うなら
/// `chamberlain.ai.complete` を経由すべきで、キーの生値をトリガーに渡す理由が無い
/// (生値を渡せる穴が残っていると、#57 で `ai.complete` の呼び出しを履歴に残す意味も消える)。
/// 判定は綴りではなく解決先で行う ([`is_framework_secret`])。
///
/// 一方 `grants` 側の突き合わせは**綴りの完全一致**のままにしてある。正規化すると
/// 「宣言と違う綴りでも通る」方向に緩むが、完全一致なら綴り違いは拒否 (安全側) に倒れる。
pub(crate) fn decide_secret(grants: Option<&TriggerGrants>, name: &str) -> Result<(), Denial> {
    if is_framework_secret(name) {
        return Err(Denial::FrameworkOwned);
    }
    match grants {
        None => Err(Denial::NoContext),
        Some(g) if g.secrets.contains(name) => Ok(()),
        Some(_) => Err(Denial::Undeclared),
    }
}

/// 拒否 1 件。`TauriHost` が JS 実行の後に回収して観測面に流す。
///
/// op から直接 activity を書かないのは、op が `AppHandle` と履歴ハンドルを持たないため。
/// 溜めて回収する形にすると、`OpState` に置くものが manifest 由来の宣言だけで済む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DenialRecord {
    /// 帰属先のトリガー。実行文脈の外なら `None`。
    pub trigger_id: Option<String>,
    pub message: String,
}

/// `OpState` に置く権限の状態。op から見える唯一の判断材料。
///
/// `Default` (宣言ゼロ・実行文脈なし) は**全部拒否**になる。extension が起動時にこれを
/// 載せておき、worker が manifest 由来の宣言で差し替える。こうしておくと「配線し忘れ」が
/// 素通りではなく拒否 + 記録として現れる。
#[derive(Debug, Default)]
pub(crate) struct TriggerPermissions {
    /// trigger id → manifest 由来の宣言。起動時に 1 回作られる。
    grants: BTreeMap<String, TriggerGrants>,
    /// 今 JS を動かしているトリガー。`None` は実行文脈の外。
    current: Option<String>,
    /// 未回収の拒否。
    denials: Vec<DenialRecord>,
}

impl TriggerPermissions {
    pub(crate) fn new(grants: BTreeMap<String, TriggerGrants>) -> Self {
        Self {
            grants,
            ..Default::default()
        }
    }

    /// JS を動かす直前に呼ぶ。以後の op はこのトリガーの宣言で判定される。
    pub(crate) fn enter(&mut self, trigger_id: &str) {
        self.current = Some(trigger_id.to_string());
    }

    /// JS の実行が終わったら呼ぶ。実行文脈を閉じ、その間に溜まった拒否を返す
    /// (以後の op は [`Denial::NoContext`] で落ちる)。
    ///
    /// 回収を別メソッドに分けない。分けると「閉じたが回収していない」状態が作れて、
    /// 溜まった拒否が次の実行の持ち物として報告されうる。
    pub(crate) fn leave(&mut self) -> Vec<DenialRecord> {
        self.current = None;
        std::mem::take(&mut self.denials)
    }

    /// `getSecret(name)` を通してよいか。拒否した場合は記録に残して `false` を返す。
    pub(crate) fn authorize_secret(&mut self, name: &str) -> bool {
        // 宣言を持たないトリガー (discovery に載っていない等) は「何も許可されていない」に倒す。
        let no_grants = TriggerGrants::default();
        let grants = self
            .current
            .as_deref()
            .map(|id| self.grants.get(id).unwrap_or(&no_grants));
        match decide_secret(grants, name) {
            Ok(()) => true,
            Err(denial) => {
                self.record(DenialRecord {
                    trigger_id: self.current.clone(),
                    message: denial.message_for_secret(name),
                });
                false
            }
        }
    }

    /// 拒否を 1 件記録する。**同じ実行の中で同じ拒否は 1 回しか残さない。**
    ///
    /// 観測面に出したいのは「どの宣言を踏み外したか」であって回数ではない。1 回の tick に
    /// N 回の拒否を素通しすると、N 行の history 追記と N 回の UI emit になる。retention は
    /// 20,000 行 (`MAX_ROWS`) なので、ループの中で `getSecret` を呼ぶトリガー 1 つが
    /// **本物の `[notify]` / `[error]` を履歴から押し出せてしまう**。宣言し忘れの検出手段が
    /// `[denied]` である以上、その経路自身が観測面を潰せる状態は許容できない。
    ///
    /// 重複判定後の要素数は「踏み外した宣言の種類数」で頭打ちになるので、線形走査で足りる。
    fn record(&mut self, denial: DenialRecord) {
        if !self.denials.contains(&denial) {
            self.denials.push(denial);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(names: &[&str]) -> TriggerGrants {
        TriggerGrants::with_secrets(names.iter().map(|s| s.to_string()))
    }

    fn state() -> TriggerPermissions {
        TriggerPermissions::new(BTreeMap::from([
            ("declaring".to_string(), grants(&["github_token"])),
            ("silent".to_string(), grants(&[])),
        ]))
    }

    #[test]
    fn declared_secret_is_allowed() {
        assert_eq!(
            decide_secret(Some(&grants(&["github_token"])), "github_token"),
            Ok(())
        );
    }

    #[test]
    fn undeclared_secret_is_denied() {
        assert_eq!(
            decide_secret(Some(&grants(&["github_token"])), "slack_token"),
            Err(Denial::Undeclared)
        );
    }

    #[test]
    fn trigger_without_declaration_gets_nothing() {
        assert_eq!(
            decide_secret(Some(&grants(&[])), "github_token"),
            Err(Denial::Undeclared)
        );
    }

    /// framework の持ち物は宣言を書いても渡らない (#56 の論点)。
    #[test]
    fn framework_secret_is_denied_even_when_declared() {
        assert_eq!(
            decide_secret(
                Some(&grants(&[ANTHROPIC_API_KEY_NAME])),
                ANTHROPIC_API_KEY_NAME
            ),
            Err(Denial::FrameworkOwned)
        );
    }

    /// framework のキーは**綴りを変えても**渡らない。
    ///
    /// `store::get` の env fallback は名前を正規化するので、下の綴りはどれも
    /// `CHAMBERLAIN_SECRET_ANTHROPIC_API_KEY` に解決する = 同じキーが返る。綴りの完全一致で
    /// 弾いていると、宣言に別綴りを書くだけで framework のキーを持ち出せてしまう。
    #[test]
    fn framework_secret_is_denied_under_alias_spellings() {
        for alias in [
            "ANTHROPIC_API_KEY",
            "anthropic-api-key",
            "Anthropic.Api.Key",
            "anthropic api key",
        ] {
            assert_eq!(
                decide_secret(Some(&grants(&[alias])), alias),
                Err(Denial::FrameworkOwned),
                "'{alias}' が framework のキーとして弾かれていない"
            );
        }
    }

    /// 実行文脈の外は既定で拒否。投げっぱなしの promise が後から解決した場合等。
    #[test]
    fn outside_of_a_trigger_everything_is_denied() {
        assert_eq!(decide_secret(None, "github_token"), Err(Denial::NoContext));
    }

    #[test]
    fn enter_scopes_the_decision_to_that_trigger() {
        let mut s = state();
        s.enter("declaring");
        assert!(s.authorize_secret("github_token"));
        assert!(s.leave().is_empty());

        s.enter("silent");
        assert!(!s.authorize_secret("github_token"));
    }

    #[test]
    fn leave_closes_the_context() {
        let mut s = state();
        s.enter("declaring");
        assert!(s.authorize_secret("github_token"));
        s.leave();
        assert!(!s.authorize_secret("github_token"));
    }

    /// 拒否は「誰が何を要求したか」が分かる形で残る (完了条件: 拒否が観測面に残る)。
    #[test]
    fn denials_are_recorded_with_their_trigger_and_drained_once() {
        let mut s = state();
        s.enter("silent");
        assert!(!s.authorize_secret("github_token"));

        let denials = s.leave();
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0].trigger_id.as_deref(), Some("silent"));
        assert!(denials[0].message.contains("github_token"));
        assert!(denials[0].message.contains("requiredSecrets"));

        s.enter("silent");
        assert!(
            s.leave().is_empty(),
            "回収済みの拒否は次の実行に持ち越さない"
        );
    }

    /// 同じ拒否を繰り返しても 1 行しか残さない。ループの中で `getSecret` を呼ぶトリガーが
    /// 履歴を埋め尽くして本物のイベントを押し流すのを防ぐ。
    #[test]
    fn repeating_the_same_denial_records_it_once() {
        let mut s = state();
        s.enter("silent");
        for _ in 0..1_000 {
            assert!(!s.authorize_secret("github_token"));
        }
        assert!(!s.authorize_secret("slack_token"));

        assert_eq!(s.leave().len(), 2, "踏み外した宣言の種類ぶんだけ残る");
    }

    /// 宣言の無いトリガーが grants に載っていなくても素通りさせない。
    #[test]
    fn unknown_trigger_id_is_treated_as_having_no_grants() {
        let mut s = state();
        s.enter("never-discovered");
        assert!(!s.authorize_secret("github_token"));
    }
}
