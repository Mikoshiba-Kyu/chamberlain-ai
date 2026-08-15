//! 実行時登録の受け取り口 (#58 / #55)。
//!
//! トリガーの出どころは 2 つある。ビルド時に焼き込まれたもの (resource dir) と、実行時に
//! エンドユーザーが登録したもの (`<app_data>/triggers/`)。**両者は discovery から先が
//! 完全に同じ**で、権限の宣言も同じように強制される (#56 / #57)。「焼き込みなら何でも
//! できる」という例外を作ると、焼き込みを偽装する経路が価値を持ってしまう。
//!
//! このモジュールが持つのは、その 2 つ目のソースにファイルを迎え入れるときの門番だけ:
//! 名前の検証 ([`validate_trigger_id`] / [`validate_entry_path`])、entry の静的検査
//! ([`lint_entry_source`])、上限付きのコピー ([`copy_tree`])。manifest の意味の検証
//! (schedule / tz / allowedHosts) は `lib.rs` 側にあり、**焼き込みと共通**である。
//!
//! # 何を守っているのか
//!
//! 選んだフォルダの中身は「他人が書いたもの」でありうる ((b) 社内配布 / (c) 秘書の生成)。
//! id はコピー先のディレクトリ名になり、`entry` は V8 が読むファイルのパスになる。
//! どちらも文字列をそのまま使うと `<app_data>` の外を書いたり読んだりできてしまう。

use std::fs;
use std::path::{Component, Path};
use std::sync::{Mutex, MutexGuard};

use serde::Serialize;

/// トリガーの出どころ。UI に見せる (#58) ほか、解除できるかの判断に使う。
///
/// UI に渡すときも文字列に潰さずこの型のまま送る。潰すと「同梱と衝突したら登録を断る」
/// のような判断が文字列比較になり、種類が増えたときに漏れてもコンパイラが教えてくれない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerSource {
    /// `bundle.resources` でアプリに焼き込まれたもの。エージェント開発者のもので、
    /// エンドユーザーは外せない (「アプリの形」の一部)。
    Bundled,
    /// 実行時に `<app_data>/triggers/` へ登録されたもの。エンドユーザーが入れ外しできる。
    Registered,
}

impl TriggerSource {
    /// ログ用の短い識別子 (JSON 側は Serialize が同じ文字列を出す)。
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Registered => "registered",
        }
    }
}

/// framework が予約している ID。トリガーは名乗れない。
///
/// `__meta__` は state store の予約 namespace、`__task__` はトリガーに帰属しない
/// activity の source。どちらも名乗られると「framework 自身の記録」と混ざる。
const RESERVED_IDS: [&str; 2] = ["__meta__", "__task__"];

/// framework が予約している ID か。**discovery と登録の両方から見る。**
///
/// 予約語の一覧が出どころごとに分かれていると、片方の経路 (`<app_data>/triggers/` に
/// 直接置く) からだけ `__task__` を名乗れてしまう。
pub(crate) fn is_reserved_id(id: &str) -> bool {
    RESERVED_IDS.contains(&id)
}

/// ID の最大長。コピー先のディレクトリ名になるので、パス長の上限を踏まない範囲に抑える。
const MAX_ID_LEN: usize = 64;

/// 1 トリガーに含められるファイル数。
///
/// トリガーは manifest + エントリ + せいぜい数本のモジュールで足りる。上限は「巨大な
/// フォルダを間違って選んだ」を弾くためのもので、悪意の防波堤としては下の総バイト数が主。
const MAX_FILES: usize = 200;

/// 1 トリガーの総バイト数。
const MAX_TOTAL_BYTES: u64 = 8 * 1024 * 1024;

/// 1 トリガーに含められるディレクトリ数 (コピー先に作る数、根を含む)。
///
/// ファイル数と総バイト数だけでは、深さ制限の下でも横に広がる空ディレクトリの木
/// (`a/1..a/100000`) が上限なしに複製できてしまう。1 バイトも運ばない入力で
/// `create_dir` を無限に叩かせない。
const MAX_DIRS: usize = 200;

