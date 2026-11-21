//! 결정론적 시간 해석 (logis-center `utils/time_guide.rs` 이식)
//!
//! **모델이 낸 날짜는 믿지 않는다.** 질의 해석(모델이든 사전이든)은 기간의 *의도*만 낸다: `yesterday`, `last_month`,
//! `this_week`, 계절 `summer` 같은 키다. 실제 범위(`start_ms` ~ `end_ms`)는 이 모듈이 현재 시각과 사용자의 시계(언어 → 시간대,
//! 남반구 여부)로 다시 계산한다. 그래서 같은 질의는 몇 번을 물어도 같은 범위가 된다.
//!
//! 현재 시각은 [`now_ms`] 다. 평가 · 시험이 시각을 고정할 수 있게 [`EVAL_NOW_ENV`](없으면 logis-center 의
//! [`LEGACY_EVAL_NOW_ENV`])를 먼저 읽는다. 계산 함수는 모두 `now_ms` 를 인자로 받는 순수 함수다.
//!
//! logis-center 와 다른 점:
//! - `this_week` · `last_week` 를 더했다(ISO 주, 월요일 시작). logis-center 에는 없었다.
//! - 커머스 쿠폰 전용 조건(`started_at` · `expired_at`, 벽시계를 UTC 로 저장한 값)은 옮기지 않았다. 웹 분석 사건의 시각은
//!   진짜 epoch ms 라서 [`period_ms`] 가 사용자 시간대의 하루 경계로 바로 자른다.
//! - 기간 비교([`resolve_comparison_window`], 시나리오 5): [`resolve`] 는 `this_*` 를 그 기간 전체로 두지만(예전과 같다), 비교할
//!   때 진행 중인 기간은 지금까지 지난 시간만큼만 보고 앞 기간도 같은 길이로 자른다. 화요일 13시의 "이번 주 vs 지난주" 는
//!   월요일 0시부터 +37시간끼리 견준다.

