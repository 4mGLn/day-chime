pub mod browser;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use reqwest::{blocking::Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration as StdDuration, Instant},
};

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub source: String,
    pub browser_url: String,
    /// Legacy configuration key, accepted but ignored: browser records now expire by work date.
    pub browser_max_age_seconds: u64,
    pub login_id: String,
    pub timezone: String,
    pub work_minutes: i64,
    pub break_minutes: i64,
    pub poll_seconds: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            source: "browser".into(),
            browser_url: "https://example.daouoffice.com/home".into(),
            browser_max_age_seconds: 900,
            login_id: String::new(),
            timezone: "Asia/Seoul".into(),
            work_minutes: 480,
            break_minutes: 60,
            poll_seconds: 300,
        }
    }
}
impl Config {
    pub fn validate(&self) -> Result<Tz> {
        if !matches!(self.source.as_str(), "browser" | "api") {
            bail!("source must be browser or api");
        }
        browser::validate_source_url(&self.browser_url)?;
        if self.source == "api" && self.login_id.trim().is_empty() {
            bail!("login_id must be set");
        }
        if !(1..=1440).contains(&self.work_minutes) || !(0..=1440).contains(&self.break_minutes) {
            bail!("invalid work_minutes/break_minutes");
        }
        if self.work_minutes + self.break_minutes > 1440 {
            bail!("work plus break must not exceed 24 hours");
        }
        if !(10..=3600).contains(&self.poll_seconds) {
            bail!("poll_seconds must be between 10 and 3600");
        }
        self.timezone.parse().context("invalid timezone")
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub login_id: String,
    pub accrual_date: NaiveDate,
    pub start_work_time: Option<String>,
    pub end_work_time: Option<String>,
    pub zone_id: String,
    pub is_working_day: bool,
}
#[derive(Deserialize)]
struct Envelope {
    code: serde_json::Value,
    data: PageData,
}
#[derive(Deserialize)]
struct PageData {
    page: Page,
    elements: Vec<Record>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    current_page: usize,
    total_pages: usize,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: serde_json::Value,
}

pub struct Api {
    client: Client,
    base: String,
    id: String,
    secret: String,
    token: Option<(String, Instant)>,
}
impl Api {
    pub fn new(id: String, secret: String) -> Result<Self> {
        Self::with_base(id, secret, "https://api.daouoffice.com".into())
    }
    fn with_base(id: String, secret: String, base: String) -> Result<Self> {
        if id.trim().is_empty() || secret.trim().is_empty() {
            bail!("empty credentials");
        }
        let client = Client::builder()
            .timeout(StdDuration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            base,
            id,
            secret,
            token: None,
        })
    }
    fn token(&mut self) -> Result<String> {
        if let Some((token, expires)) = &self.token {
            if Instant::now() < *expires {
                return Ok(token.clone());
            }
        }
        let response = self
            .client
            .post(format!("{}/public/auth/v1/oauth2/token", self.base))
            .basic_auth(&self.id, Some(&self.secret))
            .form(&[("grant_type", "client_credentials")])
            .send()
            .context("token request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("token HTTP {}", status.as_u16());
        }
        let value: TokenResponse = response.json().context("invalid token response")?;
        if !value.token_type.eq_ignore_ascii_case("bearer") || value.access_token.is_empty() {
            bail!("invalid token type or empty token");
        }
        let seconds = number(&value.expires_in).context("invalid expires_in")?;
        if seconds == 0 || seconds > 31_536_000 {
            bail!("expires_in outside supported range");
        }
        self.token = Some((
            value.access_token.clone(),
            Instant::now() + StdDuration::from_secs(seconds.saturating_sub(30)),
        ));
        Ok(value.access_token)
    }
    pub fn records(&mut self, day: NaiveDate) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        for page in 0..1000 {
            let mut response = None;
            for attempt in 0..2 {
                let token = self.token()?;
                let r = self
                    .client
                    .get(format!("{}/public/api/attnd-v2/attnd", self.base))
                    .bearer_auth(token)
                    .query(&[
                        ("startDate", day.to_string()),
                        ("endDate", day.to_string()),
                        ("page", page.to_string()),
                        ("pageSize", "50".into()),
                    ])
                    .send()
                    .context("attendance request failed")?;
                if r.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                    self.token = None;
                    continue;
                }
                response = Some(r);
                break;
            }
            let r = response.context("attendance authentication failed")?;
            if !r.status().is_success() {
                bail!("attendance HTTP {}", r.status().as_u16());
            }
            let envelope: Envelope = r.json().context("invalid attendance response")?;
            if number(&envelope.code) != Some(200) {
                bail!("attendance API returned non-success code");
            }
            if envelope.data.page.current_page != page {
                bail!("unexpected pagination page");
            }
            let total = envelope.data.page.total_pages;
            records.extend(envelope.data.elements);
            if total == 0 || page + 1 >= total {
                return Ok(records);
            }
        }
        bail!("attendance exceeded 1000 pages")
    }
}
fn number(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

#[derive(Clone, Debug)]
pub struct Schedule {
    pub key: String,
    pub due: DateTime<Utc>,
    pub zone: Tz,
}
pub fn schedule(records: &[Record], config: &Config, day: NaiveDate) -> Result<Option<Schedule>> {
    let matches: Vec<_> = records
        .iter()
        .filter(|r| r.login_id == config.login_id && r.accrual_date == day)
        .collect();
    if matches.len() > 1 {
        bail!("multiple records for the same user/date; refusing ambiguous reminder");
    }
    let Some(record) = matches.first() else {
        return Ok(None);
    };
    if !record.is_working_day || record.end_work_time.as_ref().is_some_and(|s| !s.is_empty()) {
        return Ok(None);
    }
    let Some(start) = record.start_work_time.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let zone: Tz = record
        .zone_id
        .parse()
        .context("invalid attendance zoneId")?;
    let naive = NaiveDateTime::parse_from_str(start, "%Y-%m-%d %H:%M:%S")
        .context("invalid startWorkTime")?;
    let start = zone
        .from_local_datetime(&naive)
        .single()
        .context("ambiguous or nonexistent startWorkTime")?;
    let due =
        start.with_timezone(&Utc) + Duration::minutes(config.work_minutes + config.break_minutes);
    Ok(Some(Schedule {
        key: format!("{}:{}", config.login_id, day),
        due,
        zone,
    }))
}

/// Today's record takes priority; yesterday can carry an overnight shift into today.
pub fn current_schedule(
    api: &mut Api,
    config: &Config,
    day: NaiveDate,
) -> Result<Option<Schedule>> {
    let today = api.records(day)?;
    // A real check-in/out today takes priority over an older shift.
    if today.iter().any(|r| {
        r.login_id == config.login_id
            && r.accrual_date == day
            && (r.start_work_time.as_ref().is_some_and(|v| !v.is_empty())
                || r.end_work_time.as_ref().is_some_and(|v| !v.is_empty()))
    }) {
        return schedule(&today, config, day);
    }
    let yesterday = day.pred_opt().context("date underflow")?;
    let previous = schedule(&api.records(yesterday)?, config, yesterday)?;
    Ok(previous.filter(|s| s.due.with_timezone(&s.zone).date_naive() == day))
}

#[derive(Default, Serialize, Deserialize)]
pub struct State {
    pub notified: BTreeSet<String>,
}
impl State {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => serde_json::from_slice(&data).context("invalid state file"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        let temp = path.with_extension("tmp");
        let bytes = serde_json::to_vec(self)?;
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&temp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(temp, path).context("save notification state")
    }
}

pub struct Reminder {
    pub key: String,
    pub summary: String,
    pub body: String,
}

fn duration_label(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes}분"),
        (hours, 0) => format!("{hours}시간"),
        (hours, minutes) => format!("{hours}시간 {minutes}분"),
    }
}

