mod config_reload;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use clap::{Parser, Subcommand};
use day_chime::{
    delivery::{Delivery, DeliveryOutcome, SaveOutcome},
    Api, Config, Schedule, State,
};
use fs2::FileExt;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(version, about = "DaouOffice attendance desktop reminder")]
struct Args {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        dry_run: bool,
    },
    Check,
    NotifyTest,
    BrowserHost,
}

fn api_for(config: &Config) -> Result<Option<Api>> {
    if config.source != "api" {
        return Ok(None);
    }
    Ok(Some(Api::new(
        std::env::var("DAOU_CLIENT_ID").context("set DAOU_CLIENT_ID")?,
        std::env::var("DAOU_CLIENT_SECRET").context("set DAOU_CLIENT_SECRET")?,
    )?))
}
fn prepare(config: Config) -> Result<(Config, Tz, Option<Api>)> {
    let timezone = config.validate()?;
    let api = api_for(&config)?;
    Ok((config, timezone, api))
}
fn fetch(
    config: &Config,
    timezone: Tz,
    state_dir: &Path,
    api: &mut Option<Api>,
    now: DateTime<Utc>,
) -> Result<Option<Schedule>> {
    if let Some(api) = api.as_mut() {
        day_chime::current_schedule(api, config, now.with_timezone(&timezone).date_naive())
    } else {
        day_chime::browser::current_schedule(state_dir, config, now)
    }
}

#[derive(PartialEq)]
enum RefreshStatus {
    Scheduled(String, DateTime<Utc>, Tz),
    Empty,
    Failed(String),
}
impl RefreshStatus {
    fn from_result(result: &Result<Option<Schedule>>) -> Self {
        match result {
            Ok(Some(s)) => Self::Scheduled(s.key.clone(), s.due, s.zone),
            Ok(None) => Self::Empty,
            Err(error) => Self::Failed(format!("{error:#}")),
        }
    }
}
#[derive(Default)]
struct RefreshLog {
    previous: Option<RefreshStatus>,
}
impl RefreshLog {
    fn changed(&mut self, status: RefreshStatus) -> bool {
        if self.previous.as_ref() == Some(&status) {
            return false;
        }
        self.previous = Some(status);
        true
    }
}

