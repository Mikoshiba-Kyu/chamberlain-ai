//! Schedule DSL パーサと wall-clock 発火時刻計算 (#26 Phase 1)。
//!
//! `manifest.schedule` は `@` 始まりの wall-clock DSL のみを受け付ける。
//! **0.2.0 で interval schedule (`"5m"` / `"1h"`) は廃止された** (#26 決定事項 4)。
//! 展開型スケジューラでは interval は「wall-clock の生成規則のひとつ」に格下げされ、
//! `@hourly` が 1 時間 interval と同義になるため、別系統として維持する意味が無くなった。
//! さらにグリッドに割り切れない interval (`"7m"` 等) は日の境目で間隔が崩れ、展開器が
//! 「前回展開の末尾時刻」を覚える必要が生じる (展開済み境界 1 つで済まなくなる)。
//!
//! DSL 一覧:
//!
//! | 記法 | 発火時刻 |
//! |---|---|
//! | `@hourly` | 毎時 :00 |
//! | `@hourly :45` | 毎時 :45 |
//! | `@every 10m` | 毎時 :00,:10,:20,:30,:40,:50 |
//! | `@daily 09:00` | 毎日 09:00 |
//! | `@weekly MON 09:00` | 毎週月曜 09:00 |
//! | `@monthly 15 09:00` | 毎月 15 日 09:00 |
//! | `@at 2026-08-01T18:30` | その時刻に 1 回だけ |
//!
//! `@hourly :MM` と `@every Nm` は生成器として同一である。どちらも「毎時、指定した分集合で
//! 発火」であり、前者は分集合が 1 要素、後者は N 等分。よって [`Schedule::Hourly`] の
//! `minutes` に畳んで 1 本の実装で扱う。
//!
//! `@every <N>m` は **N が 5 / 10 / 15 / 20 / 30 の場合のみ有効**。60 の約数なら毎時同じ
//! 分集合になるため、展開が各時間で独立・冪等になり、展開済み境界 1 つで完結する。
//! 下限 5 分は旧 interval の下限を踏襲している (説明が増えない)。
//! カンマ区切りリスト (`@hourly :00,:15,:45`) は意図的に露出させない。リスト構文を認めると
//! 「曜日リストは? 月リストは?」と滑り出して cron に着地するため (#18)。
//!
//! TZ セマンティクス:
//! - デフォルトは user local (OS TZ を [`iana_time_zone`] で取得)
//! - `manifest.tz` に IANA name を書けば上書き
//!
//! DST 挙動 (spring-forward / fall-back):
//! - spring-forward で存在しない時刻 (例: LA の 3 月 DST 日の 02:30) は skip
//! - fall-back で重複する時刻 (例: LA の 11 月 DST 日の 01:30 が 2 回来る) は
//!   1 回目 (早い UTC) のみ発火。2 回目は `next_scheduled_after` が
//!   翌日の予定を返すことで skip される
//!
//! JST は現状 DST 無しなので日本ユーザーには直接影響しないが、将来的にユーザーが
//! 海外環境で使う場合の挙動を明文化しておく (#18)。

use chrono::{
    DateTime, Datelike, MappedLocalTime, NaiveDate, NaiveDateTime, TimeZone, Timelike, Weekday,
};
use chrono_tz::Tz;

/// `@every <N>m` で許可する N。60 の約数のうち、5 分以上かつ「人間が丸いと感じる」値。
/// 6 / 12 は 60 の約数だが DSL としては露出させない (#26 決定事項 5)。
const EVERY_ALLOWED_MINUTES: &[u32] = &[5, 10, 15, 20, 30];

/// `manifest.schedule` の意味論。すべて「絶対時刻の生成規則」であり、展開器がこれを
/// 具体的な時刻のタスクに変換する。実行時の発火判定には使われない (#26 決定事項 2)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Schedule {
    /// 毎時、`minutes` の各分に発火。昇順・重複なしが不変条件。
    /// `@hourly` → `[0]` / `@hourly :45` → `[45]` / `@every 15m` → `[0, 15, 30, 45]`。
    Hourly { minutes: Vec<u32> },
    /// 毎日 HH:MM に発火。
    Daily { hour: u32, minute: u32 },
    /// 毎週指定曜日の HH:MM に発火。
    Weekly {
        weekday: Weekday,
        hour: u32,
        minute: u32,
    },
    /// 毎月 D 日 HH:MM に発火。D が存在しない月 (2 月の 30 日等) は skip。
    Monthly { day: u32, hour: u32, minute: u32 },
    /// 特定日時に 1 回だけ発火。TZ は manifest.tz (省略時 user local) で解釈される。
    At { datetime: NaiveDateTime },
}

