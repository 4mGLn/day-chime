use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use day_chime::{Api, Config, State};
use fs2::FileExt;
use std::{
    path::PathBuf,
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
    let config: Config = toml::from_str(
        &std::fs::read_to_string(&path)
            .with_context(|| format!("read config {}", path.display()))?,
    )?;
    let timezone = config.validate()?;
    if matches!(args.command, Command::BrowserHost) {
        return day_chime::browser::host(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            &config,
            &state_dir,
        );
    }
    let mut api = if config.source == "api" {
        Some(Api::new(
            std::env::var("DAOU_CLIENT_ID").context("set DAOU_CLIENT_ID")?,
            std::env::var("DAOU_CLIENT_SECRET").context("set DAOU_CLIENT_SECRET")?,
        )?)
    } else {
        None
    };
    let mut fetch = || -> Result<Option<day_chime::Schedule>> {
        if let Some(api) = api.as_mut() {
            day_chime::current_schedule(
                api,
                &config,
                Utc::now().with_timezone(&timezone).date_naive(),
            )
        } else {
            day_chime::browser::current_schedule(&state_dir, &config, Utc::now())
        }
    };
    if matches!(args.command, Command::Check) {
        match fetch()? {
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
    let running = Arc::new(AtomicBool::new(true));
    let signal = running.clone();
    ctrlc::set_handler(move || {
        signal.store(false, Ordering::SeqCst);
    })?;
    let mut next_poll = Instant::now();
    let mut cached: Option<day_chime::Schedule> = None;
    let mut fetched = None;
    let mut last_day = Utc::now().with_timezone(&timezone).date_naive();
    println!("day-chime running (dry_run={dry_run})");
    while running.load(Ordering::SeqCst) {
        let now = Utc::now();
        let day = now.with_timezone(&timezone).date_naive();
        if day != last_day {
            cached = None;
            fetched = None;
            next_poll = Instant::now();
            last_day = day;
        }
        if Instant::now() >= next_poll {
            match fetch() {
                Ok(schedule) => {
                    cached = schedule;
                    fetched = Some(Instant::now());
                    if let Some(s) = &cached {
                        println!("퇴근 예정: {}", s.due.with_timezone(&s.zone));
                    }
                }
                Err(e) => {
                    eprintln!("attendance refresh failed: {e:#}");
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
                // Re-read the browser record before delivery to honor valid corrections,
                // completion and the working-date boundary; browser liveness is unnecessary.
                if !reminders.is_empty()
                    && config.source == "browser"
                    && !matches!(fetch(), Ok(Some(latest)) if latest.key == s.key && latest.due == s.due)
                {
                    cached = None;
                    continue;
                }
                for reminder in reminders {
                    let result = if dry_run {
                        println!(
                            "[dry-run] {}: {}",
                            reminder.summary,
                            reminder.body.replace('\n', ", ")
                        );
                        Ok(())
                    } else {
                        day_chime::notify_with_summary(&reminder.summary, &reminder.body)
                    };
                    match result {
                        Ok(()) => {
                            println!(
                                "notification {}: {} ({})",
                                if dry_run { "simulated" } else { "delivered" },
                                reminder.summary,
                                reminder.key
                            );
                            state.notified.insert(reminder.key);
                            if !dry_run {
                                state.save(&state_path)?;
                            }
                        }
                        Err(e) => eprintln!("{e:#}"),
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}
