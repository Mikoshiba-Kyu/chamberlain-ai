//! Schedule DSL パーサと wall-clock 発火時刻計算 (#17 Phase 1 + #18 Phase 2)。
//!
//! `manifest.schedule` は 1 フィールドで 2 系統を扱う。先頭文字で判別する:
//! - 数字始まり (`"1h"` 等) → [`Schedule::Interval`] (Phase 1)
//! - `@` 始まり (`"@daily 09:00"` 等) → [`Schedule::WallClock`] (Phase 2)
//!
//! manifest フィールドを増やさない (エージェント開発者の学習コストを最小化する) 都合で
//! パース時点で分岐する形をとる。詳細な設計議論は #17 / #18 参照。
//!
//! TZ セマンティクス:
//! - デフォルトは user local (OS TZ を [`iana_time_zone`] で取得)
//! - `manifest.tz` に IANA name を書けば上書き
//! - Interval schedule は TZ 非依存 (経過時間ベース) なので `tz` は wall-clock のみ効く
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
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) enum Schedule {
    /// `"5m"` / `"1h"` 等。前回 fire から N 経過したら次 fire。
    Interval(Duration),
    /// `"@daily 09:00"` 等。TZ に紐付いた wall-clock 時刻で fire。
    WallClock(WallClockSpec),
}

#[derive(Debug, Clone)]
pub(crate) enum WallClockSpec {
    /// 毎時 :00 に fire。
    Hourly,
    /// 毎日 HH:MM に fire。
    Daily { hour: u32, minute: u32 },
    /// 毎週指定曜日の HH:MM に fire。
    Weekly {
        weekday: Weekday,
        hour: u32,
        minute: u32,
    },
    /// 毎月 D 日 HH:MM に fire。D が存在しない月 (2 月の 30 日等) は skip。
    Monthly { day: u32, hour: u32, minute: u32 },
    /// 特定日時に 1 回だけ fire。TZ は manifest.tz (省略時 user local) で解釈される。
    /// 発火後は永久 skip。起動時に既に過ぎていた場合も skip (missed-fire policy)。
    At { datetime: NaiveDateTime },
}

/// `manifest.schedule` DSL 全体のエントリ。
pub(crate) fn parse_schedule(s: &str) -> Result<Schedule, String> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(format!("empty schedule: '{s}'"));
    }
    if trimmed.starts_with('@') {
        parse_wall_clock(trimmed).map(Schedule::WallClock)
    } else {
        parse_interval(trimmed).map(Schedule::Interval)
    }
}

/// Phase 1 の interval DSL。`5m` / `1h` / `10s` の形式のみ。
/// 単位無し・複合単位 (`1h30m`) は非対応 (wall-clock DSL に譲る)。
fn parse_interval(s: &str) -> Result<Duration, String> {
    // char 単位で末尾を切り分ける (byte offset だと multi-byte char で panic する)
    let mut chars = s.chars();
    let unit = chars
        .next_back()
        .ok_or_else(|| format!("empty schedule: '{s}'"))?;
    let num_part = chars.as_str();
    if num_part.is_empty() {
        return Err(format!("schedule missing number: '{s}'"));
    }
    let n: u64 = num_part
        .parse()
        .map_err(|_| format!("invalid schedule number in '{s}'"))?;
    if n == 0 {
        return Err(format!("schedule must be positive: '{s}'"));
    }
    let secs = match unit {
        's' => n,
        'm' => n
            .checked_mul(60)
            .ok_or_else(|| format!("schedule overflow: '{s}'"))?,
        'h' => n
            .checked_mul(60 * 60)
            .ok_or_else(|| format!("schedule overflow: '{s}'"))?,
        other => return Err(format!("unknown schedule unit '{other}' in '{s}'")),
    };
    Ok(Duration::from_secs(secs))
}