use chrono::{Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// 평가 · 시험이 현재 시각을 고정하는 환경 변수 (epoch ms)
pub const EVAL_NOW_ENV: &str = "PD_EVAL_NOW_MS";
/// logis-center 의 같은 변수. [`EVAL_NOW_ENV`] 가 없을 때 읽는다
pub const LEGACY_EVAL_NOW_ENV: &str = "LOGIS_EVAL_NOW_MS";
/// `recently` 의 길이 (오늘 포함 앞으로 30일)
pub const RECENT_DAYS: i64 = 30;
const DAY_MS: i64 = 86_400_000;

/// 해석하는 기간 키. 사전 · 모델이 낸 키는 이 목록 밖이면 버린다
pub const TIME_KEYS: [&str; 9] = ["today", "yesterday", "this_week", "last_week", "this_month", "last_month", "this_year", "last_year", "recently"];
/// 계절 키
pub const SEASON_KEYS: [&str; 4] = ["spring", "summer", "autumn", "winter"];

/// 현재 시각 (UTC epoch ms). [`EVAL_NOW_ENV`] · [`LEGACY_EVAL_NOW_ENV`] 가 있으면 그 값 (프로세스에서 처음 읽은 값으로 고정)
pub fn now_ms() -> i64 {
    static PIN: std::sync::OnceLock<Option<i64>> = std::sync::OnceLock::new();
    PIN.get_or_init(|| {
        [EVAL_NOW_ENV, LEGACY_EVAL_NOW_ENV]
            .iter()
            .find_map(|k| std::env::var(k).ok().and_then(|v| v.trim().parse::<i64>().ok()).filter(|v| *v > 0))
    })
    .unwrap_or_else(|| Utc::now().timestamp_millis())
}

const LANG_NAME_CODES: &[(&str, &str)] = &[
    ("korean", "ko"), ("한국어", "ko"), ("한국", "ko"),
    ("japanese", "ja"), ("日本語", "ja"),
    ("chinese", "zh"), ("中文", "zh"), ("汉语", "zh"), ("漢語", "zh"),
    ("english", "en"),
    ("german", "de"), ("deutsch", "de"),
    ("spanish", "es"), ("español", "es"), ("espanol", "es"),
    ("french", "fr"), ("français", "fr"), ("francais", "fr"),
    ("italian", "it"), ("italiano", "it"),
    ("portuguese", "pt"), ("português", "pt"), ("portugues", "pt"),
    ("dutch", "nl"), ("nederlands", "nl"),
    ("czech", "cs"), ("čeština", "cs"), ("cestina", "cs"),
    ("arabic", "ar"), ("العربية", "ar"),
    ("albanian", "sq"), ("armenian", "hy"), ("bengali", "bn"), ("bulgarian", "bg"),
    ("croatian", "hr"), ("estonian", "et"), ("filipino", "tl"), ("tagalog", "tl"),
    ("georgian", "ka"), ("greek", "el"), ("icelandic", "is"), ("indonesian", "id"),
    ("kazakh", "kk"), ("khmer", "km"), ("latvian", "lv"), ("lithuanian", "lt"),
    ("malayalam", "ml"), ("malay", "ms"), ("marathi", "mr"), ("persian", "fa"),
    ("polish", "pl"), ("serbian", "sr"), ("slovak", "sk"), ("swedish", "sv"),
    ("turkish", "tr"),
];

/// 언어 이름 · 코드(`ko`, `ko-KR`, `Korean`, `한국어`, `Traditional Chinese`)를 두 글자 코드로. 번체 중국어는 `zh-tw`. 모르면 `en`.
/// logis-center 는 이름이 글 머리에 있을 때만 알아봤다(`Traditional Chinese` → `tr`). 여기서는 낱말 하나로 들어 있어도 본다
pub fn lang_code_of(lang: &str) -> String {
    let l = lang.trim().to_lowercase();
    if l.starts_with("zh-tw") || l.starts_with("zh-hk") || l.starts_with("zh-hant") {
        return "zh-tw".to_string();
    }
    let hit = LANG_NAME_CODES
        .iter()
        .find(|(name, _)| l.starts_with(*name))
        .or_else(|| {
            LANG_NAME_CODES
                .iter()
                .find(|(name, _)| if name.is_ascii() { l.split(|c: char| !c.is_alphanumeric()).any(|w| w == *name) } else { l.contains(*name) })
        });
    if let Some((_, code)) = hit {
        if *code == "zh" && (l.contains("tradition") || l.contains('繁') || l.contains("hant")) {
            return "zh-tw".to_string();
        }
        return code.to_string();
    }
    let code: String = l.chars().take_while(|c| c.is_ascii_alphabetic()).take(2).collect();
    if code.chars().count() >= 2 {
        code
    } else {
        "en".to_string()
    }
}

/// 언어의 대표 시간대(고정 오프셋)와 남반구 여부. logis-center 와 같은 표이고, 모르는 언어는 +09:00 이다
pub fn lang_clock(lang_code: &str) -> (FixedOffset, bool) {
    let norm = if lang_code.trim().is_empty() { String::new() } else { lang_code_of(lang_code) };
    let code = norm.split('-').next().unwrap_or("");
    let (offset_minutes, southern) = match code {
        "sq" | "ca" | "hr" | "cs" | "da" | "nl" | "fr" | "de" | "hu" | "it" | "no" | "pl" | "sr" | "sk" | "sl" | "es" | "sv" => (60, false),
        "bg" | "et" | "fi" | "el" | "he" | "lv" | "lt" | "ro" | "uk" => (120, false),
        "ar" | "tr" | "ru" => (180, false),
        "sw" => (180, true),
        "fa" => (210, false),
        "az" | "ka" => (240, false),
        "kk" | "ur" | "uz" => (300, false),
        "hi" | "mr" | "te" => (330, false),
        "bn" => (360, false),
        "id" | "km" | "th" | "vi" => (420, false),
        "zh" | "ms" | "tl" => (480, false),
        "ja" | "ko" => (540, false),
        "hy" => (240, false),
        "ml" => (330, false),
        "pt" => (-180, true),
        "is" | "en" => (0, false),
        _ => (540, false),
    };
    let offset = FixedOffset::east_opt(offset_minutes * 60).unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC"));
    (offset, southern)
}

/// `now_ms` 의 그 시간대 날짜
pub fn today_in(offset: &FixedOffset, now_ms: i64) -> NaiveDate {
    date_of_ms(offset, now_ms).unwrap_or_else(|| Utc::now().with_timezone(offset).date_naive())
}

pub fn date_of_ms(offset: &FixedOffset, ms: i64) -> Option<NaiveDate> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.with_timezone(offset).date_naive())
}

