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
//! 実行文脈の外 (`current` が `None`) からの呼び出しは `NoContext` で拒否する。
//! トリガーが `tick()` の中で await しなかった promise が後から解決した場合など、
//! framework が意図していない経路はここに落ちる。
//!
//! **既知の限界**: 全トリガーが 1 つの isolate を共有しているため、トリガー A が投げっぱなしに
//! した promise がトリガー B の実行中に解決すると、B の権限で `getSecret` が通る。塞ぐには
//! トリガーごとの isolate 分離が要るが、#59 の判断と同じ理由で割に合わないため採らない。
//! 拒否も許可も観測面には残るので、事後に追える状態は保つ。

use std::collections::{BTreeMap, BTreeSet};

use crate::history::ActivityKind;
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
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TriggerGrants {
    /// manifest の `requiredSecrets`。
    pub secrets: BTreeSet<String>,
    /// manifest の `allowedHosts` (#57)。宣言順を保つ (同意画面に書いた順で出す)。
    pub hosts: Vec<HostPattern>,
}

/// 宣言を持たないトリガーの権限 = 何も許可されていない。実行文脈はあるが grants が
/// 見つからない場合 (discovery に載っていない等) に借りる先。
static NO_GRANTS: TriggerGrants = TriggerGrants {
    secrets: BTreeSet::new(),
    hosts: Vec::new(),
};

/// `allowedHosts` の 1 エントリ。
///
/// **ホスト単位までしか見ない。** パスやクエリは宣言に含めない (Chrome の
/// `host_permissions` と同じ粒度)。ポートも見ないので、宣言したホストなら任意のポートに
/// 出られる — スキームの制限 ([`decide_host`]) の方が実効的な歯止めになる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HostPattern {
    /// `api.github.com` — 完全一致。
    Exact(String),
    /// `*.githubusercontent.com` — **サブドメインのみ**。保持しているのは `*.` を除いた
    /// 部分で、`example.com` 自身にはマッチしない (1 ラベル以上の前置が要る)。
    Subdomain(String),
}

impl HostPattern {
    /// 宣言として書かれた元の文字列。UI と同意画面はこれを見せる。
    pub(crate) fn as_declared(&self) -> String {
        match self {
            Self::Exact(h) => h.clone(),
            Self::Subdomain(suffix) => format!("*.{suffix}"),
        }
    }

    /// `host` は URL から取り出した小文字のホスト名。
    fn matches(&self, host: &str) -> bool {
        match self {
            Self::Exact(h) => host == h,
            // `*.example.com` は `x.example.com` にマッチし `example.com` にはしない。
            // 末尾一致だけだと `notexample.com` が通るので、境界が `.` であることまで見る。
            Self::Subdomain(suffix) => host
                .strip_suffix(suffix)
                .and_then(|prefix| prefix.strip_suffix('.'))
                .is_some_and(|label| !label.is_empty()),
        }
    }
}

/// `allowedHosts` の 1 エントリを検証して [`HostPattern`] にする。discovery が使う。
///
/// **宣言の意味を失わせる書き方は拒否する。** 単独の `*` は「全部」と同義で宣言が無意味に
/// なるし、`*.com` のような公開サフィックス直下も同じ。同意画面に出す文字列が実際の制限と
/// 一致していなければ、それはセキュリティシアターになる。
///
/// 公開サフィックスの完全な判定 (PSL) は持たない。`*.co.uk` のような 2 ラベルの公開
/// サフィックスは通る。ここで弾きたいのは「明らかに宣言を放棄している書き方」であって、
/// PSL の維持まで背負うと単一開発者の手に余る (#55 の「やらないと決めるもの」と同じ判断)。
pub(crate) fn parse_host_pattern(raw: &str) -> Result<HostPattern, String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        return Err("empty host pattern".to_string());
    }
    // スキーム / パス / ポート / 認証情報を書かれたら、宣言と実際の制限がずれる。
    if let Some(bad) = s.find(['/', ':', '@', '?', '#', ' ']) {
        return Err(format!(
            "host pattern '{raw}' must be a bare host name (found '{}'); \
             scheme, port and path are not part of the declaration",
            &s[bad..bad + 1]
        ));
    }

    if let Some(suffix) = s.strip_prefix("*.") {
        validate_host_name(suffix).map_err(|e| format!("host pattern '{raw}': {e}"))?;
        if !suffix.contains('.') {
            return Err(format!(
                "host pattern '{raw}' is too broad; '*.{suffix}' covers a whole suffix"
            ));
        }
        return Ok(HostPattern::Subdomain(suffix.to_string()));
    }

    if s.contains('*') {
        return Err(format!(
            "host pattern '{raw}': '*' is only allowed as a leading '*.' subdomain wildcard"
        ));
    }
    validate_host_name(&s).map_err(|e| format!("host pattern '{raw}': {e}"))?;
    Ok(HostPattern::Exact(s))
}