/// Arrival and departure are independent daily events, persisted only after delivery.
pub fn pending_reminders(
    schedule: &Schedule,
    config: &Config,
    state: &State,
    now: DateTime<Utc>,
) -> Vec<Reminder> {
    let mut reminders = Vec::new();
    let arrival = (schedule.due - Duration::minutes(config.work_minutes + config.break_minutes))
        .with_timezone(&schedule.zone);
    let departure = schedule.due.with_timezone(&schedule.zone);
    let today = now.with_timezone(&schedule.zone).date_naive();
    let arrival_label = arrival.format(if arrival.date_naive() == today {
        "%H:%M"
    } else {
        "%m/%d %H:%M"
    });
    let departure_label = if departure.date_naive() == today {
        departure.format("%H:%M").to_string()
    } else if Some(departure.date_naive()) == today.succ_opt() {
        format!("내일 {}", departure.format("%H:%M"))
    } else {
        departure.format("%m/%d %H:%M").to_string()
    };
    let follow_up = if now >= schedule.due {
        "퇴근 예정 시각이 지났어요."
    } else {
        "예정 시각에 다시 알려드릴게요."
    };
    let arrival_key = format!("arrival:{}", schedule.key);
    if !state.notified.contains(&arrival_key) {
        reminders.push(Reminder {
            key: arrival_key,
            summary: format!("출근 확인 · {departure_label} 퇴근 예정"),
            body: format!(
                "출근 {arrival_label}\n근무 {} · 휴게 {} 기준\n{follow_up}",
                duration_label(config.work_minutes),
                duration_label(config.break_minutes)
            ),
        });
    }
    if now >= schedule.due && !state.notified.contains(&schedule.key) {
        let departure_time = departure.format(
            if arrival.date_naive() == departure.date_naive() && departure.date_naive() == today {
                "%H:%M"
            } else {
                "%m/%d %H:%M"
            },
        );
        let day_label = if departure.date_naive() == today {
            "오늘 "
        } else {
            ""
        };
        reminders.push(Reminder {
            key: schedule.key.clone(),
            summary: "이제 퇴근할 시간이에요".into(),
            body: format!(
                "{day_label}퇴근 예정은 {departure_time}이에요.\n퇴근 체크도 잊지 마세요. 오늘도 수고하셨어요!"
            ),
        });
    }
    reminders
}