/// 그 시간대에서 하루가 시작하는 시각 (epoch ms)
pub fn day_start_ms(offset: &FixedOffset, d: NaiveDate) -> i64 {
    offset
        .with_ymd_and_hms(d.year(), d.month(), d.day(), 0, 0, 0)
        .single()
        .map(|t| t.timestamp_millis())
        .unwrap_or(0)
}

/// 날짜 범위(양 끝 포함)를 그 시간대의 epoch ms 범위로: 시작일 00:00:00.000 ~ 끝날 23:59:59.999
pub fn period_ms(offset: &FixedOffset, start: NaiveDate, end: NaiveDate) -> (i64, i64) {
    let s = day_start_ms(offset, start);
    let e = match end.succ_opt() {
        Some(next) => day_start_ms(offset, next) - 1,
        None => day_start_ms(offset, end) + DAY_MS - 1,
    };
    (s, e)
}

fn month_last(y: i32, m: u32) -> Option<NaiveDate> {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)?.pred_opt()
}

fn month_period(y: i32, m: u32) -> Option<(NaiveDate, NaiveDate)> {
    Some((NaiveDate::from_ymd_opt(y, m, 1)?, month_last(y, m)?))
}

fn year_period(y: i32) -> Option<(NaiveDate, NaiveDate)> {
    Some((NaiveDate::from_ymd_opt(y, 1, 1)?, NaiveDate::from_ymd_opt(y, 12, 31)?))
}

/// ISO 주(월요일 시작)의 월요일
fn week_start(d: NaiveDate) -> NaiveDate {
    d - Duration::days(d.weekday().num_days_from_monday() as i64)
}

/// 기간 키의 날짜 범위 (양 끝 포함). `this_*` 는 오늘까지가 아니라 그 달 · 주 · 해 전체다(logis-center 와 같음)
pub fn relative_period(key: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate)> {
    match key {
        "today" => Some((today, today)),
        "yesterday" => {
            let d = today.pred_opt()?;
            Some((d, d))
        }
        "this_week" => {
            let s = week_start(today);
            Some((s, s + Duration::days(6)))
        }
        "last_week" => {
            let s = week_start(today) - Duration::days(7);
            Some((s, s + Duration::days(6)))
        }
        "this_month" => month_period(today.year(), today.month()),
        "last_month" => {
            let prev = NaiveDate::from_ymd_opt(today.year(), today.month(), 1)?.pred_opt()?;
            month_period(prev.year(), prev.month())
        }
        "this_year" => year_period(today.year()),
        "last_year" => year_period(today.year() - 1),
        "recently" => Some((today.checked_sub_signed(Duration::days(RECENT_DAYS))?, today)),
        _ => None,
    }
}

fn season_start_month(season: &str, southern: bool) -> Option<u32> {
    let north: u32 = match season {
        "spring" => 3,
        "summer" => 6,
        "autumn" => 9,
        "winter" => 12,
        _ => return None,
    };
    // 남반구는 여섯 달 어긋난다 (여름 = 12 ~ 2월)
    Some(if southern { (north + 5) % 12 + 1 } else { north })
}

/// 계절 하나의 날짜 범위 (석 달, 겨울은 해를 넘긴다)
pub fn season_period(season: &str, year: i32, southern: bool) -> Option<(NaiveDate, NaiveDate)> {
    let sm = season_start_month(season, southern)?;
    let start = NaiveDate::from_ymd_opt(year, sm, 1)?;
    let (ey, em) = if sm + 2 > 12 { (year + 1, sm + 2 - 12) } else { (year, sm + 2) };
    Some((start, month_last(ey, em)?))
}

/// 계절이 아직 오지 않았을 때 어느 해로 볼까: 올해(`Current`) 또는 지난 가장 최근의 그 계절(`Latest`)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeasonAnchor {
    Current,
    Latest,
}