/// `manifest.schedule` DSL のエントリ。
///
/// 旧 interval 形式 (`"5m"` / `"1h"`) は 0.2.0 で廃止されたが、単に
/// "unknown keyword" で落とすとエージェント開発者が移行先を推測できない。
/// 数字始まりを検出したら移行先を名指しするエラーを返す。
pub(crate) fn parse_schedule(s: &str) -> Result<Schedule, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(format!("empty schedule: '{s}'"));
    }
    if !trimmed.starts_with('@') {
        if looks_like_legacy_interval(trimmed) {
            return Err(format!(
                "interval schedule '{trimmed}' was removed in 0.2.0; \
                 use '@every 5m' (5/10/15/20/30) for sub-hourly, or '@hourly' / '@daily HH:MM'"
            ));
        }
        return Err(format!(
            "schedule must start with '@' (e.g. '@hourly', '@daily 09:00'), got '{trimmed}'"
        ));
    }

    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap(); // starts_with('@') 済みなので必ず取れる
    let rest = parts.next().unwrap_or("").trim();
    match head {
        "@hourly" => parse_hourly(rest, trimmed),
        "@every" => parse_every(rest, trimmed),
        "@daily" => {
            let (h, m) = parse_hhmm(rest, trimmed)?;
            Ok(Schedule::Daily { hour: h, minute: m })
        }
        "@weekly" => {
            let mut sp = rest.splitn(2, char::is_whitespace);
            let wd_s = sp
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("@weekly missing weekday in '{trimmed}'"))?;
            let time_s = sp
                .next()
                .ok_or_else(|| format!("@weekly missing time in '{trimmed}'"))?
                .trim();
            let weekday = parse_weekday(wd_s, trimmed)?;
            let (h, m) = parse_hhmm(time_s, trimmed)?;
            Ok(Schedule::Weekly {
                weekday,
                hour: h,
                minute: m,
            })
        }
        "@monthly" => {
            let mut sp = rest.splitn(2, char::is_whitespace);
            let d_s = sp
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("@monthly missing day in '{trimmed}'"))?;
            let time_s = sp
                .next()
                .ok_or_else(|| format!("@monthly missing time in '{trimmed}'"))?
                .trim();
            let d: u32 = d_s
                .parse()
                .map_err(|_| format!("@monthly invalid day '{d_s}' in '{trimmed}'"))?;
            if !(1..=31).contains(&d) {
                return Err(format!(
                    "@monthly day out of range (1-31): '{d}' in '{trimmed}'"
                ));
            }
            let (h, m) = parse_hhmm(time_s, trimmed)?;
            Ok(Schedule::Monthly {
                day: d,
                hour: h,
                minute: m,
            })
        }
        "@at" => {
            if rest.is_empty() {
                return Err(format!("@at missing datetime in '{trimmed}'"));
            }
            let dt = NaiveDateTime::parse_from_str(rest, "%Y-%m-%dT%H:%M")
                .map_err(|e| format!("@at invalid datetime '{rest}' in '{trimmed}': {e}"))?;
            Ok(Schedule::At { datetime: dt })
        }
        other => Err(format!(
            "unknown schedule keyword '{other}' in '{trimmed}' \
             (expected @hourly / @every / @daily / @weekly / @monthly / @at)"
        )),
    }
}