/// ホスト名として成立しているか。ラベルが空 (`a..b` / `.a` / `a.`) や、`[a-z0-9-]` 以外の
/// 文字を弾く。IP リテラルは数字とドットなのでそのまま通る。
fn validate_host_name(s: &str) -> Result<(), String> {
    if s.is_empty() {
        return Err("empty host name".to_string());
    }
    for label in s.split('.') {
        if label.is_empty() {
            return Err("empty label".to_string());
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!("label '{label}' starts or ends with '-'"));
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("label '{label}' has characters outside [a-z0-9-]"));
        }
    }
    Ok(())
}

/// `getSecret` の拒否理由。
///
/// **capability ごとに理由の集合が違うので型も分ける。** 1 つの enum を共用すると
/// どちらにも「相手側にしか無い理由」の腕が残り、到達しない文言を書くことになる。
/// 共通なのは変数名だけで、本文はどのみち capability 固有である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SecretDenial {
    /// トリガーの実行文脈の外から呼ばれた。
    NoContext,
    /// framework が所有する secret。宣言の有無に関わらず渡さない。
    FrameworkOwned,
    /// manifest の `requiredSecrets` に無い名前。「宣言し忘れ」はエージェント開発者の
    /// デバッグ対象、[`Self::FrameworkOwned`] は設計上通らない経路で、意味が違う。
    Undeclared,
}

