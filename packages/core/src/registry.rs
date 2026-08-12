//! 実行時登録の受け取り口 (#58 / #55)。
//!
//! トリガーの出どころは 2 つある。ビルド時に焼き込まれたもの (resource dir) と、実行時に
//! エンドユーザーが登録したもの (`<app_data>/triggers/`)。**両者は discovery から先が
//! 完全に同じ**で、権限の宣言も同じように強制される (#56 / #57)。「焼き込みなら何でも
//! できる」という例外を作ると、焼き込みを偽装する経路が価値を持ってしまう。
//!
//! このモジュールが持つのは、その 2 つ目のソースにファイルを迎え入れるときの門番だけ:
//! 名前の検証 ([`validate_trigger_id`] / [`validate_entry_path`]) と、上限付きのコピー
//! ([`copy_tree`])。manifest の意味の検証 (schedule / tz / allowedHosts) は `lib.rs` 側に
//! あり、**焼き込みと共通**である。
//!
//! # 何を守っているのか
//!
//! 選んだフォルダの中身は「他人が書いたもの」でありうる ((b) 社内配布 / (c) 秘書の生成)。
//! id はコピー先のディレクトリ名になり、`entry` は V8 が読むファイルのパスになる。
//! どちらも文字列をそのまま使うと `<app_data>` の外を書いたり読んだりできてしまう。

use std::fs;
use std::path::{Component, Path};

use serde::Serialize;

/// トリガーの出どころ。UI に見せる (#58) ほか、解除できるかの判断に使う。
///
/// UI に渡すときも文字列に潰さずこの型のまま送る。潰すと「同梱と衝突したら登録を断る」
/// のような判断が文字列比較になり、種類が増えたときに漏れてもコンパイラが教えてくれない。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TriggerSource {
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

/// ディレクトリの最大深さ。
const MAX_DEPTH: usize = 8;

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

/// ディレクトリを再帰コピーする。上限を超えたら**途中で失敗する** (呼び出し側が後始末する)。
///
/// - シンボリックリンクは拒否する。追うと `<app_data>` の外を巻き込み、追わなければ
///   コピー先で壊れたリンクになる。どちらも「登録できた」と言えない
/// - ドット始まりのエントリは読み飛ばす。リポジトリのフォルダをそのまま選んだときに
///   `.git` を丸ごと持ち込まないため。トリガーの動作に要るものがドット始まりになることは無い
/// - 上限はファイル数・総バイト数・深さの 3 つ。zip を受ける口 (#55 の論点) を後から
///   足すときも、展開先をここに通せば同じ上限がかかる
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<CopyStats, String> {
    let mut stats = CopyStats::default();
    copy_dir(src, dst, 0, &mut stats)?;
    Ok(stats)
}

fn copy_dir(src: &Path, dst: &Path, depth: usize, stats: &mut CopyStats) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("ディレクトリが深すぎます (最大 {MAX_DEPTH} 階層)"));
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