/// ディレクトリの最大深さ。
const MAX_DEPTH: usize = 8;

/// 据え置き / 取り外しを直列化する。
///
/// 入れ替えは「staging に積む → 旧版を `.old-<id>` へ退避 → rename」の 3 手で進む。
/// 同じ id に対して 2 本が同時に走ると、後発の頭にある残骸掃除が先発の退避先を消し、
/// 先発の rename が失敗したときに戻す先が無くなる。**「入れ替えに失敗しても前の
/// トリガーが残る」はこのモジュールが守っている唯一の約束**なので、UI 側が二重に
/// 押させない作りであることに頼らず、ここで閉じておく。
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// poisoning は「別の登録がパニックした」以上の意味を持たない。ディスク上の残骸は
/// `.staging-` / `.old-` の命名規則で次回に掃除できるので、ロックごと使えなくしない。
fn lock_installs() -> MutexGuard<'static, ()> {
    INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 登録中の一時ディレクトリの接頭辞。
///
/// ドット始まりなのは discovery が読み飛ばすため。コピー途中でプロセスが落ちても、
/// 残骸が「同じ id のトリガー」として拾われない。
const STAGING_PREFIX: &str = ".staging-";

/// 置き換え中に退避した旧バージョンの接頭辞。同上の理由でドット始まり。
const BACKUP_PREFIX: &str = ".old-";

/// コピーの結果。観測面に「何を入れたか」を残すために使う。
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct CopyStats {
    pub files: usize,
    pub bytes: u64,
    /// 作ったディレクトリ数 (根を含む)。上限の判定にだけ使う。
    pub dirs: usize,
    /// 読み飛ばしたドット始まりのエントリ数 (`.git` / `.DS_Store` 等)。
    pub skipped: usize,
}

/// トリガー ID がコピー先のディレクトリ名として安全か検証する。
///
/// **登録・解除の経路だけで使う。** ここを通った id だけが `<app_data>/triggers/<id>`
/// という形でパスに埋め込まれる。焼き込み側の id は開発者が書いたもので、かつパスを
/// 組み立てる材料にならない (ディレクトリは既にそこにある) ので同じ検証はしない。
pub(crate) fn validate_trigger_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("トリガー ID が空です".to_string());
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!("トリガー ID が長すぎます (最大 {MAX_ID_LEN} 文字)"));
    }
    if is_reserved_id(id) {
        return Err(format!("トリガー ID '{id}' は framework の予約語です"));
    }
    if id.starts_with('.') {
        return Err("トリガー ID をドットで始めることはできません".to_string());
    }
    // 許すのは ASCII 英数と `-` `_` のみ。`.` を落としているのは `..` を弾くためだけでなく、
    // 「拡張子に見える id」でコピー先が紛らわしくなるのを避けるため。
    if let Some(bad) = id
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Err(format!(
            "トリガー ID に使えない文字が含まれています: '{bad}' (ASCII 英数と - _ のみ)"
        ));
    }
    Ok(())
}

/// manifest の `entry` がトリガーのディレクトリ内を指しているか検証する。
///
/// **焼き込みにも適用する。** `entry: "../../../secrets.ts"` は V8 にトリガーの外の
/// ファイルを読ませられる。焼き込みだけ例外にすると、#55 の登録機構がある以上「焼き込みを
/// 装って抜ける」経路になる。
pub(crate) fn validate_entry_path(entry: &str) -> Result<(), String> {
    if entry.is_empty() {
        return Err("entry が空です".to_string());
    }
    let path = Path::new(entry);
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            _ => {
                return Err(format!(
                    "entry はトリガーのディレクトリ内の相対パスでなければなりません: '{entry}'"
                ))
            }
        }
    }
    Ok(())
}