impl SecretDenial {
    /// 観測面に出す本文。`[denied]` プレフィックスは [`ActivityKind`] が付ける。
    fn message(&self, name: &str) -> String {
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

/// `http.fetch` の拒否理由 (#57)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HostDenial {
    /// トリガーの実行文脈の外から呼ばれた。
    NoContext,
    /// manifest の `allowedHosts` に無いホスト。
    Undeclared,
    /// 宣言済みのホストだが、平文で出ようとした。
    InsecureScheme,
}

impl HostDenial {
    /// 観測面に出す本文。`hop` はリダイレクト追跡中ならそのことを示す。
    ///
    /// **URL 全体ではなくホストで書く。** 宣言の単位がホストなので直すべき対象を名指すのは
    /// こちらだし、[`TriggerPermissions::record`] の重複排除は本文をキーにするため、URL を
    /// 入れると同じホストへの 1000 個の URL が 1000 行になってまとめる意味が消える。
    fn message(&self, target: &FetchTarget, hop: bool) -> String {
        let at = if hop { " (after a redirect)" } else { "" };
        let host = target.host;
        match self {
            Self::NoContext => format!(
                "http.fetch to '{host}' was called outside of a trigger execution; \
                 the caller could not be identified"
            ),
            Self::Undeclared => {
                format!("host '{host}'{at} is not declared in the manifest's allowedHosts")
            }
            Self::InsecureScheme => format!(
                "'{}' to '{host}'{at} is not https; plaintext is only allowed to loopback",
                target.scheme
            ),
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
///
/// 帰結として、dev の env fallback 経由では**宣言の綴りと実際に読まれる値がずれうる**:
/// `requiredSecrets: ["github-token"]` は宣言どおり通り、`secret_store::get` はそれを
/// `CHAMBERLAIN_SECRET_GITHUB_TOKEN` に解決するので、`github_token` として置いた値が返る。
/// keyring (本番) は綴りを区別するので起きない。**framework のキーだけは正規化して弾く**
/// のはこの差が実害になる唯一の相手だからで、一般の名前まで正規化して一致させると
/// 「宣言と違う名前が読める」を仕様として認めることになる。
pub(crate) fn decide_secret(
    grants: Option<&TriggerGrants>,
    name: &str,
) -> Result<(), SecretDenial> {
    if is_framework_secret(name) {
        return Err(SecretDenial::FrameworkOwned);
    }
    match grants {
        None => Err(SecretDenial::NoContext),
        Some(g) if g.secrets.contains(name) => Ok(()),
        Some(_) => Err(SecretDenial::Undeclared),
    }
}

/// `http.fetch` の宛先のうち、判断に使う 2 つ。URL のパースは呼び出し側 ([`crate::http`]) が
/// 行う — この層を `reqwest` から独立させておくため。
pub(crate) struct FetchTarget<'a> {
    /// 小文字のスキーム (`"https"` 等)。
    pub scheme: &'a str,
    /// 小文字のホスト名。ポートは含まない。
    pub host: &'a str,
}

impl FetchTarget<'_> {
    /// 平文を許すループバックか。開発とローカルツール連携のための例外で、これ以外の宛先は
    /// 宣言済みでも https でなければ通さない (宣言済みホストに平文で token を出せると、
    /// 宛先を縛った意味が半分消える)。
    fn is_loopback(&self) -> bool {
        // 127.0.0.0/8 全体と ::1 を std に判定させる。手書きの列挙にすると 127.0.0.2 が
        // 漏れる。なお IPv6 リテラルは `[` `:` を含むので `allowedHosts` に宣言できず、
        // ここに到達する前に Undeclared で落ちる (宣言の粒度をホスト名に限った帰結)。
        self.host == "localhost"
            || self
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    }
}

/// `http.fetch(url)` を通してよいかの判断。副作用を持たない。
///
/// 順序は「文脈 → 宣言 → スキーム」。宣言に無いホストへの平文アクセスは
/// [`HostDenial::Undeclared`] として報告する — 先に直すべきは宣言の方だから。
pub(crate) fn decide_host(
    grants: Option<&TriggerGrants>,
    target: &FetchTarget,
) -> Result<(), HostDenial> {
    let Some(grants) = grants else {
        return Err(HostDenial::NoContext);
    };
    if !grants.hosts.iter().any(|p| p.matches(target.host)) {
        return Err(HostDenial::Undeclared);
    }
    if target.scheme != "https" && !(target.scheme == "http" && target.is_loopback()) {
        return Err(HostDenial::InsecureScheme);
    }
    Ok(())
}

/// 観測面に出す本文の上限 (文字数)。実際の本文は 100 文字前後で、これは
/// 「トリガーが渡した文字列がそのまま伸びる」経路への蓋。
const MAX_MESSAGE_CHARS: usize = 200;

/// 1 回の JS 実行で溜められる記録の種類数。これを超えた分は種類ごとに 1 行へ畳む。
///
/// **重複排除だけでは足りない。** キーになる本文にはトリガーが決めた文字列が入るので、
/// `for (i of 0..100000) getSecret("x"+i)` のように毎回違う名前で呼べば種類数は
/// いくらでも増やせる。そうなると (1) 線形走査が O(n²) になり (2) `leave()` の時点で
/// 10 万件の history 追記と UI emit が走り、20,000 行の retention を丸ごと押し流す。
const MAX_PENDING: usize = 32;

/// 溢れた分をまとめる行の本文。
const OVERFLOW_MESSAGE: &str = "...and more of the same kind (suppressed)";

/// 文字境界で切り詰める。バイト長で切ると多バイト文字を割ってしまう。
fn truncate_chars(s: String, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &s[..cut]),
        None => s,
    }
}

/// op が観測面に流したい 1 件。`TauriHost` が JS 実行の後に回収する。
///
/// op から直接 activity を書かないのは、op が `AppHandle` と履歴ハンドルを持たないため。
/// 溜めて回収する形にすると、`OpState` に置くものが manifest 由来の宣言だけで済む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpActivity {
    /// 帰属先のトリガー。実行文脈の外なら `None`。
    pub trigger_id: Option<String>,
    pub kind: ActivityKind,
    pub message: String,
    /// 同じ実行の中で同じ記録が起きた回数。1 のときは表示に出さない。
    pub count: u32,
}

impl OpActivity {
    /// **本文はここで切り詰める。** 本文にはトリガーが決めた文字列 (secret 名 / ホスト /
    /// model 名) が入るので、素通しすると `ai.complete({ model: "A".repeat(10_000_000) })`
    /// が 10 MB の行を履歴に書ける。入口が 1 箇所なので、ここで止めれば全経路に効く。
    fn new(trigger_id: Option<String>, kind: ActivityKind, message: String) -> Self {
        Self {
            trigger_id,
            kind,
            message: truncate_chars(message, MAX_MESSAGE_CHARS),
            count: 1,
        }
    }