/// 계절의 해. 오늘이 그 계절 안이면 그 해, `last_year` 면 작년, 아니면 기준(`anchor`)을 따른다. 계절 키가 아니면 None
pub fn season_year(season: &str, time_key: &str, today: NaiveDate, southern: bool, anchor: SeasonAnchor) -> Option<i32> {
    season_start_month(season, southern)?;
    let y = today.year();
    if time_key == "last_year" {
        return Some(y - 1);
    }
    for cand in [y - 1, y] {
        if let Some((s, e)) = season_period(season, cand, southern) {
            if s <= today && today <= e {
                return Some(cand);
            }
        }
    }
    let anchor = if time_key == "this_year" { SeasonAnchor::Current } else { anchor };
    match anchor {
        SeasonAnchor::Current => Some(y),
        SeasonAnchor::Latest => {
            let (s, _) = season_period(season, y, southern)?;
            Some(if s <= today { y } else { y - 1 })
        }
    }
}

/// 기간 키 + 계절 키의 날짜 범위. 계절이 있으면 계절(기간 키는 해를 고르는 데만 쓴다), 없으면 기간 키
pub fn intent_period(time_key: &str, season_key: &str, today: NaiveDate, southern: bool, anchor: SeasonAnchor) -> Option<(NaiveDate, NaiveDate)> {
    if let Some(y) = season_year(season_key, time_key, today, southern, anchor) {
        if let Some(p) = season_period(season_key, y, southern) {
            return Some(p);
        }
    }
    relative_period(time_key, today)
}

/// 해석한 기간: 날짜 범위와 epoch ms 범위(사용자 시간대의 하루 경계)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub start_ms: i64,
    pub end_ms: i64,
    /// 시간대 오프셋 (분)
    pub offset_minutes: i32,
    /// 사람이 읽는 설명 (`Time intent 'last_week'`, `Season 'summer'` …)
    pub label: String,
}

impl TimeRange {
    /// `at_ms` 가 이 범위 안인가 (양 끝 포함)
    pub fn contains(&self, at_ms: i64) -> bool {
        self.start_ms <= at_ms && at_ms <= self.end_ms
    }
}

/// 기간 의도 → 범위. `time_key` · `season_key` 는 [`TIME_KEYS`] · [`SEASON_KEYS`] 이거나 빈 글이다. 둘 다 해석할 수 없으면 None.
/// 모델이 날짜를 냈더라도 이 함수의 결과로 덮어쓴다(날짜 불신 원칙)
pub fn resolve(time_key: &str, season_key: &str, lang_code: &str, anchor: SeasonAnchor, now_ms: i64) -> Option<TimeRange> {
    let (time_key, season_key) = (time_key.trim().to_ascii_lowercase(), season_key.trim().to_ascii_lowercase());
    let (offset, southern) = lang_clock(lang_code);
    let today = today_in(&offset, now_ms);
    let (start, end) = intent_period(&time_key, &season_key, today, southern, anchor)?;
    let label = if season_year(&season_key, &time_key, today, southern, anchor).is_none() {
        format!("Time intent '{time_key}'")
    } else if time_key.is_empty() {
        format!("Season '{season_key}'")
    } else {
        format!("Season '{season_key}' of time intent '{time_key}'")
    };
    let (start_ms, end_ms) = period_ms(&offset, start, end);
    Some(TimeRange { start, end, start_ms, end_ms, offset_minutes: offset.local_minus_utc() / 60, label })
}

/// 기간 비교 창 (epoch ms, 양 끝 포함)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonWindow {
    pub current_start_ms: i64,
    /// 진행 중인 기간이면 그 기간의 끝이 아니라 지금(`now_ms`)
    pub current_end_ms: i64,
    pub previous_start_ms: i64,
    /// 진행 중인 기간이면 `previous_start_ms + (current_end_ms − current_start_ms)` (앞 기간이 더 짧으면 그 끝 23:59:59.999 에서 자른다)
    pub previous_end_ms: i64,
    /// 진행 중인 기간이라 같은 경과 시간으로 잘랐는가
    pub pace_matched: bool,
    /// `this_week (+37.0h) vs last_week (+37.0h)` · `yesterday vs day_before`
    pub label: String,
    /// 견주는 두 기간의 이름 (기간 키, 키가 없는 앞 기간은 `day_before` · `week_before` · `month_before` · `year_before` ·
    /// `previous_period`)
    pub current_key: String,
    pub previous_key: String,
}