#[derive(Default)]
struct ErrorLog {
    previous: BTreeMap<String, String>,
}
impl ErrorLog {
    fn changed(&mut self, key: &str, error: String) -> bool {
        if self.previous.get(key) == Some(&error) {
            return false;
        }
        self.previous.insert(key.to_owned(), error);
        true
    }
    fn recovered(&mut self, key: &str) {
        self.previous.remove(key);
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    if matches!(args.command, Command::NotifyTest) {
        return day_chime::notify("day-chime 데스크톱 알림 테스트입니다.");
    }
    let state_dir = args
        .state_dir
        .or_else(|| dirs::data_local_dir().map(|d| d.join("day-chime")))
        .context("cannot resolve state directory")?;
    let path = args
        .config
        .or_else(|| dirs::config_dir().map(|d| d.join("day-chime/config.toml")))
        .context("cannot resolve config directory")?;
    let contents =
        std::fs::read(&path).with_context(|| format!("read config {}", path.display()))?;
    let mut config = config_reload::parse(&contents)?;
    let mut timezone = config.validate()?;
    if matches!(args.command, Command::BrowserHost) {
        return day_chime::browser::host_reloading(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            config,
            &contents,
            &path,
            &state_dir,
        );
    }
    let mut api = api_for(&config)?;
    if matches!(args.command, Command::Check) {
        match fetch(&config, timezone, &state_dir, &mut api, Utc::now())? {
            Some(s) => println!("퇴근 예정: {}", s.due.with_timezone(&s.zone)),
            None => println!("오늘 알림 대상 출근 기록이 없습니다 (미출근/비근무일/퇴근 완료)."),
        }
        return Ok(());
    }
    let dry_run = matches!(args.command, Command::Run { dry_run: true });
    std::fs::create_dir_all(&state_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state_dir.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("another day-chime daemon is running")?;
    let state_path = state_dir.join("notifications.json");
    let mut state = State::load(&state_path)?;
    let mut delivery = Delivery::default();
    let mut watcher = config_reload::Watch::new(contents)?;
    let mut refresh_log = RefreshLog::default();
    let mut notification_errors = ErrorLog::default();
    let mut persistence_errors = ErrorLog::default();
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })?;
    let mut next_poll = Instant::now();
    let mut cached: Option<Schedule> = None;
    let mut fetched: Option<Instant> = None;
    let mut last_day = Utc::now().with_timezone(&timezone).date_naive();
    println!("day-chime running (dry_run={dry_run})");
    while running.load(Ordering::SeqCst) {
        let now = Utc::now();
        match watcher.poll(&path, prepare) {
            config_reload::Update::Applied((new_config, new_timezone, new_api)) => {
                config = new_config;
                timezone = new_timezone;
                api = new_api;
                cached = None;
                fetched = None;
                next_poll = Instant::now();
                println!("configuration reloaded");
            }
            config_reload::Update::Rejected(error) => {
                eprintln!("configuration update ignored; keeping last valid settings: {error}")
            }
            config_reload::Update::Recovered => {
                println!("configuration file access recovered; keeping active settings")
            }
            config_reload::Update::Unchanged => {}
        }
        let day = now.with_timezone(&timezone).date_naive();
        if day != last_day {
            cached = None;
            fetched = None;
            next_poll = Instant::now();
            last_day = day;
        }
        if Instant::now() >= next_poll {
            let result = fetch(&config, timezone, &state_dir, &mut api, Utc::now());
            if refresh_log.changed(RefreshStatus::from_result(&result)) {
                match &result {
                    Ok(Some(s)) => println!("퇴근 예정: {}", s.due.with_timezone(&s.zone)),
                    Ok(None) => println!("현재 알림 대상 출근 기록이 없습니다."),
                    Err(error) => eprintln!("attendance refresh failed: {error:#}"),
                }
            }
            match result {
                Ok(schedule) => {
                    cached = schedule;
                    fetched = Some(Instant::now());
                }
                Err(_) => {
                    cached = None;
                    fetched = None;
                }
            }
            next_poll = Instant::now()
                + Duration::from_secs(if config.source == "browser" {
                    5
                } else {
                    config.poll_seconds
                });
        }
        if let Some(s) = &cached {
            if fetched.is_some_and(|t| t.elapsed() <= Duration::from_secs(config.poll_seconds)) {
                let reminders = day_chime::pending_reminders(s, &config, &state, Utc::now());
                // Validate corrections immediately before delivery without depending on browser liveness.
                let current = reminders.is_empty()
                    || config.source != "browser"
                    || matches!(fetch(&config, timezone, &state_dir, &mut api, Utc::now()), Ok(Some(latest)) if latest.key == s.key && latest.due == s.due);
                if current {
                    for reminder in reminders {
                        let result = delivery.attempt(
                            &reminder,
                            &mut state,
                            Instant::now(),
                            dry_run,
                            |reminder| {
                                if dry_run {
                                    println!(
                                        "[dry-run] {}: {}",
                                        reminder.summary,
                                        reminder.body.replace('\n', ", ")
                                    );
                                    Ok(())
                                } else {
                                    day_chime::notify_with_summary(
                                        &reminder.summary,
                                        &reminder.body,
                                    )
                                }
                            },
                        );
                        match result {
                            DeliveryOutcome::Delivered => {
                                notification_errors.recovered(&reminder.key);
                                println!(
                                    "notification {}: {} ({})",
                                    if dry_run { "simulated" } else { "delivered" },
                                    reminder.summary,
                                    reminder.key
                                );
                            }
                            DeliveryOutcome::Failed { error, retry_after } => {
                                if notification_errors.changed(&reminder.key, format!("{error:#}"))
                                {
                                    eprintln!(
                                        "notification failed; retry in {}s: {error:#}",
                                        retry_after.as_secs()
                                    );
                                }
                            }
                            DeliveryOutcome::Skipped => {}
                        }
                    }
                } else {
                    cached = None;
                    fetched = None;
                    next_poll = Instant::now();
                }
            }
        }
        // Persist independently of attendance availability, including after corrections/reloads.
        match delivery.flush(&state, Instant::now(), |state| state.save(&state_path)) {
            SaveOutcome::Saved => {
                persistence_errors.recovered("state");
                println!("notification state saved");
            }
            SaveOutcome::Failed { error, retry_after } => {
                if persistence_errors.changed("state", format!("{error:#}")) {
                    eprintln!(
                        "notification state save failed; retry in {}s: {error:#}",
                        retry_after.as_secs()
                    );
                }
            }
            SaveOutcome::Idle => {}
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_errors_are_logged_only_on_first_failure_change_or_after_recovery() {
        let mut log = ErrorLog::default();
        assert!(log.changed("arrival:day", "service unavailable".into()));
        assert!(!log.changed("arrival:day", "service unavailable".into()));
        assert!(log.changed("departure:day", "service unavailable".into()));
        assert!(log.changed("arrival:day", "permission denied".into()));
        assert!(!log.changed("arrival:day", "permission denied".into()));
        log.recovered("arrival:day");
        assert!(log.changed("arrival:day", "permission denied".into()));
        assert!(!log.changed("departure:day", "service unavailable".into()));
        let mut persistence = ErrorLog::default();
        assert!(persistence.changed("state", "disk full".into()));
        assert!(!persistence.changed("state", "disk full".into()));
        persistence.recovered("state");
        assert!(persistence.changed("state", "disk full".into()));
    }
    #[test]
    fn schedule_logging_tracks_real_transitions_and_error_recovery() {
        let due = DateTime::parse_from_rfc3339("2026-11-04T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut log = RefreshLog::default();
        let scheduled = || RefreshStatus::Scheduled("same-day".into(), due, chrono_tz::Asia::Seoul);
        assert!(log.changed(scheduled()));
        assert!(!log.changed(scheduled()));
        assert!(log.changed(RefreshStatus::Failed("missing snapshot".into())));
        assert!(!log.changed(RefreshStatus::Failed("missing snapshot".into())));
        assert!(log.changed(scheduled()));
        assert!(log.changed(RefreshStatus::Scheduled(
            "same-day".into(),
            due + chrono::Duration::minutes(1),
            chrono_tz::Asia::Seoul
        )));
        assert!(log.changed(RefreshStatus::Empty));
        assert!(!log.changed(RefreshStatus::Empty));
    }
}