    /// 表示用の本文。2 回以上なら回数を添える。
    pub(crate) fn display(&self) -> String {
        match self.count {
            1 => self.message.clone(),
            n => format!("{} (×{n})", self.message),
        }
    }
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
    /// 未回収の記録 (拒否と `ai.complete` の呼び出し)。
    pending: Vec<OpActivity>,
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

    /// JS の実行が終わったら呼ぶ。実行文脈を閉じ、その間に溜まった記録を返す
    /// (以後の op は `NoContext` で落ちる)。
    ///
    /// 回収を別メソッドに分けない。分けると「閉じたが回収していない」状態が作れて、
    /// 溜まった記録が次の実行の持ち物として報告されうる。
    pub(crate) fn leave(&mut self) -> Vec<OpActivity> {
        self.current = None;
        std::mem::take(&mut self.pending)
    }

    /// 今の実行文脈の宣言。`None` は文脈の外。
    ///
    /// 宣言を持たないトリガー (discovery に載っていない等) は空の [`TriggerGrants`] に
    /// 倒す — 素通りではなく「何も許可されていない」として扱う。
    fn current_grants(&self) -> Option<&TriggerGrants> {
        let id = self.current.as_deref()?;
        Some(self.grants.get(id).unwrap_or(&NO_GRANTS))
    }

    /// `await` を挟む op (`http.fetch`) 用の実行文脈のスナップショット。
    ///
    /// `OpState` を借りたまま `await` することはできないので、判断に要るものを先に
    /// 取り出しておく。リダイレクトの各ホップは**この写しに対して**照合するので、
    /// 1 回の fetch の途中で権限が入れ替わることはない。
    pub(crate) fn snapshot(&self) -> PermissionSnapshot {
        PermissionSnapshot {
            trigger_id: self.current.clone(),
            grants: self.current_grants().cloned(),
        }
    }

    /// `getSecret(name)` を通してよいか。拒否した場合は記録に残して `false` を返す。
    ///
    /// こちらは同期 op なので判断と記録を 1 つにできる。`http.fetch` が
    /// [`Self::snapshot`] と [`Self::record`] に分かれているのは `await` を挟むためで、
    /// 判断そのものは同じ層 ([`decide_secret`] / [`decide_host`]) にある。
    pub(crate) fn authorize_secret(&mut self, name: &str) -> bool {
        match decide_secret(self.current_grants(), name) {
            Ok(()) => true,
            Err(denial) => {
                let record = OpActivity::new(
                    self.current.clone(),
                    ActivityKind::Denied,
                    denial.message(name),
                );
                self.record(record);
                false
            }
        }
    }

    /// `ai.complete` の呼び出しを記録する (#57)。
    ///
    /// **拒否は無い。** 宛先を core が決める (Anthropic 固定) ので `allowedHosts` のような
    /// 宣言が取れない一方、これは framework が持つ API キーの持ち出しにあたる — 無制限に
    /// 呼べるとエンドユーザーの課金でトリガー作者の用事が処理できてしまう。そこで宣言の
    /// 代わりに必ず痕跡を残す。レート制限は必要になってから (Issue #57)。
    ///
    /// 本文に prompt の中身も長さも入れない。中身は履歴がそのまま漏洩面になるため、長さは
    /// [`Self::record`] の重複排除のキーが毎回変わって**まとめる意味が消える**ため。
    /// 知りたいのは呼び出しの量で、それは回数が持つ。
    pub(crate) fn record_ai_call(&mut self, model: &str) {
        let record = OpActivity::new(
            self.current.clone(),
            ActivityKind::AiCall,
            format!("ai.complete model={model}"),
        );
        self.record(record);
    }