/// 旧 interval 形式 (`5m` / `1h` / `10s`) に見えるか。移行エラーメッセージの出し分け用。
fn looks_like_legacy_interval(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next_back() {
        Some('s' | 'm' | 'h') => {
            let num = chars.as_str();
            !num.is_empty() && num.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// `@hourly` (引数なし = :00) と `@hourly :MM` を扱う。
fn parse_hourly(rest: &str, orig: &str) -> Result<Schedule, String> {
    if rest.is_empty() {
        return Ok(Schedule::Hourly { minutes: vec![0] });
    }
    let mm = rest.strip_prefix(':').ok_or_else(|| {
        format!("@hourly argument must be ':MM' (e.g. '@hourly :45'), got '{rest}' in '{orig}'")
    })?;
    // カンマ区切りリストは意図的に非サポート。誤用に対して意図を明示して落とす。
    if mm.contains(',') {
        return Err(format!(
            "@hourly takes a single ':MM'; comma lists are not supported \
             (use '@every Nm' for an evenly-spaced set) in '{orig}'"
        ));
    }
    let m: u32 = mm
        .parse()
        .map_err(|_| format!("@hourly invalid minute '{mm}' in '{orig}'"))?;
    if m >= 60 {
        return Err(format!(
            "@hourly minute out of range (0-59): '{m}' in '{orig}'"
        ));
    }
    Ok(Schedule::Hourly { minutes: vec![m] })
}

/// `@every <N>m` を毎時の分集合に展開する。N は [`EVERY_ALLOWED_MINUTES`] のみ。
fn parse_every(rest: &str, orig: &str) -> Result<Schedule, String> {
    if rest.is_empty() {
        return Err(format!("@every missing interval in '{orig}'"));
    }
    let num = rest.strip_suffix('m').ok_or_else(|| {
        format!("@every interval must be in minutes (e.g. '@every 10m'), got '{rest}' in '{orig}'")
    })?;
    let n: u32 = num
        .parse()
        .map_err(|_| format!("@every invalid interval '{rest}' in '{orig}'"))?;
    if !EVERY_ALLOWED_MINUTES.contains(&n) {
        return Err(format!(
            "@every interval must be one of {EVERY_ALLOWED_MINUTES:?} minutes \
             (a divisor of 60, so each hour expands identically), got '{n}m' in '{orig}'"
        ));
    }
    // n は 60 の約数なので 0 から n 刻みで必ず 60 未満に収まる。
    let minutes = (0..60).step_by(n as usize).collect();
    Ok(Schedule::Hourly { minutes })
}

fn parse_hhmm(s: &str, orig: &str) -> Result<(u32, u32), String> {
    let (h_s, m_s) = s
        .split_once(':')
        .ok_or_else(|| format!("expected HH:MM in '{orig}', got '{s}'"))?;
    let h: u32 = h_s
        .parse()
        .map_err(|_| format!("invalid hour '{h_s}' in '{orig}'"))?;
    let m: u32 = m_s
        .parse()
        .map_err(|_| format!("invalid minute '{m_s}' in '{orig}'"))?;
    if h >= 24 {
        return Err(format!("hour out of range (0-23): '{h}' in '{orig}'"));
    }
    if m >= 60 {
        return Err(format!("minute out of range (0-59): '{m}' in '{orig}'"));
    }
    Ok((h, m))
}

fn parse_weekday(s: &str, orig: &str) -> Result<Weekday, String> {
    match s {
        "MON" => Ok(Weekday::Mon),
        "TUE" => Ok(Weekday::Tue),
        "WED" => Ok(Weekday::Wed),
        "THU" => Ok(Weekday::Thu),
        "FRI" => Ok(Weekday::Fri),
        "SAT" => Ok(Weekday::Sat),
        "SUN" => Ok(Weekday::Sun),
        _ => Err(format!(
            "invalid weekday '{s}' (expected MON/TUE/WED/THU/FRI/SAT/SUN) in '{orig}'"
        )),
    }
}

/// manifest.tz を解決する。省略時は OS TZ (user local) を IANA name として取得。
/// 解決失敗はエラー (discovery で reject される)。
///
/// user local 検出の優先順:
/// 1. `TZ` 環境変数 (POSIX 標準)。IANA name としてパース可能ならこれを採用
/// 2. [`iana_time_zone::get_timezone`] (Linux では `/etc/localtime` を辿る OS の設定)
///
/// **TZ env を優先する理由**: dev container のように `/etc/localtime` が `Etc/UTC` のまま
/// `containerEnv.TZ=Asia/Tokyo` だけ立っている環境が現実に存在する (この repo 自体がそう)。
/// この状態で iana-time-zone だけ見ると UTC が返り、`@daily 13:35` は 22:35 JST に fire する
/// という気付きにくいバグを踏む。ユーザーが env で明示的に指定したなら尊重するのが自然。
pub(crate) fn resolve_tz(name: Option<&str>) -> Result<Tz, String> {
    if let Some(n) = name {
        return n
            .parse::<Tz>()
            .map_err(|e| format!("invalid tz '{n}': {e}"));
    }
    if let Ok(tz_env) = std::env::var("TZ") {
        if !tz_env.is_empty() {
            if let Ok(tz) = tz_env.parse::<Tz>() {
                return Ok(tz);
            }
            // TZ env が POSIX 文字列 (`JST-9` 等) の場合は IANA 名として parse できない。
            // その場合は iana_time_zone にフォールバック (log は出さない: 正常経路)。
        }
    }
    let sys = iana_time_zone::get_timezone().map_err(|e| format!("cannot detect user tz: {e}"))?;
    sys.parse::<Tz>()
        .map_err(|e| format!("system tz '{sys}' not in chrono-tz DB: {e}"))
}

/// 「`after_ms` より厳密に後にある最も早い予定時刻」を返す (ms since epoch, UTC)。
///
/// 展開器はこれを境界時刻から繰り返し呼んでホライズンまでのタスクを生成する
/// (#26 決定事項 2)。`list_triggers` の表示用に単発で呼ぶ経路もある。
///
/// 戻り値 `None` は「以降 fire することがない」を意味する (現状 `@at` の期日が past、
/// または探索窓を越えた場合のみ)。
///
/// DST 実装:
/// - Ambiguous (fall-back) は earlier (最初の UTC 出現) を採用。2 回目は "厳密に後" 条件で
///   自然に skip される (2 回目の UTC 時刻 > 1 回目の UTC 時刻 なので、1 回目を after とした
///   クエリで 2 回目が返ることは無い)
/// - None (spring-forward) は該当日 / 該当時刻を飛ばして次候補を探索
pub(crate) fn next_scheduled_after(after_ms: u64, spec: &Schedule, tz: &Tz) -> Option<u64> {
    // now_millis() が u64 なので、`as i64` だと u64::MAX 近辺の値が負値に化ける。
    // 現実的な運用では発生しないが、破損した state store から巨大な値を読んだ場合の
    // silent skip を避けるため try_from で早期に None に落とす (Issue #21 #15)。
    let after_i64 = i64::try_from(after_ms).ok()?;
    let after_utc = DateTime::<chrono::Utc>::from_timestamp_millis(after_i64)?;
    let local_after = after_utc.with_timezone(tz);

    match spec {
        Schedule::Hourly { minutes } => {
            // 現在ローカル時刻の :00 (切り捨て) を起点に、1 時間ずつ進めながら
            // 各時間の分集合を昇順に見る。分集合は parse 時点で昇順なので、
            // 最初に "after より後" を満たしたものが最も早い予定時刻になる。
            let mut hour_start = local_after
                .date_naive()
                .and_hms_opt(local_after.hour(), 0, 0)?;
            for _ in 0..(24 * 400) {
                for m in minutes {
                    let naive = hour_start.with_minute(*m)?;
                    if let Some(ms) = pick_earlier_utc(tz, &naive) {
                        if ms > after_ms {
                            return Some(ms);
                        }
                    }
                }
                hour_start = hour_start.checked_add_signed(chrono::TimeDelta::hours(1))?;
            }
            None
        }
        Schedule::Daily { hour, minute } => {
            let mut date = local_after.date_naive();
            for _ in 0..400 {
                if let Some(naive) = date.and_hms_opt(*hour, *minute, 0) {
                    if let Some(ms) = pick_earlier_utc(tz, &naive) {
                        if ms > after_ms {
                            return Some(ms);
                        }
                    }
                }
                date = date.succ_opt()?;
            }
            None
        }
        Schedule::Weekly {
            weekday,
            hour,
            minute,
        } => {
            let mut date = local_after.date_naive();
            for _ in 0..400 {
                if date.weekday() == *weekday {
                    if let Some(naive) = date.and_hms_opt(*hour, *minute, 0) {
                        if let Some(ms) = pick_earlier_utc(tz, &naive) {
                            if ms > after_ms {
                                return Some(ms);
                            }
                        }
                    }
                }
                date = date.succ_opt()?;
            }
            None
        }
        Schedule::Monthly { day, hour, minute } => {
            let mut year = local_after.year();
            let mut month = local_after.month();
            // 20 年 (240 ヶ月) 分回せば @monthly 31 の運用でも十分先まで到達できる。
            for _ in 0..(12 * 20) {
                if let Some(date) = NaiveDate::from_ymd_opt(year, month, *day) {
                    if let Some(naive) = date.and_hms_opt(*hour, *minute, 0) {
                        if let Some(ms) = pick_earlier_utc(tz, &naive) {
                            if ms > after_ms {
                                return Some(ms);
                            }
                        }
                    }
                }
                if month == 12 {
                    month = 1;
                    year += 1;
                } else {
                    month += 1;
                }
            }
            None
        }
        Schedule::At { datetime } => {
            let ms = pick_earlier_utc(tz, datetime)?;
            if ms > after_ms {
                Some(ms)
            } else {
                None
            }
        }
    }
}

/// NaiveDateTime (local wall-clock) を tz で解決して ms since epoch (UTC) を返す。
/// Ambiguous (fall-back) は earlier を採用、None (spring-forward gap) は None を返す。
/// pre-epoch (1970 以前) は返り値 u64 に載らないので None (Issue #21 #10)。
fn pick_earlier_utc(tz: &Tz, naive: &NaiveDateTime) -> Option<u64> {
    let dt = match tz.from_local_datetime(naive) {
        MappedLocalTime::Single(dt) => dt,
        MappedLocalTime::Ambiguous(earlier, _) => earlier,
        MappedLocalTime::None => return None,
    };
    u64::try_from(dt.timestamp_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tz_utc() -> Tz {
        "UTC".parse().unwrap()
    }

    fn tz_tokyo() -> Tz {
        "Asia/Tokyo".parse().unwrap()
    }

    fn tz_la() -> Tz {
        "America/Los_Angeles".parse().unwrap()
    }

    fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .single()
            .unwrap()
            .timestamp_millis() as u64
    }

    // ---- interval 廃止 (#26 決定事項 4) ----

    #[test]
    fn legacy_interval_is_rejected_with_migration_hint() {
        for s in ["10s", "5m", "1h", "6h"] {
            let err = parse_schedule(s).unwrap_err();
            assert!(
                err.contains("removed in 0.2.0"),
                "'{s}' should name the migration: {err}"
            );
            assert!(
                err.contains("@every 5m"),
                "'{s}' should point at @every: {err}"
            );
        }
    }

    #[test]
    fn non_at_sign_is_rejected() {
        let err = parse_schedule("daily 09:00").unwrap_err();
        assert!(err.contains("must start with '@'"), "{err}");
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("💾").is_err()); // multi-byte, no panic
    }

    // ---- @hourly / @every (#26 決定事項 5) ----

    #[test]
    fn parse_hourly_bare_is_minute_zero() {
        assert_eq!(
            parse_schedule("@hourly").unwrap(),
            Schedule::Hourly { minutes: vec![0] }
        );
        assert_eq!(
            parse_schedule(" @hourly ").unwrap(),
            Schedule::Hourly { minutes: vec![0] }
        );
    }

    #[test]
    fn parse_hourly_with_minute() {
        assert_eq!(
            parse_schedule("@hourly :45").unwrap(),
            Schedule::Hourly { minutes: vec![45] }
        );
        assert_eq!(
            parse_schedule("@hourly :00").unwrap(),
            Schedule::Hourly { minutes: vec![0] }
        );
    }

    #[test]
    fn parse_hourly_rejects_bad_args() {
        assert!(parse_schedule("@hourly 45").is_err()); // ':' 無し
        assert!(parse_schedule("@hourly :60").is_err());
        assert!(parse_schedule("@hourly :xx").is_err());
    }

    // カンマ区切りリストは DSL に露出させない (cron への滑りを防ぐ)。
    // 誤用時に「非サポート」と名指しで落ちることを保証する。
    #[test]
    fn parse_hourly_rejects_comma_list() {
        let err = parse_schedule("@hourly :00,:15,:45").unwrap_err();
        assert!(err.contains("comma lists are not supported"), "{err}");
    }

    #[test]
    fn parse_every_expands_to_minute_set() {
        assert_eq!(
            parse_schedule("@every 10m").unwrap(),
            Schedule::Hourly {
                minutes: vec![0, 10, 20, 30, 40, 50]
            }
        );
        assert_eq!(
            parse_schedule("@every 15m").unwrap(),
            Schedule::Hourly {
                minutes: vec![0, 15, 30, 45]
            }
        );
        assert_eq!(
            parse_schedule("@every 30m").unwrap(),
            Schedule::Hourly {
                minutes: vec![0, 30]
            }
        );
        // 下限。288 件/日を生成する。
        assert_eq!(
            parse_schedule("@every 5m").unwrap(),
            Schedule::Hourly {
                minutes: (0..60).step_by(5).collect()
            }
        );
    }

    // 60 の約数でない値、および約数でも allowlist 外 (6 / 12) を拒否する。
    // allowlist に絞る理由は「毎時同じ分集合になる」ことと DSL の丸さの両立 (#26)。
    #[test]
    fn parse_every_rejects_non_allowlisted() {
        for s in ["@every 7m", "@every 6m", "@every 12m", "@every 60m"] {
            let err = parse_schedule(s).unwrap_err();
            assert!(err.contains("must be one of"), "'{s}': {err}");
        }
        assert!(parse_schedule("@every 4m").is_err());
        assert!(parse_schedule("@every 1h").is_err()); // 分単位のみ
        assert!(parse_schedule("@every").is_err());
    }

    // ---- 既存 DSL の回帰 ----

    #[test]
    fn parse_daily() {
        assert_eq!(
            parse_schedule("@daily 09:00").unwrap(),
            Schedule::Daily { hour: 9, minute: 0 }
        );
        assert!(parse_schedule("@daily").is_err());
        assert!(parse_schedule("@daily 25:00").is_err());
        assert!(parse_schedule("@daily 09:99").is_err());
        assert!(parse_schedule("@daily 9").is_err());
    }

    #[test]
    fn parse_weekly() {
        assert_eq!(
            parse_schedule("@weekly MON 09:00").unwrap(),
            Schedule::Weekly {
                weekday: Weekday::Mon,
                hour: 9,
                minute: 0
            }
        );
        assert!(parse_schedule("@weekly Mon 09:00").is_err()); // 小文字は拒否
        assert!(parse_schedule("@weekly XYZ 09:00").is_err());
        assert!(parse_schedule("@weekly MON").is_err());
    }

    #[test]
    fn parse_monthly() {
        assert_eq!(
            parse_schedule("@monthly 15 09:00").unwrap(),
            Schedule::Monthly {
                day: 15,
                hour: 9,
                minute: 0
            }
        );
        assert!(parse_schedule("@monthly 0 09:00").is_err());
        assert!(parse_schedule("@monthly 32 09:00").is_err());
        assert!(parse_schedule("@monthly 15").is_err());
    }

    #[test]
    fn parse_at() {
        match parse_schedule("@at 2026-08-01T18:30").unwrap() {
            Schedule::At { datetime } => {
                assert_eq!(datetime.year(), 2026);
                assert_eq!(datetime.month(), 8);
                assert_eq!(datetime.day(), 1);
                assert_eq!(datetime.hour(), 18);
                assert_eq!(datetime.minute(), 30);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(parse_schedule("@at").is_err());
        assert!(parse_schedule("@at 2026-08-01 18:30").is_err()); // T が無い
        assert!(parse_schedule("@at bogus").is_err());
    }

    #[test]
    fn parse_unknown_keyword() {
        assert!(parse_schedule("@yearly").is_err());
        assert!(parse_schedule("@").is_err());
    }

    // ---- next_scheduled_after: 基本ケース ----

    #[test]
    fn hourly_utc() {
        let spec = Schedule::Hourly { minutes: vec![0] };
        let tz = tz_utc();
        // 12:30 → 次は 13:00
        let after = utc_ms(2026, 3, 1, 12, 30);
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 13, 0)
        );
        // ちょうど 13:00 → 次は 14:00 (厳密に後)
        let after2 = utc_ms(2026, 3, 1, 13, 0);
        assert_eq!(
            next_scheduled_after(after2, &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 14, 0)
        );
    }

    // @hourly :45 は毎時 :45 のみ。:00 を返してはいけない。
    #[test]
    fn hourly_with_minute_offset() {
        let spec = Schedule::Hourly { minutes: vec![45] };
        let tz = tz_utc();
        // 12:30 → 同じ時間の 12:45
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 30), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 12, 45)
        );
        // 12:45 ちょうど → 翌時 13:45
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 45), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 13, 45)
        );
        // 12:50 → 翌時 13:45 (:00 に戻らない)
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 50), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 13, 45)
        );
    }

    // @every は分集合を昇順に走査し、時間をまたぐときに :00 へ正しく戻る。
    #[test]
    fn hourly_minute_set_walks_and_wraps() {
        let spec = parse_schedule("@every 20m").unwrap(); // :00, :20, :40
        let tz = tz_utc();
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 0), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 12, 20)
        );
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 20), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 12, 40)
        );
        // :40 の次は翌時 :00 に wrap する
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 40), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 13, 0)
        );
        // 分集合の隙間 (12:41) からでも次は 13:00
        assert_eq!(
            next_scheduled_after(utc_ms(2026, 3, 1, 12, 41), &spec, &tz).unwrap(),
            utc_ms(2026, 3, 1, 13, 0)
        );
    }

    // 展開器の使い方 (境界から繰り返し呼ぶ) が正しい列を生むことを保証する。
    // これは #26 決定事項 2 の生成器としての契約そのもの。
    #[test]
    fn hourly_minute_set_generates_dense_sequence() {
        let spec = parse_schedule("@every 30m").unwrap();
        let tz = tz_utc();
        let mut cursor = utc_ms(2026, 3, 1, 12, 0);
        let mut generated = Vec::new();
        for _ in 0..4 {
            cursor = next_scheduled_after(cursor, &spec, &tz).unwrap();
            generated.push(cursor);
        }
        assert_eq!(
            generated,
            vec![
                utc_ms(2026, 3, 1, 12, 30),
                utc_ms(2026, 3, 1, 13, 0),
                utc_ms(2026, 3, 1, 13, 30),
                utc_ms(2026, 3, 1, 14, 0),
            ]
        );
    }

    #[test]
    fn daily_tokyo_next_is_today() {
        // JST 08:00 (UTC 前日 23:00) から @daily 09:00 → JST 当日 09:00 (UTC 00:00)
        let spec = Schedule::Daily { hour: 9, minute: 0 };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 18, 23, 0); // JST 2026-07-19 08:00
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 7, 19, 0, 0)
        );
    }

    #[test]
    fn daily_tokyo_missed_becomes_tomorrow() {
        // JST 10:00 から @daily 09:00 → 翌日 09:00
        let spec = Schedule::Daily { hour: 9, minute: 0 };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 1, 0); // JST 2026-07-19 10:00
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 7, 20, 0, 0)
        );
    }

    #[test]
    fn weekly_next_monday() {
        let spec = Schedule::Weekly {
            weekday: Weekday::Mon,
            hour: 9,
            minute: 0,
        };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 1, 0); // JST 2026-07-19 (日) 10:00
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 7, 20, 0, 0)
        );
    }

    #[test]
    fn monthly_skips_missing_day() {
        // @monthly 31 09:00 in Tokyo: 2026-02 には 31 日が無いので 2026-03-31 に飛ぶ
        let spec = Schedule::Monthly {
            day: 31,
            hour: 9,
            minute: 0,
        };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 1, 31, 15, 0); // JST 2026-02-01 00:00
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 3, 31, 0, 0)
        );
    }

    #[test]
    fn at_returns_target_when_future() {
        let spec = Schedule::At {
            datetime: NaiveDate::from_ymd_opt(2026, 8, 1)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
        };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 0, 0);
        // JST 2026-08-01 18:30 = UTC 2026-08-01 09:30
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 8, 1, 9, 30)
        );
    }

    #[test]
    fn at_returns_none_when_past() {
        let spec = Schedule::At {
            datetime: NaiveDate::from_ymd_opt(2026, 7, 1)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
        };
        let tz = tz_tokyo();
        assert!(next_scheduled_after(utc_ms(2026, 7, 19, 0, 0), &spec, &tz).is_none());
    }

    // ---- DST semantics (America/Los_Angeles) ----

    #[test]
    fn dst_spring_forward_skips_nonexistent_time() {
        // 2026-03-08 の LA は 02:00 → 03:00 (02:30 は存在しない)
        let spec = Schedule::Daily {
            hour: 2,
            minute: 30,
        };
        let tz = tz_la();
        let after = utc_ms(2026, 3, 8, 7, 0); // LA 2026-03-07 23:00 PST
                                              // 2026-03-08 02:30 は存在しない → 2026-03-09 02:30 PDT (UTC-7)
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 3, 9, 9, 30)
        );
    }

    // spring-forward の欠落時間は @hourly でも飛ばされる。LA の 2026-03-08 は
    // 02:00-02:59 が存在しないので、01:00 の次は 03:00 になる。
    #[test]
    fn dst_spring_forward_skips_missing_hour_for_hourly() {
        let spec = Schedule::Hourly { minutes: vec![0] };
        let tz = tz_la();
        // LA 2026-03-08 01:00 PST (UTC-8) = UTC 09:00
        let after = utc_ms(2026, 3, 8, 9, 0);
        // 次は LA 03:00 PDT (UTC-7) = UTC 10:00 (02:00 は存在しない)
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 3, 8, 10, 0)
        );
    }

    #[test]
    fn dst_fall_back_fires_earlier_occurrence() {
        // 2026-11-01 LA: 02:00 → 01:00 (01:00-01:59 が 2 回来る)
        let spec = Schedule::Daily {
            hour: 1,
            minute: 30,
        };
        let tz = tz_la();
        let after = utc_ms(2026, 11, 1, 6, 0); // LA 2026-10-31 23:00 PDT
                                               // 1 回目 (PDT UTC-7) = UTC 2026-11-01 08:30
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 11, 1, 8, 30)
        );
    }

    #[test]
    fn dst_fall_back_second_occurrence_is_skipped() {
        let spec = Schedule::Daily {
            hour: 1,
            minute: 30,
        };
        let tz = tz_la();
        let after = utc_ms(2026, 11, 1, 8, 30); // 1 回目 fire 直後
                                                // 2 回目 (09:30 UTC) ではなく翌日 01:30 PST = UTC 2026-11-02 09:30
        assert_eq!(
            next_scheduled_after(after, &spec, &tz).unwrap(),
            utc_ms(2026, 11, 2, 9, 30)
        );
    }

    #[test]
    fn at_spring_forward_never_fires() {
        // 2026-03-08 の LA で @at 2026-03-08T02:30 → 存在しない時刻 → 永久 skip
        let spec = Schedule::At {
            datetime: NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(2, 30, 0)
                .unwrap(),
        };
        let tz = tz_la();
        assert!(next_scheduled_after(utc_ms(2026, 3, 1, 0, 0), &spec, &tz).is_none());
    }

    // ---- resolve_tz ----

    #[test]
    fn resolve_tz_explicit() {
        assert_eq!(
            resolve_tz(Some("Asia/Tokyo")).unwrap(),
            chrono_tz::Asia::Tokyo
        );
    }

    #[test]
    fn resolve_tz_rejects_bogus() {
        assert!(resolve_tz(Some("Not/A_Zone")).is_err());
    }

    // TZ env の優先を確認する。dev container の `/etc/localtime = UTC` + `TZ=Asia/Tokyo`
    // で app が UTC を採ってしまうバグに対する regression。
    #[test]
    fn resolve_tz_respects_tz_env() {
        let saved = std::env::var("TZ").ok();
        // SAFETY: shared env; tests within this file run in the same process.
        // 他 test は resolve_tz(None) を叩かないので競合はしない。
        unsafe { std::env::set_var("TZ", "Asia/Tokyo") };
        let tz = resolve_tz(None).unwrap();
        assert_eq!(tz, chrono_tz::Asia::Tokyo);
        match saved {
            Some(v) => unsafe { std::env::set_var("TZ", v) },
            None => unsafe { std::env::remove_var("TZ") },
        }
    }
}