pub fn notify(body: &str) -> Result<()> {
    notify_with_summary("퇴근 시간 알림", body)
}
pub fn notify_with_summary(summary: &str, body: &str) -> Result<()> {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary(summary)
        .body(body)
        .appname("day-chime");
    #[cfg(target_os = "linux")]
    notification.icon("day-chime");
    notification.show().context("desktop notification failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    fn record() -> Record {
        serde_json::from_value(serde_json::json!({"loginId":"me","accrualDate":"2026-11-03","startWorkTime":"2026-11-03 09:10:00","endWorkTime":null,"zoneId":"Asia/Seoul","isWorkingDay":true})).unwrap()
    }
    fn config() -> Config {
        Config {
            login_id: "me".into(),
            ..Config::default()
        }
    }
    #[test]
    fn rejects_shift_longer_than_one_day() {
        let c = Config {
            work_minutes: 1440,
            break_minutes: 1,
            ..config()
        };
        assert!(c.validate().is_err());
    }
    #[test]
    fn calculates_work_plus_break_and_matches_identity() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let r = record();
        assert_eq!(
            schedule(std::slice::from_ref(&r), &config(), day)
                .unwrap()
                .unwrap()
                .due
                .to_rfc3339(),
            "2026-11-03T09:10:00+00:00"
        );
        let mut c = config();
        c.login_id = "other".into();
        assert!(schedule(&[r], &c, day).unwrap().is_none());
    }
    #[test]
    fn skips_missing_completed_and_nonworking() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        for n in 0..3 {
            let mut r = record();
            match n {
                0 => r.start_work_time = None,
                1 => r.end_work_time = Some("2026-11-03 18:00:00".into()),
                _ => r.is_working_day = false,
            };
            assert!(schedule(&[r], &config(), day).unwrap().is_none());
        }
    }
    #[test]
    fn refuses_duplicates_and_ambiguous_dst() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        assert!(schedule(&[record(), record()], &config(), day).is_err());
        let mut r = record();
        r.zone_id = "America/New_York".into();
        r.start_work_time = Some("2026-11-01 01:30:00".into());
        assert!(schedule(&[r], &config(), day).is_err());
    }
    #[test]
    fn state_survives_restart_and_rejects_corruption() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("state.json");
        let mut state = State::load(&p).unwrap();
        state.notified.insert("me:2026-11-03".into());
        state.save(&p).unwrap();
        assert!(State::load(&p).unwrap().notified.contains("me:2026-11-03"));
        std::fs::write(&p, "broken").unwrap();
        assert!(State::load(&p).is_err());
    }
    fn mock(responses: Vec<(u16, String)>) -> (String, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(StdDuration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 4096];
                loop {
                    let n = stream.read(&mut buf).unwrap();
                    request.extend_from_slice(&buf[..n]);
                    if let Some(pos) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..pos]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length: ")
                                    .and_then(|s| s.parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if request.len() >= pos + 4 + len {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                requests.push(String::from_utf8(request).unwrap());
                write!(stream,"HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
            requests
        });
        (base, handle)
    }
    fn token() -> String {
        r#"{"access_token":"test-token","token_type":"bearer","expires_in":"86400"}"#.into()
    }
    fn page(n: usize, total: usize) -> String {
        serde_json::json!({"code":"200","data":{"page":{"currentPage":n,"totalPages":total},"elements":[{"loginId":"me","accrualDate":"2026-11-03","startWorkTime":null,"endWorkTime":null,"zoneId":"Asia/Seoul","isWorkingDay":true}]}}).to_string()
    }
    #[test]
    fn http_contract_auth_pagination_and_401_refresh() {
        let (base, handle) = mock(vec![
            (200, token()),
            (401, "{}".into()),
            (200, token()),
            (200, page(0, 2)),
            (200, page(1, 2)),
        ]);
        let mut api = Api::with_base("id".into(), "secret".into(), base).unwrap();
        assert_eq!(
            api.records(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())
                .unwrap()
                .len(),
            2
        );
        let requests = handle.join().unwrap();
        assert!(requests[0].starts_with("POST /public/auth/v1/oauth2/token "));
        assert!(requests[0]
            .to_lowercase()
            .contains("authorization: basic awq6c2vjcmv0"));
        assert!(requests[0].contains("grant_type=client_credentials"));
        assert!(requests[3].contains("startDate=2026-11-03&endDate=2026-11-03&page=0&pageSize=50"));
        assert!(requests[4].contains("page=1"));
        assert!(requests[4].contains("Bearer test-token"));
    }
    #[test]
    fn overnight_shift_is_carried_into_next_day_but_old_daytime_is_not() {
        let day = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        for (start, expected) in [
            ("2026-11-02 18:00:00", true),
            ("2026-11-02 09:00:00", false),
        ] {
            let empty = serde_json::json!({"code":200,"data":{"page":{"currentPage":0,"totalPages":1},"elements":[{"loginId":"me","accrualDate":"2026-11-03","startWorkTime":null,"endWorkTime":null,"zoneId":"Asia/Seoul","isWorkingDay":true}]}}).to_string();
            let previous = serde_json::json!({"code":200,"data":{"page":{"currentPage":0,"totalPages":1},"elements":[{"loginId":"me","accrualDate":"2026-11-02","startWorkTime":start,"endWorkTime":null,"zoneId":"Asia/Seoul","isWorkingDay":true}]}}).to_string();
            let (base, handle) = mock(vec![(200, token()), (200, empty), (200, previous)]);
            let mut api = Api::with_base("id".into(), "secret".into(), base).unwrap();
            let schedule = current_schedule(&mut api, &config(), day).unwrap();
            assert_eq!(schedule.is_some(), expected);
            if let Some(s) = schedule {
                assert_eq!(s.key, "me:2026-11-02");
                assert_eq!(
                    s.due.with_timezone(&s.zone).format("%H:%M").to_string(),
                    "03:00"
                );
            }
            handle.join().unwrap();
        }
    }
    #[test]
    fn rejects_application_error_and_malformed_response() {
        for body in [
            r#"{"code":500,"data":{"page":{"currentPage":0,"totalPages":0},"elements":[]}}"#,
            "{}",
        ] {
            let (base, handle) = mock(vec![(200, token()), (200, body.into())]);
            let mut api = Api::with_base("id".into(), "secret".into(), base).unwrap();
            assert!(api
                .records(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap())
                .is_err());
            handle.join().unwrap();
        }
    }
    #[test]
    fn initial_and_due_reminders_are_independent_and_survive_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notifications.json");
        let due: DateTime<Utc> = "2026-11-03T09:50:00Z".parse().unwrap();
        let schedule = Schedule {
            key: "browser:tenant:2026-11-03".into(),
            due,
            zone: chrono_tz::Asia::Seoul,
        };
        let mut state = State::default();
        let initial = pending_reminders(&schedule, &config(), &state, due - Duration::hours(8));
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].summary, "출근 확인 · 18:50 퇴근 예정");
        assert_eq!(
            initial[0].body,
            "출근 09:50\n근무 8시간 · 휴게 1시간 기준\n예정 시각에 다시 알려드릴게요."
        );
        // Failure leaves state untouched, so the initial reminder remains retryable.
        assert_eq!(
            pending_reminders(&schedule, &config(), &state, due).len(),
            2
        );
        state.notified.insert(initial[0].key.clone());
        state.save(&path).unwrap();
        let restarted = State::load(&path).unwrap();
        assert!(
            pending_reminders(&schedule, &config(), &restarted, due - Duration::seconds(1))
                .is_empty()
        );
        let pending = pending_reminders(&schedule, &config(), &restarted, due);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key, schedule.key);
        assert_eq!(pending[0].summary, "이제 퇴근할 시간이에요");
        assert_eq!(
            pending[0].body,
            "오늘 퇴근 예정은 18:50이에요.\n퇴근 체크도 잊지 마세요. 오늘도 수고하셨어요!"
        );
        state.notified.insert(schedule.key.clone());
        assert!(pending_reminders(&schedule, &config(), &state, due).is_empty());
        let legacy_departed = State {
            notified: BTreeSet::from([schedule.key.clone()]),
        };
        assert_eq!(
            pending_reminders(&schedule, &config(), &legacy_departed, due)[0].summary,
            "출근 확인 · 18:50 퇴근 예정"
        );
    }
    #[test]
    fn reminder_copy_uses_configured_minutes_and_zero_break() {
        let due: DateTime<Utc> = "2026-11-03T08:20:00Z".parse().unwrap();
        let schedule = Schedule {
            key: "custom".into(),
            due,
            zone: chrono_tz::Asia::Seoul,
        };
        let custom = Config {
            work_minutes: 450,
            break_minutes: 0,
            ..config()
        };
        let reminders = pending_reminders(
            &schedule,
            &custom,
            &State::default(),
            due - Duration::hours(1),
        );
        assert_eq!(reminders[0].summary, "출근 확인 · 17:20 퇴근 예정");
        assert!(reminders[0].body.contains("출근 09:50"));
        assert!(reminders[0]
            .body
            .contains("근무 7시간 30분 · 휴게 0분 기준"));
    }
    #[test]
    fn overnight_and_delayed_copy_keeps_work_dates_clear() {
        let due: DateTime<Utc> = "2026-11-03T18:00:00Z".parse().unwrap();
        let schedule = Schedule {
            key: "overnight".into(),
            due,
            zone: chrono_tz::Asia::Seoul,
        };
        let initial = pending_reminders(
            &schedule,
            &config(),
            &State::default(),
            due - Duration::hours(8),
        );
        assert_eq!(initial[0].summary, "출근 확인 · 내일 03:00 퇴근 예정");
        assert!(initial[0].body.contains("출근 18:00"));
        let delayed = pending_reminders(
            &schedule,
            &config(),
            &State::default(),
            due + Duration::hours(1),
        );
        assert_eq!(delayed[0].summary, "출근 확인 · 03:00 퇴근 예정");
        assert!(delayed[0].body.contains("출근 11/03 18:00"));
        assert!(delayed[0].body.ends_with("퇴근 예정 시각이 지났어요."));
        assert!(!delayed[0].body.contains("다시 알려"));
        assert!(delayed[1].body.contains("퇴근 예정은 11/04 03:00이에요."));
        assert!(!delayed[1].body.contains("현재"));
        let next_day = pending_reminders(
            &schedule,
            &config(),
            &State::default(),
            due + Duration::days(1),
        );
        assert!(next_day[1]
            .body
            .starts_with("퇴근 예정은 11/04 03:00이에요."));
    }
}