    /// 記録を 1 件足す。**同じ実行の中で同じ記録は 1 行にまとめ、回数だけ数える。**
    ///
    /// 1 回の tick に N 件を素通しすると、N 行の history 追記と N 回の UI emit になる。
    /// retention は 20,000 行 (`MAX_ROWS`) なので、ループの中で `getSecret` を呼ぶトリガー
    /// 1 つが**本物の `[notify]` / `[error]` を履歴から押し出せてしまう**。宣言し忘れの
    /// 検出手段が `[denied]` である以上、その経路自身が観測面を潰せる状態は許容できない。
    /// `[ai]` も同じ理由でまとめる (回数は課金に効くので捨てずに数える)。
    ///
    /// **種類数も [`MAX_PENDING`] で頭打ちにする。** 本文はトリガーが決めた文字列を含むので、
    /// 毎回違う名前で呼べば「同じ記録」にならず種類が無限に増える。溢れた分は kind ごとの
    /// 1 行へ畳むので、要素数は `MAX_PENDING + kind 数` で止まり、線形走査で足りる。
    pub(crate) fn record(&mut self, activity: OpActivity) {
        if self.bump(&activity) {
            return;
        }
        if self.pending.len() < MAX_PENDING {
            self.pending.push(activity);
            return;
        }
        // 上限に当たった。個別には残せないが「起きたこと」は消さない。
        let overflow = OpActivity::new(
            activity.trigger_id,
            activity.kind,
            OVERFLOW_MESSAGE.to_string(),
        );
        if !self.bump(&overflow) {
            self.pending.push(overflow);
        }
    }

    /// 同じ記録が既にあれば回数を足して `true`。
    fn bump(&mut self, activity: &OpActivity) -> bool {
        match self.pending.iter_mut().find(|a| {
            a.trigger_id == activity.trigger_id
                && a.kind == activity.kind
                && a.message == activity.message
        }) {
            Some(existing) => {
                existing.count = existing.count.saturating_add(1);
                true
            }
            None => false,
        }
    }
}

/// `await` を挟む op が持ち回る実行文脈の写し。
///
/// [`TriggerPermissions`] は `OpState` の中にあり、`await` を跨いで借りたままにはできない。
/// 一方リダイレクトの追跡は**ホップごとに判断が要る**ので、判断材料をこちらに写して持ち回る。
pub(crate) struct PermissionSnapshot {
    trigger_id: Option<String>,
    /// `None` は実行文脈の外。
    grants: Option<TriggerGrants>,
}