impl ComparisonWindow {
    /// 현재 창 (시작, 끝)
    pub fn current_range(&self) -> (i64, i64) {
        (self.current_start_ms, self.current_end_ms)
    }

    /// 비교 창 (시작, 끝)
    pub fn previous_range(&self) -> (i64, i64) {
        (self.previous_start_ms, self.previous_end_ms)
    }
}

/// 기간 키 바로 앞의 같은 단위 기간과 그 이름 (오늘의 앞은 어제, 이번 주의 앞은 지난주, 지난달의 앞은 그 전달 …)
fn preceding(key: &str, start: NaiveDate, end: NaiveDate) -> Option<((NaiveDate, NaiveDate), &'static str)> {
    let day = |d: NaiveDate| (d, d);
    let week = || (start - Duration::days(7), end - Duration::days(7));
    Some(match key {
        "today" => (day(start.pred_opt()?), "yesterday"),
        "yesterday" => (day(start.pred_opt()?), "day_before"),
        "this_week" => (week(), "last_week"),
        "last_week" => (week(), "week_before"),
        "this_month" | "last_month" => {
            let p = start.pred_opt()?;
            (month_period(p.year(), p.month())?, if key == "this_month" { "last_month" } else { "month_before" })
        }
        "this_year" => (year_period(start.year() - 1)?, "last_year"),
        "last_year" => (year_period(start.year() - 1)?, "year_before"),
        "recently" => {
            let len = (end - start).num_days() + 1;
            ((start - Duration::days(len), start.pred_opt()?), "previous_period")
        }
        _ => return None,
    })
}

/// 기간 비교 창. `primary_time_key` 가 없으면 `today` 다. `explicit_compare_key` 가 있으면 그 기간과, 없으면 바로 앞의 같은 단위
/// 기간과 견준다(둘 중 더 늦은 기간이 현재 창이 된다: "지난주 대비 이번 주" 도 이번 주가 현재다). 현재 창이 지금을 품은 진행 중인
/// 기간(오늘 · 이번 주 · 이번 달 · 올해 · 최근)이면 지금까지 지난 시간만 보고 비교 창도 같은 경과 시간으로 자른다(`pace_matched`).
/// 끝난 기간(어제 · 지난주 …)은 두 기간 전체를 견준다. 기간 키가 아니면(계절 · 모르는 키) None
pub fn resolve_comparison_window(primary_time_key: Option<&str>, explicit_compare_key: Option<&str>, lang: &str, now_ms: i64) -> Option<ComparisonWindow> {
    let norm = |k: Option<&str>| k.map(|k| k.trim().to_ascii_lowercase()).filter(|k| !k.is_empty());
    let primary = norm(primary_time_key).unwrap_or_else(|| "today".to_string());
    let compare = norm(explicit_compare_key).filter(|k| *k != primary);
    let (offset, _) = lang_clock(lang);
    let today = today_in(&offset, now_ms);
    let cur = relative_period(&primary, today)?;
    let (prev, prev_key) = match compare {
        Some(k) => (relative_period(&k, today)?, k),
        None => {
            let (p, name) = preceding(&primary, cur.0, cur.1)?;
            (p, name.to_string())
        }
    };
    let ((cur_key, cur), (prev_key, prev)) = if prev.0 > cur.0 { ((prev_key, prev), (primary, cur)) } else { ((primary, cur), (prev_key, prev)) };
    let (cur_start, cur_end) = period_ms(&offset, cur.0, cur.1);
    let (prev_start, prev_end) = period_ms(&offset, prev.0, prev.1);
    let in_progress = cur_start <= now_ms && now_ms < cur_end;
    let (current_end_ms, previous_end_ms, label) = if in_progress {
        let elapsed = now_ms - cur_start;
        let cut = (prev_start + elapsed).min(prev_end);
        let hours = |ms: i64| ms as f64 / 3_600_000.0;
        (now_ms, cut, format!("{cur_key} (+{:.1}h) vs {prev_key} (+{:.1}h)", hours(elapsed), hours(cut - prev_start)))
    } else {
        (cur_end, prev_end, format!("{cur_key} vs {prev_key}"))
    };
    Some(ComparisonWindow {
        current_start_ms: cur_start,
        current_end_ms,
        previous_start_ms: prev_start,
        previous_end_ms,
        pace_matched: in_progress,
        label,
        current_key: cur_key,
        previous_key: prev_key,
    })
}