fn parse_wall_clock(s: &str) -> Result<WallClockSpec, String> {
    let mut parts = s.splitn(2, char::is_whitespace);
    let head = parts.next().unwrap(); // starts_with('@') 済みなので必ず取れる
    let rest = parts.next().unwrap_or("").trim();
    match head {
        "@hourly" => {
            if !rest.is_empty() {
                return Err(format!("@hourly takes no args, got '{rest}' in '{s}'"));
            }
            Ok(WallClockSpec::Hourly)
        }
        "@daily" => {
            let (h, m) = parse_hhmm(rest, s)?;
            Ok(WallClockSpec::Daily { hour: h, minute: m })
        }
        "@weekly" => {
            let mut sp = rest.splitn(2, char::is_whitespace);
            let wd_s = sp
                .next()
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("@weekly missing weekday in '{s}'"))?;
            let time_s = sp
                .next()
                .ok_or_else(|| format!("@weekly missing time in '{s}'"))?
                .trim();
            let weekday = parse_weekday(wd_s, s)?;
            let (h, m) = parse_hhmm(time_s, s)?;
            Ok(WallClockSpec::Weekly {
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
                .ok_or_else(|| format!("@monthly missing day in '{s}'"))?;
            let time_s = sp
                .next()
                .ok_or_else(|| format!("@monthly missing time in '{s}'"))?
                .trim();
            let d: u32 = d_s
                .parse()
                .map_err(|_| format!("@monthly invalid day '{d_s}' in '{s}'"))?;
            if !(1..=31).contains(&d) {
                return Err(format!("@monthly day out of range (1-31): '{d}' in '{s}'"));
            }
            let (h, m) = parse_hhmm(time_s, s)?;
            Ok(WallClockSpec::Monthly {
                day: d,
                hour: h,
                minute: m,
            })
        }
        "@at" => {
            if rest.is_empty() {
                return Err(format!("@at missing datetime in '{s}'"));
            }
            let dt = NaiveDateTime::parse_from_str(rest, "%Y-%m-%dT%H:%M")
                .map_err(|e| format!("@at invalid datetime '{rest}' in '{s}': {e}"))?;
            Ok(WallClockSpec::At { datetime: dt })
        }
        other => Err(format!("unknown wall-clock keyword '{other}' in '{s}'")),
    }
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

/// wall-clock schedule の「`after_ms` より厳密に後にある最も早い予定時刻」を返す (ms since epoch, UTC)。
///
/// 用途:
/// - worker の should_fire 判定: `next_scheduled_after(last_fire_at) <= now` なら fire
/// - list_triggers の nextFireAt: UI 表示用に `next_scheduled_after(now)` を返す
///
/// 戻り値 `None` は「以降 fire することがない」を意味する (現状 `@at` の期日が past のときのみ)。
///
/// DST 実装:
/// - Ambiguous (fall-back) は earlier (最初の UTC 出現) を採用。2 回目は "厳密に後" 条件で
///   自然に skip される (2 回目の UTC 時刻 > 1 回目の UTC 時刻 なので、1 回目を last としたクエリで
///   2 回目が返ることは無い)
/// - None (spring-forward) は該当日 / 該当時刻を飛ばして次候補を探索
pub(crate) fn next_scheduled_after(after_ms: u64, spec: &WallClockSpec, tz: &Tz) -> Option<u64> {
    // now_millis() が u64 なので、`as i64` だと u64::MAX 近辺の値が負値に化ける。
    // 現実的な運用では発生しないが、破損した state store から巨大な値を読んだ場合の
    // silent skip を避けるため try_from で早期に None に落とす (Issue #21 #15)。
    let after_i64 = i64::try_from(after_ms).ok()?;
    let after_utc = DateTime::<chrono::Utc>::from_timestamp_millis(after_i64)?;
    let local_after = after_utc.with_timezone(tz);

    match spec {
        WallClockSpec::Hourly => {
            // 現在ローカル時刻の :00 (切り捨て) から始めて 1 時間刻みで探索。
            // 「厳密に after より後」を要求するので、まず現在時刻の :00 を起点にして
            // 1 時間ずつ足しながらチェックする。
            let mut candidate = local_after
                .date_naive()
                .and_hms_opt(local_after.hour(), 0, 0)?;
            for _ in 0..(24 * 400) {
                candidate = candidate.checked_add_signed(chrono::TimeDelta::hours(1))?;
                if let Some(ms) = pick_earlier_utc(tz, &candidate) {
                    if ms > after_ms {
                        return Some(ms);
                    }
                }
            }
            None
        }
        WallClockSpec::Daily { hour, minute } => {
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
        WallClockSpec::Weekly {
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
        WallClockSpec::Monthly { day, hour, minute } => {
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
        WallClockSpec::At { datetime } => {
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

    // ---- parse_schedule (interval branch) ----

    #[test]
    fn parse_interval_basic() {
        assert!(matches!(
            parse_schedule("10s").unwrap(),
            Schedule::Interval(d) if d == Duration::from_secs(10)
        ));
        assert!(matches!(
            parse_schedule("5m").unwrap(),
            Schedule::Interval(d) if d == Duration::from_secs(300)
        ));
        assert!(matches!(
            parse_schedule("1h").unwrap(),
            Schedule::Interval(d) if d == Duration::from_secs(3600)
        ));
        assert!(matches!(
            parse_schedule(" 1h ").unwrap(),
            Schedule::Interval(d) if d == Duration::from_secs(3600)
        ));
    }

    #[test]
    fn parse_interval_rejects() {
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("m").is_err());
        assert!(parse_schedule("0m").is_err());
        assert!(parse_schedule("1d").is_err());
        assert!(parse_schedule("1h30m").is_err());
        assert!(parse_schedule("💾").is_err()); // multi-byte, no panic
    }

    // ---- parse_schedule (wall-clock branch) ----

    #[test]
    fn parse_wall_clock_hourly() {
        assert!(matches!(
            parse_schedule("@hourly").unwrap(),
            Schedule::WallClock(WallClockSpec::Hourly)
        ));
        assert!(parse_schedule("@hourly 09:00").is_err()); // 余分な引数
    }

    #[test]
    fn parse_wall_clock_daily() {
        match parse_schedule("@daily 09:00").unwrap() {
            Schedule::WallClock(WallClockSpec::Daily { hour, minute }) => {
                assert_eq!(hour, 9);
                assert_eq!(minute, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(parse_schedule("@daily").is_err());
        assert!(parse_schedule("@daily 25:00").is_err());
        assert!(parse_schedule("@daily 09:99").is_err());
        assert!(parse_schedule("@daily 9").is_err());
    }

    #[test]
    fn parse_wall_clock_weekly() {
        match parse_schedule("@weekly MON 09:00").unwrap() {
            Schedule::WallClock(WallClockSpec::Weekly {
                weekday,
                hour,
                minute,
            }) => {
                assert_eq!(weekday, Weekday::Mon);
                assert_eq!(hour, 9);
                assert_eq!(minute, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(parse_schedule("@weekly Mon 09:00").is_err()); // 小文字は拒否
        assert!(parse_schedule("@weekly XYZ 09:00").is_err());
        assert!(parse_schedule("@weekly MON").is_err());
    }

    #[test]
    fn parse_wall_clock_monthly() {
        match parse_schedule("@monthly 15 09:00").unwrap() {
            Schedule::WallClock(WallClockSpec::Monthly { day, hour, minute }) => {
                assert_eq!(day, 15);
                assert_eq!(hour, 9);
                assert_eq!(minute, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(parse_schedule("@monthly 0 09:00").is_err());
        assert!(parse_schedule("@monthly 32 09:00").is_err());
        assert!(parse_schedule("@monthly 15").is_err());
    }

    #[test]
    fn parse_wall_clock_at() {
        match parse_schedule("@at 2026-08-01T18:30").unwrap() {
            Schedule::WallClock(WallClockSpec::At { datetime }) => {
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
    fn parse_wall_clock_unknown_keyword() {
        assert!(parse_schedule("@yearly").is_err());
        assert!(parse_schedule("@").is_err());
    }

    // ---- next_scheduled_after: 基本ケース ----

    #[test]
    fn hourly_utc() {
        let spec = WallClockSpec::Hourly;
        let tz = tz_utc();
        // 12:30 → 次は 13:00
        let after = utc_ms(2026, 3, 1, 12, 30);
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        assert_eq!(next, utc_ms(2026, 3, 1, 13, 0));
        // ちょうど 13:00 → 次は 14:00 (厳密に後)
        let after2 = utc_ms(2026, 3, 1, 13, 0);
        let next2 = next_scheduled_after(after2, &spec, &tz).unwrap();
        assert_eq!(next2, utc_ms(2026, 3, 1, 14, 0));
    }

    #[test]
    fn daily_tokyo_next_is_today() {
        // JST 08:00 (UTC 前日 23:00) から @daily 09:00 → JST 当日 09:00 (UTC 00:00)
        let spec = WallClockSpec::Daily { hour: 9, minute: 0 };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 18, 23, 0); // JST 2026-07-19 08:00
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // JST 2026-07-19 09:00 = UTC 2026-07-19 00:00
        assert_eq!(next, utc_ms(2026, 7, 19, 0, 0));
    }

    #[test]
    fn daily_tokyo_missed_becomes_tomorrow() {
        // JST 10:00 から @daily 09:00 → 翌日 09:00
        let spec = WallClockSpec::Daily { hour: 9, minute: 0 };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 1, 0); // JST 2026-07-19 10:00
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // JST 2026-07-20 09:00 = UTC 2026-07-20 00:00
        assert_eq!(next, utc_ms(2026, 7, 20, 0, 0));
    }

    #[test]
    fn weekly_next_monday() {
        let spec = WallClockSpec::Weekly {
            weekday: Weekday::Mon,
            hour: 9,
            minute: 0,
        };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 1, 0); // JST 2026-07-19 (日) 10:00
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // JST 2026-07-20 (月) 09:00 = UTC 2026-07-20 00:00
        assert_eq!(next, utc_ms(2026, 7, 20, 0, 0));
    }

    #[test]
    fn monthly_skips_missing_day() {
        // @monthly 31 09:00 in Tokyo: 2026-02 には 31 日が無いので 2026-03-31 に飛ぶ
        let spec = WallClockSpec::Monthly {
            day: 31,
            hour: 9,
            minute: 0,
        };
        let tz = tz_tokyo();
        // JST 2026-02-01 00:00 (= UTC 2026-01-31 15:00) から
        let after = utc_ms(2026, 1, 31, 15, 0);
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // 2026-02-31 は無い → 2026-03-31 09:00 JST = UTC 2026-03-31 00:00
        assert_eq!(next, utc_ms(2026, 3, 31, 0, 0));
    }

    #[test]
    fn at_returns_target_when_future() {
        let spec = WallClockSpec::At {
            datetime: NaiveDate::from_ymd_opt(2026, 8, 1)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
        };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 0, 0);
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // JST 2026-08-01 18:30 = UTC 2026-08-01 09:30
        assert_eq!(next, utc_ms(2026, 8, 1, 9, 30));
    }

    #[test]
    fn at_returns_none_when_past() {
        let spec = WallClockSpec::At {
            datetime: NaiveDate::from_ymd_opt(2026, 7, 1)
                .unwrap()
                .and_hms_opt(18, 30, 0)
                .unwrap(),
        };
        let tz = tz_tokyo();
        let after = utc_ms(2026, 7, 19, 0, 0);
        assert!(next_scheduled_after(after, &spec, &tz).is_none());
    }

    // ---- DST semantics (America/Los_Angeles) ----

    #[test]
    fn dst_spring_forward_skips_nonexistent_time() {
        // 2026-03-08 の LA は 02:00 → 03:00 (02:30 は存在しない)
        // @daily 02:30 で 2026-03-07 の LA 深夜から探索 → 2026-03-08 は skip、2026-03-09 02:30 になる
        let spec = WallClockSpec::Daily {
            hour: 2,
            minute: 30,
        };
        let tz = tz_la();
        // LA 2026-03-07 23:00 PST (UTC-8) = UTC 2026-03-08 07:00
        let after = utc_ms(2026, 3, 8, 7, 0);
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // 2026-03-08 02:30 は存在しない → 2026-03-09 02:30 PDT (UTC-7) = UTC 2026-03-09 09:30
        assert_eq!(next, utc_ms(2026, 3, 9, 9, 30));
    }

    #[test]
    fn dst_fall_back_fires_earlier_occurrence() {
        // 2026-11-01 LA: 02:00 → 01:00 (01:00-01:59 が 2 回来る)
        // @daily 01:30 → 1 回目 (PDT UTC-7) を発火時刻とする
        let spec = WallClockSpec::Daily {
            hour: 1,
            minute: 30,
        };
        let tz = tz_la();
        // LA 2026-10-31 23:00 PDT (UTC-7) = UTC 2026-11-01 06:00
        let after = utc_ms(2026, 11, 1, 6, 0);
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // 1 回目 (PDT UTC-7) = UTC 2026-11-01 08:30
        assert_eq!(next, utc_ms(2026, 11, 1, 8, 30));
    }

    #[test]
    fn dst_fall_back_second_occurrence_is_skipped() {
        // 1 回目 (08:30 UTC) を last とした次クエリでは、2 回目 (09:30 UTC) ではなく
        // 翌日 01:30 (PST UTC-8) = UTC 2026-11-02 09:30 が返らないといけない
        let spec = WallClockSpec::Daily {
            hour: 1,
            minute: 30,
        };
        let tz = tz_la();
        let after = utc_ms(2026, 11, 1, 8, 30); // 1 回目 fire 直後
        let next = next_scheduled_after(after, &spec, &tz).unwrap();
        // 翌日 01:30 PST = UTC 2026-11-02 09:30 (day+25h から、DST 終わったので 8h→9h)
        assert_eq!(next, utc_ms(2026, 11, 2, 9, 30));
    }

    #[test]
    fn at_spring_forward_never_fires() {
        // 2026-03-08 の LA で @at 2026-03-08T02:30 → 存在しない時刻 → 永久 skip
        let spec = WallClockSpec::At {
            datetime: NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(2, 30, 0)
                .unwrap(),
        };
        let tz = tz_la();
        let after = utc_ms(2026, 3, 1, 0, 0);
        assert!(next_scheduled_after(after, &spec, &tz).is_none());
    }

    // ---- resolve_tz ----

    #[test]
    fn resolve_tz_explicit() {
        let tz = resolve_tz(Some("Asia/Tokyo")).unwrap();
        assert_eq!(tz, chrono_tz::Asia::Tokyo);
    }

    #[test]
    fn resolve_tz_rejects_bogus() {
        assert!(resolve_tz(Some("Not/A_Zone")).is_err());
    }

    // TZ env の優先を確認する。dev container の `/etc/localtime = UTC` + `TZ=Asia/Tokyo`
    // で app が UTC を採ってしまうバグに対する regression。
    // TZ env は test 実行環境自体に副作用があるので、他のテストと並行しないよう
    // `#[cfg(test)]` 側でシリアライズさせる方法もあるが、
    // ここでは単発 test で set → assert → restore を素直に行う。
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