impl PermissionSnapshot {
    /// `http.fetch` の 1 ホップを通してよいか。拒否は**記録を返すだけ**で、`OpState` への
    /// 反映は呼び出し側が `await` の外で [`TriggerPermissions::record`] する。
    pub(crate) fn check_host(&self, target: &FetchTarget, hop: bool) -> Result<(), OpActivity> {
        decide_host(self.grants.as_ref(), target).map_err(|denial| {
            OpActivity::new(
                self.trigger_id.clone(),
                ActivityKind::Denied,
                denial.message(target, hop),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(names: &[&str]) -> TriggerGrants {
        TriggerGrants {
            secrets: names.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
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
            Err(SecretDenial::Undeclared)
        );
    }

    #[test]
    fn trigger_without_declaration_gets_nothing() {
        assert_eq!(
            decide_secret(Some(&grants(&[])), "github_token"),
            Err(SecretDenial::Undeclared)
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
            Err(SecretDenial::FrameworkOwned)
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
                Err(SecretDenial::FrameworkOwned),
                "'{alias}' が framework のキーとして弾かれていない"
            );
        }
    }

    /// 実行文脈の外は既定で拒否。投げっぱなしの promise が後から解決した場合等。
    #[test]
    fn outside_of_a_trigger_everything_is_denied() {
        assert_eq!(
            decide_secret(None, "github_token"),
            Err(SecretDenial::NoContext)
        );
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

    // --- allowedHosts の宣言の検証 (#57) ---

    #[test]
    fn plain_host_names_parse() {
        assert_eq!(
            parse_host_pattern("api.github.com"),
            Ok(HostPattern::Exact("api.github.com".to_string()))
        );
        // 大文字と前後の空白は正規化する。DNS 名は大文字小文字を区別しない。
        assert_eq!(
            parse_host_pattern("  API.GitHub.COM "),
            Ok(HostPattern::Exact("api.github.com".to_string()))
        );
    }

    #[test]
    fn subdomain_wildcard_parses_and_round_trips() {
        let p = parse_host_pattern("*.githubusercontent.com").expect("valid");
        assert_eq!(
            p,
            HostPattern::Subdomain("githubusercontent.com".to_string())
        );
        assert_eq!(p.as_declared(), "*.githubusercontent.com");
    }

    /// 宣言の意味を失わせる書き方は通さない。ここが通ると #55 の同意画面が
    /// 「実際には何も制限していない宣言」を見せることになる。
    #[test]
    fn declarations_that_would_mean_nothing_are_rejected() {
        for bad in ["*", "*.com", "*.localhost", ""] {
            assert!(
                parse_host_pattern(bad).is_err(),
                "'{bad}' が宣言として通ってしまった"
            );
        }
    }

    /// ワイルドカードは先頭の `*.` だけ。途中や部分一致は通さない。
    #[test]
    fn wildcards_are_only_allowed_as_a_leading_label() {
        for bad in ["api.*.com", "*github.com", "gith*b.com"] {
            assert!(parse_host_pattern(bad).is_err(), "'{bad}' が通ってしまった");
        }
    }

    /// スキームやパス、ポートを書かれたら宣言と実際の制限がずれる。粒度はホストまで。
    #[test]
    fn scheme_port_and_path_are_not_part_of_the_declaration() {
        for bad in [
            "https://api.github.com",
            "api.github.com/v3",
            "api.github.com:8443",
            "user@api.github.com",
        ] {
            assert!(parse_host_pattern(bad).is_err(), "'{bad}' が通ってしまった");
        }
    }

    #[test]
    fn malformed_host_names_are_rejected() {
        for bad in [
            "a..b.com",
            ".github.com",
            "github.com.",
            "-foo.com",
            "fo o.com",
        ] {
            assert!(parse_host_pattern(bad).is_err(), "'{bad}' が通ってしまった");
        }
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

        let recorded = s.leave();
        assert_eq!(recorded.len(), 2, "踏み外した宣言の種類ぶんだけ残る");
        // 回数は捨てない。1 回と 1000 回は運用上まったく違う。
        assert!(
            recorded[0].display().ends_with("(×1000)"),
            "{:?}",
            recorded[0]
        );
        assert_eq!(recorded[1].display(), recorded[1].message);
    }

    /// 毎回違う名前で呼ばれても種類数が爆発しない。重複排除のキーはトリガーが決めた文字列を
    /// 含むので、これが無いと 10 万件の history 追記と UI emit が走って retention を潰せる。
    #[test]
    fn distinct_denials_are_capped_and_folded_into_one_row() {
        let mut s = state();
        s.enter("silent");
        for i in 0..10_000 {
            assert!(!s.authorize_secret(&format!("token_{i}")));
        }

        let recorded = s.leave();
        assert!(
            recorded.len() <= MAX_PENDING + 1,
            "種類数が頭打ちになっていない: {}",
            recorded.len()
        );
        let overflow = recorded
            .iter()
            .find(|a| a.message == OVERFLOW_MESSAGE)
            .expect("溢れた分の行が無い");
        assert!(overflow.count > 1, "溢れた件数が数えられていない");
    }

    /// トリガーが渡した文字列がそのまま履歴の行長になるのを止める。
    #[test]
    fn caller_supplied_text_cannot_grow_the_recorded_message() {
        let mut s = state();
        s.enter("declaring");
        s.record_ai_call(&"A".repeat(10_000_000));

        let recorded = s.leave();
        assert!(
            recorded[0].message.chars().count() <= MAX_MESSAGE_CHARS + 1,
            "本文が切り詰められていない: {} 文字",
            recorded[0].message.chars().count()
        );
    }

    /// `ai.complete` も同じ仕組みでまとめる。**本文に呼び出しごとに変わる値を入れない**の
    /// が効く条件で、prompt の長さを入れていた頃はここが 3 行になっていた。
    #[test]
    fn ai_calls_to_the_same_model_collapse_into_one_row_with_a_count() {
        let mut s = state();
        s.enter("declaring");
        s.record_ai_call("claude-sonnet-5");
        s.record_ai_call("claude-sonnet-5");
        s.record_ai_call("claude-opus-5");

        let recorded = s.leave();
        assert_eq!(recorded.len(), 2, "model ごとに 1 行: {recorded:?}");
        assert_eq!(recorded[0].kind, ActivityKind::AiCall);
        assert_eq!(
            recorded[0].display(),
            "ai.complete model=claude-sonnet-5 (×2)"
        );
        assert_eq!(recorded[1].display(), "ai.complete model=claude-opus-5");
    }

    /// 宣言の無いトリガーが grants に載っていなくても素通りさせない。
    #[test]
    fn unknown_trigger_id_is_treated_as_having_no_grants() {
        let mut s = state();
        s.enter("never-discovered");
        assert!(!s.authorize_secret("github_token"));
    }
}