/// 질의에 들어갈 기준표: 오늘 · 어제 · 이번 주 · 지난주 · 이번 달 · 지난달 · 올해 · 작년의 날짜 (모델에게 보여 주는 참고. 모델은 키만 고른다)
pub fn guide_table(lang_code: &str, now_ms: i64) -> Vec<(String, NaiveDate, NaiveDate)> {
    let (offset, _) = lang_clock(lang_code);
    let today = today_in(&offset, now_ms);
    TIME_KEYS
        .iter()
        .filter_map(|k| relative_period(k, today).map(|(s, e)| (k.to_string(), s, e)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    // 2026-11-18 (수) 12:00 KST = 03:00 UTC
    const NOW: i64 = 1_794_970_800_000;

    #[test]
    fn now_is_wednesday_in_seoul() {
        let (kst, _) = lang_clock("ko");
        assert_eq!(today_in(&kst, NOW), d(2026, 11, 18));
        assert_eq!(today_in(&kst, NOW).weekday(), chrono::Weekday::Wed);
    }

    #[test]
    fn language_names_map_to_codes() {
        for (name, code) in [("ko-KR", "ko"), ("Korean", "ko"), ("한국어", "ko"), ("zh-Hant", "zh-tw"), ("Traditional Chinese", "zh-tw"), ("pt-BR", "pt"), ("?", "en")] {
            assert_eq!(lang_code_of(name), code, "{name}");
        }
    }

    #[test]
    fn relative_keys_resolve_to_whole_days_in_the_user_clock() {
        let r = resolve("yesterday", "", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((r.start, r.end), (d(2026, 11, 17), d(2026, 11, 17)));
        // 2026-11-17 00:00 KST = 2026-11-16 15:00 UTC
        assert_eq!(r.start_ms, 1_794_841_200_000);
        assert_eq!(r.end_ms, r.start_ms + DAY_MS - 1);
        assert_eq!(r.offset_minutes, 540);
        assert!(r.contains(r.start_ms) && r.contains(r.end_ms) && !r.contains(r.end_ms + 1));

        let w = resolve("this_week", "", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((w.start, w.end), (d(2026, 11, 16), d(2026, 11, 22)));
        let lw = resolve("last_week", "", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((lw.start, lw.end), (d(2026, 11, 9), d(2026, 11, 15)));
        let lm = resolve("last_month", "", "en", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((lm.start, lm.end), (d(2026, 10, 1), d(2026, 10, 31)));
        let rc = resolve("recently", "", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((rc.start, rc.end), (d(2026, 10, 19), d(2026, 11, 18)));
        assert!(resolve("next_decade", "", "ko", SeasonAnchor::Current, NOW).is_none());
    }

    #[test]
    fn the_same_instant_is_a_different_day_in_another_clock() {
        // 2026-11-18 03:00 UTC 는 서울에서 18일, 런던(en)에서도 18일, 그러나 2026-11-17 20:00 UTC 는 서울 18일 · 런던 17일
        let evening_utc = 1_794_945_600_000;
        let ko = resolve("today", "", "ko", SeasonAnchor::Current, evening_utc).unwrap();
        let en = resolve("today", "", "en", SeasonAnchor::Current, evening_utc).unwrap();
        assert_eq!(ko.start, d(2026, 11, 18));
        assert_eq!(en.start, d(2026, 11, 17));
    }

    #[test]
    fn seasons_follow_the_hemisphere_and_anchor() {
        // 11월의 여름: 올해 기준이면 2026년 여름(지난), 남반구(pt)면 12 ~ 2월
        let s = resolve("", "summer", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((s.start, s.end), (d(2026, 6, 1), d(2026, 8, 31)));
        assert_eq!(s.label, "Season 'summer'");
        let south = resolve("", "summer", "pt-BR", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((south.start, south.end), (d(2026, 12, 1), d(2027, 2, 28)));
        // 아직 오지 않은 계절: Latest 면 작년
        let spring_latest = resolve("", "winter", "ko", SeasonAnchor::Latest, NOW).unwrap();
        assert_eq!(spring_latest.start, d(2025, 12, 1));
        let spring_current = resolve("", "winter", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!(spring_current.start, d(2026, 12, 1));
        // 작년 여름
        let ly = resolve("last_year", "summer", "ko", SeasonAnchor::Current, NOW).unwrap();
        assert_eq!((ly.start, ly.end), (d(2025, 6, 1), d(2025, 8, 31)));
        assert_eq!(ly.label, "Season 'summer' of time intent 'last_year'");
    }

    #[test]
    fn guide_table_lists_every_time_key() {
        let t = guide_table("ko", NOW);
        assert_eq!(t.len(), TIME_KEYS.len());
        assert_eq!(t[0], ("today".to_string(), d(2026, 11, 18), d(2026, 11, 18)));
    }

    // 2026-11-17 (화) 13:00 KST
    const TUESDAY_13: i64 = 1_794_841_200_000 + 13 * 3_600_000;
    const HOUR: i64 = 3_600_000;

    #[test]
    fn an_unfinished_week_is_compared_at_the_same_elapsed_time() {
        let w = resolve_comparison_window(Some("this_week"), Some("last_week"), "ko", TUESDAY_13).unwrap();
        let monday = 1_794_841_200_000 - DAY_MS;
        assert_eq!(w.current_range(), (monday, TUESDAY_13));
        assert_eq!(w.previous_range(), (monday - 7 * DAY_MS, monday - 7 * DAY_MS + 37 * HOUR));
        assert!(w.pace_matched);
        assert_eq!(w.label, "this_week (+37.0h) vs last_week (+37.0h)");
        // 비교 키를 생략해도, 순서를 바꿔 물어도 같은 창이다
        assert_eq!(resolve_comparison_window(Some("this_week"), None, "ko", TUESDAY_13).unwrap(), w);
        assert_eq!(resolve_comparison_window(Some("last_week"), Some("this_week"), "ko", TUESDAY_13).unwrap(), w);
        // 오늘 vs 어제 (기간 말이 없으면 오늘): 13시간끼리
        let t = resolve_comparison_window(None, None, "ko", TUESDAY_13).unwrap();
        assert_eq!((t.current_end_ms - t.current_start_ms, t.previous_end_ms - t.previous_start_ms), (13 * HOUR, 13 * HOUR));
        assert_eq!((t.current_key.as_str(), t.previous_key.as_str()), ("today", "yesterday"));
    }

    #[test]
    fn finished_periods_are_compared_whole_and_short_months_clamp() {
        let y = resolve_comparison_window(Some("yesterday"), None, "ko", TUESDAY_13).unwrap();
        assert!(!y.pace_matched);
        assert_eq!(y.current_range(), (1_794_841_200_000 - DAY_MS, 1_794_841_200_000 - 1));
        assert_eq!(y.previous_range(), (1_794_841_200_000 - 2 * DAY_MS, 1_794_841_200_000 - DAY_MS - 1));
        assert_eq!(y.label, "yesterday vs day_before");
        // 3월 31일 13시의 이번 달: 2월(28일)은 끝에서 자른다
        let (kst, _) = lang_clock("ko");
        let march_31 = day_start_ms(&kst, d(2026, 3, 31)) + 13 * HOUR;
        let m = resolve_comparison_window(Some("this_month"), None, "ko", march_31).unwrap();
        assert_eq!(m.previous_start_ms, day_start_ms(&kst, d(2026, 2, 1)));
        assert_eq!(m.previous_end_ms, day_start_ms(&kst, d(2026, 3, 1)) - 1);
        assert_eq!(m.current_range(), (day_start_ms(&kst, d(2026, 3, 1)), march_31));
        // 기간 키가 아니면 비교하지 않는다
        assert!(resolve_comparison_window(Some("summer"), None, "ko", TUESDAY_13).is_none());
    }
}