/// 登録経路の `entry` 検証。[`validate_entry_path`] に「コピーを生き延びるか」を足す。
///
/// [`copy_tree`] はドット始まりのエントリを読み飛ばすので、`entry` がその下に居ると
/// **同意画面まで通ったのに再起動後は load error** になる。入れる前に断れば直せる話を、
/// 入れた後の「壊れているトリガー」に化けさせない。焼き込みには掛けない (コピーを
/// 通らないので、そこにドット始まりが居ても実際に読める)。
pub(crate) fn validate_registered_entry(entry: &str) -> Result<(), String> {
    validate_entry_path(entry)?;
    let hidden = Path::new(entry)
        .components()
        .any(|c| matches!(c, Component::Normal(name) if name.to_string_lossy().starts_with('.')));
    if hidden {
        return Err(format!(
            "entry '{entry}' がドット始まりのファイル/フォルダを含んでいます (登録時にコピーされません)"
        ));
    }
    Ok(())
}

/// 静的検査のために entry を読む。**上限はコピーと同じ** ([`MAX_TOTAL_BYTES`])。
///
/// 下見はコピーの前に走るので、ここに上限が無いと、コピー側なら弾かれる大きさの
/// ファイルを同意画面より先にまるごとメモリへ載せることになる。
pub(crate) fn read_entry_source(path: &Path, entry: &str) -> Result<String, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("entry '{entry}' を読めません: {e}"))?
        .len();
    if len > MAX_TOTAL_BYTES {
        return Err(format!(
            "entry '{entry}' が大きすぎます (最大 {} MB)",
            MAX_TOTAL_BYTES / (1024 * 1024)
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("entry '{entry}' を読めません: {e}"))
}

/// entry スクリプトを仕様書 (#60) のチェックリストで機械的に見る (#61)。
///
/// **同意画面に出す材料であって、安全の担保ではない。** 何ができるかを決めているのは
/// manifest の宣言 (#56 / #57) で、そちらは Rust 側で強制されている。ここが見ているのは
/// 「仕様から外れていて動かないのに、登録して再起動するまで気づけない」種類の間違いで、
/// **書いたのが AI なら誰も中身を読んでいない** (#61 の前提) 以上、機械が読むしかない。
///
/// 戻り値の非対称は意図的:
///
/// - `Err` — **このままでは絶対に動かない**ものだけ。入口で断る方が、入れてから
///   「壊れているトリガー」として一覧に並ぶより直しやすい ([`validate_registered_entry`]
///   と同じ判断)
/// - `Ok(warnings)` — 動くかもしれないが仕様から外れているもの。文字列マッチなので
///   誤検知しうる以上、**拒否はしない**。読み手に見せて判断してもらう
pub(crate) fn lint_entry_source(source: &str) -> Result<Vec<String>, String> {
    if !exports_tick(source) {
        return Err(
            "index.ts に tick の export が見つかりません (`export function tick(ctx)` が要ります)"
                .to_string(),
        );
    }

    let mut warnings = Vec::new();
    if source.lines().any(is_import_statement) {
        warnings.push(
            "import 文があります。トリガーは相対 import も外部モジュールも解決できないので、\
             処理は index.ts 1 つに収めてください"
                .to_string(),
        );
    }
    if has_bare_call(source, "fetch") {
        warnings.push(
            "素の fetch() を呼んでいます。トリガーの実行環境に fetch は無いので、\
             chamberlain.http.fetch() を使ってください"
                .to_string(),
        );
    }
    if has_bare_call(source, "require") || source.contains("process.env") {
        warnings.push(
            "require() / process.env があります。Node.js ではないので存在しません".to_string(),
        );
    }
    if source.contains("export default") {
        warnings.push(
            "export default は認識されません。呼ばれるのは tick という名前の named export だけです"
                .to_string(),
        );
    }
    Ok(warnings)
}

/// 空白の連なりを 1 個に潰す。`export  async\n  function tick` のような書き方を
/// 素朴な `contains` で拾うための前処理。
fn squeeze_whitespace(source: &str) -> String {
    source.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `tick` が named export されているか。**見落としは拒否に直結する**ので、書き方は
/// 広めに認める (`export function` / `export const` / 末尾の `export { tick }`)。
fn exports_tick(source: &str) -> bool {
    let squeezed = squeeze_whitespace(source);
    const DIRECT: [&str; 5] = [
        "export function tick",
        "export async function tick",
        "export const tick",
        "export let tick",
        "export var tick",
    ];
    if DIRECT.iter().any(|p| squeezed.contains(p)) {
        return true;
    }
    // `export { tick }` / `export { run as tick }` — 波括弧の中だけを見る。名前を
    // 付け替えているときに外から見える名前は `as` の右側。
    squeezed.split("export {").skip(1).any(|rest| {
        rest.split_once('}').is_some_and(|(list, _)| {
            list.split(',').any(|item| {
                let exposed = item.rsplit(" as ").next().unwrap_or(item);
                exposed.trim() == "tick"
            })
        })
    })
}

/// 行が import 文か。行頭で判定するので、コメント行 (`// import ...`) や文字列の中に
/// 出てくる "import" は拾わない。
fn is_import_statement(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("import ")
        || trimmed.starts_with("import{")
        || trimmed.starts_with("import(")
        || trimmed.starts_with("import\"")
        || trimmed.starts_with("import'")
}

/// 式の位置で `name(` を呼んでいるか。
///
/// **見逃す方に倒してある。** 拾いたいのは「`chamberlain.` を付け忘れた呼び出し」だけで、
/// 同じ綴りは他の場所にも出る:
///
/// - `chamberlain.http.fetch(...)` — プロパティなので `.` で落とす
/// - `declare const chamberlain: { http: { fetch(url: string): ... } }` — 型の宣言。
///   仕様書 §5 が推奨している書き方そのものなので、**ここを拾うと正しく書けている
///   トリガーに嘘の警告が出る** (テストで固定してある)
///
/// 型の宣言と呼び出しを分けているのは直前のトークン。`await` / `=` / `(` のような式の
/// 位置に限れば、宣言 (直前が `{` や `;`) は自然に外れる。
fn has_bare_call(source: &str, name: &str) -> bool {
    /// 直前がこれなら「式の位置」。単独の `=` は `=>` `==` も兼ねる。
    const EXPRESSION_LEAD: [&str; 10] =
        ["await", "return", "=", "(", "[", "?", "!", "&&", "||", "+"];

    let bytes = source.as_bytes();
    source.match_indices(name).any(|(start, _)| {
        // 識別子の一部やプロパティなら別物 (`myfetch` / `.fetch`)。
        let joined = start
            .checked_sub(1)
            .map(|i| bytes[i])
            .is_some_and(|b| b == b'.' || b.is_ascii_alphanumeric() || b == b'_' || b == b'$');
        if joined {
            return false;
        }
        if !source[start + name.len()..].trim_start().starts_with('(') {
            return false;
        }
        let before = source[..start].trim_end();
        EXPRESSION_LEAD.iter().any(|lead| before.ends_with(lead))
    })
}

/// ディレクトリを再帰コピーする。上限を超えたら**途中で失敗する** (呼び出し側が後始末する)。
///
/// - シンボリックリンクは拒否する。追うと `<app_data>` の外を巻き込み、追わなければ
///   コピー先で壊れたリンクになる。どちらも「登録できた」と言えない
/// - ドット始まりのエントリは読み飛ばす。リポジトリのフォルダをそのまま選んだときに
///   `.git` を丸ごと持ち込まないため。トリガーの動作に要るものがドット始まりになることは無い
/// - 上限はファイル数・総バイト数・ディレクトリ数・深さの 4 つ。zip を受ける口 (#55 の
///   論点) を後から足すときも、展開先をここに通せば同じ上限がかかる
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<CopyStats, String> {
    let mut stats = CopyStats::default();
    copy_dir(src, dst, 0, &mut stats)?;
    Ok(stats)
}

fn copy_dir(src: &Path, dst: &Path, depth: usize, stats: &mut CopyStats) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("ディレクトリが深すぎます (最大 {MAX_DEPTH} 階層)"));
    }
    stats.dirs += 1;
    if stats.dirs > MAX_DIRS {
        return Err(format!("ディレクトリが多すぎます (最大 {MAX_DIRS} 個)"));
    }
    fs::create_dir_all(dst).map_err(|e| format!("{} を作成できません: {e}", dst.display()))?;

    let entries = fs::read_dir(src).map_err(|e| format!("{} を読めません: {e}", src.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{} を読めません: {e}", src.display()))?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            stats.skipped += 1;
            continue;
        }
        let from = entry.path();
        let to = dst.join(&name);
        // file_type() は symlink を追わない (metadata() と違う)。
        let file_type = entry
            .file_type()
            .map_err(|e| format!("{} を読めません: {e}", from.display()))?;

        if file_type.is_symlink() {
            return Err(format!(
                "シンボリックリンクは登録できません: {}",
                from.display()
            ));
        }
        if file_type.is_dir() {
            copy_dir(&from, &to, depth + 1, stats)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(format!(
                "通常のファイルではないものが含まれています: {}",
                from.display()
            ));
        }

        let len = entry
            .metadata()
            .map_err(|e| format!("{} を読めません: {e}", from.display()))?
            .len();
        stats.files += 1;
        stats.bytes += len;
        if stats.files > MAX_FILES {
            return Err(format!("ファイルが多すぎます (最大 {MAX_FILES} 個)"));
        }
        if stats.bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "サイズが大きすぎます (最大 {} MiB)",
                MAX_TOTAL_BYTES / (1024 * 1024)
            ));
        }
        fs::copy(&from, &to).map_err(|e| format!("{} をコピーできません: {e}", from.display()))?;
    }
    Ok(())
}

/// 検証済みのフォルダを `<registered_dir>/<id>/` に据える (#58)。
///
/// **既存を壊さずに入れ替える。** staging に全部コピーし、旧版を退避してから rename し、
/// 成功して初めて旧版を捨てる。途中で落ちても `<id>` の名前で半端な中身が見えることはなく、
/// 入れ替えに失敗しても前のトリガーがそのまま残る (配布物の更新に失敗して、動いていた
/// ものまで消えるのが最悪の結末なので、そこだけは避ける)。
pub(crate) fn install_trigger(
    src: &Path,
    registered_dir: &Path,
    id: &str,
) -> Result<CopyStats, String> {
    validate_trigger_id(id)?;
    // 3 手の入れ替えが終わるまで、同じ置き場に対する他の据え置き/取り外しを待たせる。
    let _guard = lock_installs();
    let dest = installed_path(registered_dir, id);
    let staging = registered_dir.join(format!("{STAGING_PREFIX}{id}"));
    let backup = registered_dir.join(format!("{BACKUP_PREFIX}{id}"));

    // 前回の失敗の残骸があれば捨ててから始める。
    let _ = fs::remove_dir_all(&staging);
    let _ = fs::remove_dir_all(&backup);

    let stats = copy_tree(src, &staging).inspect_err(|_| {
        let _ = fs::remove_dir_all(&staging);
    })?;

    let replacing = dest.exists();
    if replacing {
        fs::rename(&dest, &backup).map_err(|e| {
            let _ = fs::remove_dir_all(&staging);
            format!("既存の登録を退避できません: {e}")
        })?;
    }
    if let Err(e) = fs::rename(&staging, &dest) {
        let _ = fs::remove_dir_all(&staging);
        if replacing {
            let _ = fs::rename(&backup, &dest);
        }
        return Err(format!("{} に配置できません: {e}", dest.display()));
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(stats)
}

/// 登録されたトリガーの実体を消す (#58)。戻り値は「実際にあったか」。
///
/// 受け取るのは id ではなく**解決済みのディレクトリ**。discovery はディレクトリ名と
/// manifest の id が一致することを要求していないので、id から組み立て直すと
/// 「手で置いたフォルダを外したつもりで何も消えていない」が起こる。
///
/// 実体が無いこと自体はエラーにしない (`Ok(false)`)。登録したまま再起動していない状態や
/// 手で消された状態からでも解除は完了させたい。**ただし戻り値は呼び出し側が見ること** —
/// 何も消せなかったのに「解除しました」と言うと、再起動で戻ってくる。
pub(crate) fn uninstall_trigger(dir: &Path) -> Result<bool, String> {
    // 据え置きと同じロックを取る。入れ替えの最中に横から消されると、退避した旧版を
    // 戻す先が消えている、という形で同じ約束が破れる。
    let _guard = lock_installs();
    if !dir.is_dir() {
        return Ok(false);
    }
    fs::remove_dir_all(dir).map_err(|e| format!("{} を削除できません: {e}", dir.display()))?;
    Ok(true)
}

/// 登録済みトリガーの既定の置き場所。[`install_trigger`] が据える先であり、
/// 「同じ id が既に入っているか」の判定もここを見る。
pub(crate) fn installed_path(registered_dir: &Path, id: &str) -> std::path::PathBuf {
    registered_dir.join(id)
}

/// テスト用の一時ディレクトリ。tempfile クレートを足すほどの用事ではないので、
/// プロセス ID と連番で衝突を避ける。ファイルを触るテストはここを起点にする。
#[cfg(test)]
pub(crate) fn temp_dir(label: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("chamberlain-{label}-{}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn plain_ids_are_accepted() {
        for id in ["greeter", "pr-review", "daily_report", "v2"] {
            assert!(validate_trigger_id(id).is_ok(), "{id} は通るはず");
        }
    }

    /// id はコピー先のディレクトリ名になる。パスとして解釈される文字が通ると
    /// `<app_data>` の外に書ける。
    #[test]
    fn ids_that_escape_the_directory_are_rejected() {
        for id in ["..", "../evil", "a/b", "a\\b", ".hidden", "", "a b", "café"] {
            assert!(validate_trigger_id(id).is_err(), "{id:?} は弾くはず");
        }
    }

    #[test]
    fn reserved_ids_are_rejected() {
        assert!(validate_trigger_id("__meta__").is_err());
        assert!(validate_trigger_id("__task__").is_err());
    }

    #[test]
    fn entry_must_stay_inside_the_trigger_directory() {
        assert!(validate_entry_path("index.ts").is_ok());
        assert!(validate_entry_path("./src/index.ts").is_ok());
        for entry in ["../index.ts", "/etc/passwd", "src/../../x.ts", ""] {
            assert!(validate_entry_path(entry).is_err(), "{entry:?} は弾くはず");
        }
    }

    /// 登録経路の entry は「今そこにある」だけでなく「コピーを生き延びる」必要がある。
    /// ドット始まりの下に置かれた entry は同意画面を通ってから load error になる。
    #[test]
    fn registered_entry_must_survive_the_copy() {
        assert!(validate_registered_entry("index.ts").is_ok());
        assert!(validate_registered_entry("./src/index.ts").is_ok());
        for entry in [".hidden/index.ts", "src/.build/index.ts", ".index.ts"] {
            assert!(
                validate_registered_entry(entry).is_err(),
                "{entry:?} は弾くはず"
            );
        }
    }

    /// 拒否は「絶対に動かない」ものだけ。書き方の幅は広く認める — 見落とすと、
    /// 動くトリガーを入口で断ることになる。
    #[test]
    fn tick_export_is_recognised_in_every_form() {
        for source in [
            "export function tick(ctx) {}",
            "export async function tick(ctx) {}",
            "export  async\n  function\ttick(ctx) {}",
            "export const tick = (ctx) => ({});",
            "function tick(ctx) {}\nexport { tick };",
            "function run(ctx) {}\nexport { helper, run as tick };",
        ] {
            assert!(
                lint_entry_source(source).is_ok(),
                "{source:?} は tick の export として認めるはず"
            );
        }
    }

    #[test]
    fn missing_tick_export_is_rejected() {
        // 名前が違う / default export — どちらも呼ばれない (仕様書 §4)。
        assert!(lint_entry_source("export function run(ctx) {}").is_err());
        assert!(lint_entry_source("export default function tick(ctx) {}").is_err());
    }

    /// 仕様から外れているだけのものは**警告に留める**。文字列マッチは誤検知しうるので、
    /// ここで拒否すると動くトリガーを断りうる。
    #[test]
    fn spec_violations_are_warnings_not_rejections() {
        let source = r#"
import { x } from "./helper.ts";
export async function tick(ctx) {
  const r = await fetch("https://example.com");
  return { notify: { body: String(r.status) } };
}
"#;
        let warnings = lint_entry_source(source).unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("import")));
        assert!(warnings.iter().any(|w| w.contains("fetch")));
    }

    /// `chamberlain.http.fetch` は素の `fetch` ではない。**型の宣言も呼び出しではない** —
    /// 仕様書 §5 が「型は自分で宣言してください」と言っている以上、正しく書いたトリガーには
    /// 必ずこの形が入る。ここを取り違えると、同意画面に嘘の警告が出る。
    #[test]
    fn qualified_and_declared_fetch_are_not_flagged() {
        let source = r#"
declare const chamberlain: {
  http: {
    fetch(url: string, opts?: { method?: string }): Promise<{ status: number }>;
  };
};

export async function tick(ctx) {
  const r = await chamberlain.http.fetch("https://example.com");
  return null;
}
"#;
        assert_eq!(lint_entry_source(source).unwrap(), Vec::<String>::new());
    }

    /// コメントや文字列の中の "import" で警告を出さない。
    #[test]
    fn import_is_detected_by_line_not_by_substring() {
        let source = r#"
// import は使えないので 1 ファイルに書く
export function tick(ctx) {
  return { notify: { body: "important な話" } };
}
"#;
        assert_eq!(lint_entry_source(source).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn copy_tree_copies_nested_files() {
        let root = temp_dir("copy");
        let src = root.join("src");
        write(&src.join("manifest.json"), "{}");
        write(&src.join("lib/util.ts"), "export const x = 1;");
        let dst = root.join("dst");

        let stats = copy_tree(&src, &dst).unwrap();

        assert_eq!(stats.files, 2);
        assert!(dst.join("manifest.json").is_file());
        assert_eq!(
            fs::read_to_string(dst.join("lib/util.ts")).unwrap(),
            "export const x = 1;"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// リポジトリのフォルダをそのまま選んでも `.git` を持ち込まない。
    #[test]
    fn copy_tree_skips_dot_entries() {
        let root = temp_dir("dots");
        let src = root.join("src");
        write(&src.join("manifest.json"), "{}");
        write(&src.join(".git/config"), "[core]");
        write(&src.join(".env"), "SECRET=1");
        let dst = root.join("dst");

        let stats = copy_tree(&src, &dst).unwrap();

        assert_eq!(stats.files, 1);
        assert_eq!(stats.skipped, 2);
        assert!(!dst.join(".git").exists());
        assert!(!dst.join(".env").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_tree_rejects_oversized_input() {
        let root = temp_dir("big");
        let src = root.join("src");
        write(
            &src.join("blob.bin"),
            &"x".repeat(MAX_TOTAL_BYTES as usize + 1),
        );
        let dst = root.join("dst");

        let err = copy_tree(&src, &dst).unwrap_err();

        assert!(err.contains("大きすぎます"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 総バイト数の上限は「空のディレクトリが何万個」を止められない。
    #[test]
    fn copy_tree_rejects_too_many_directories() {
        let root = temp_dir("dirs");
        let src = root.join("src");
        write(&src.join("manifest.json"), "{}");
        for i in 0..=MAX_DIRS {
            fs::create_dir_all(src.join(format!("d{i}"))).unwrap();
        }
        let dst = root.join("dst");

        let err = copy_tree(&src, &dst).unwrap_err();

        assert!(err.contains("ディレクトリが多すぎます"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn copy_tree_rejects_symlinks() {
        let root = temp_dir("symlink");
        let src = root.join("src");
        write(&src.join("manifest.json"), "{}");
        std::os::unix::fs::symlink("/etc/passwd", src.join("link.ts")).unwrap();
        let dst = root.join("dst");

        let err = copy_tree(&src, &dst).unwrap_err();

        assert!(err.contains("シンボリックリンク"), "{err}");
        let _ = fs::remove_dir_all(&root);
    }

    /// 登録の基本形: `<registered_dir>/<id>/` に入り、staging の残骸を残さない。
    #[test]
    fn install_places_the_trigger_under_its_id() {
        let root = temp_dir("install");
        let src = root.join("incoming");
        write(&src.join("manifest.json"), r#"{"id":"probe"}"#);
        write(&src.join("index.ts"), "export function tick() {}");
        let registered = root.join("triggers");
        fs::create_dir_all(&registered).unwrap();

        let stats = install_trigger(&src, &registered, "probe").unwrap();

        assert_eq!(stats.files, 2);
        assert!(registered.join("probe/index.ts").is_file());
        assert_eq!(
            fs::read_dir(&registered).unwrap().count(),
            1,
            "staging の残骸が残っている"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// 同じ id の再登録は置き換え (配布物の更新)。古いファイルは残らない。
    #[test]
    fn install_replaces_an_existing_registration() {
        let root = temp_dir("replace");
        let registered = root.join("triggers");
        write(&registered.join("probe/index.ts"), "old");
        write(&registered.join("probe/stale.ts"), "gone");

        let src = root.join("incoming");
        write(&src.join("manifest.json"), r#"{"id":"probe"}"#);
        write(&src.join("index.ts"), "new");

        install_trigger(&src, &registered, "probe").unwrap();

        assert_eq!(
            fs::read_to_string(registered.join("probe/index.ts")).unwrap(),
            "new"
        );
        assert!(!registered.join("probe/stale.ts").exists());
        assert_eq!(fs::read_dir(&registered).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// **入れ替えに失敗しても前のトリガーは消えない。** 動いていたものまで失うのが
    /// 最悪の結末なので、コピーが通らない入力ではディスクを一切触らせない。
    #[test]
    fn failed_install_keeps_the_previous_version() {
        let root = temp_dir("rollback");
        let registered = root.join("triggers");
        write(&registered.join("probe/index.ts"), "old");

        let src = root.join("incoming");
        write(&src.join("manifest.json"), r#"{"id":"probe"}"#);
        write(
            &src.join("blob.bin"),
            &"x".repeat(MAX_TOTAL_BYTES as usize + 1),
        );

        assert!(install_trigger(&src, &registered, "probe").is_err());

        assert_eq!(
            fs::read_to_string(registered.join("probe/index.ts")).unwrap(),
            "old"
        );
        assert_eq!(fs::read_dir(&registered).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    /// id の検証は据える側でかかる (パスを組み立てる材料になるため)。
    #[test]
    fn install_rejects_unsafe_ids() {
        let root = temp_dir("unsafe");
        let registered = root.join("triggers");
        fs::create_dir_all(&registered).unwrap();
        let src = root.join("incoming");
        write(&src.join("manifest.json"), "{}");

        assert!(install_trigger(&src, &registered, "../escape").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    /// 解除は実体を消す。実体が無くてもエラーにはしない (呼び出し側は後始末を続ける) が、
    /// 「あったか」は戻り値で分かる。
    #[test]
    fn uninstall_removes_the_directory() {
        let root = temp_dir("uninstall");
        let registered = root.join("triggers");
        write(&registered.join("probe/index.ts"), "x");

        assert!(uninstall_trigger(&installed_path(&registered, "probe")).unwrap());
        assert!(!registered.join("probe").exists());
        assert!(!uninstall_trigger(&installed_path(&registered, "probe")).unwrap());
        let _ = fs::remove_dir_all(&root);
    }
}
