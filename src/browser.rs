//! Chrome/Brave native messaging transport. Never accesses cookies or credentials.
use crate::{Config, Schedule};
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    path::Path,
};

const MAX_MESSAGE: usize = 65_536;
pub const SNAPSHOT_FILE: &str = "browser-attendance.json";
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    schema_version: u8,
    source_url: String,
    #[serde(default = "yes")]
    available: bool,
    working_day: Option<NaiveDate>,
    clock_in: Option<String>,
    clock_out: Option<String>,
    observed_at: DateTime<Utc>,
    #[serde(default)]
    error: Option<String>,
}
fn yes() -> bool {
    true
}
#[derive(Deserialize, Serialize)]
struct Stored {
    snapshot: Snapshot,
    received_at: DateTime<Utc>,
}

pub fn validate_source_url(value: &str) -> Result<()> {
    let url = reqwest::Url::parse(value).context("invalid browser_url")?;
    let valid_company = url
        .host_str()
        .and_then(|host| host.strip_suffix(".daouoffice.com"))
        .is_some_and(|company| {
            !company.is_empty()
                && company.len() <= 63
                && !company.starts_with('-')
                && !company.ends_with('-')
                && company
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        });
    if url.scheme() != "https"
        || !valid_company
        || url.path() != "/home"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.as_str() != value
    {
        bail!("browser_url must be https://<company>.daouoffice.com/home without query or credentials");
    }
    Ok(())
}
fn clock(value: &Option<String>) -> Result<Option<NaiveTime>> {
    value
        .as_ref()
        .map(|s| {
            if s.len() != 8 {
                bail!("invalid clock format");
            }
            NaiveTime::parse_from_str(s, "%H:%M:%S").context("invalid clock time")
        })
        .transpose()
}
fn validate(snapshot: &Snapshot, config: &Config) -> Result<()> {
    if snapshot.schema_version != 1 || snapshot.source_url != config.browser_url {
        bail!("unsupported schema or unexpected source URL");
    }
    validate_source_url(&snapshot.source_url)?;
    if snapshot.error.as_ref().is_some_and(|e| e.len() > 100) {
        bail!("error code too long");
    }
    clock(&snapshot.clock_in)?;
    clock(&snapshot.clock_out)?;
    if snapshot.available && snapshot.working_day.is_none() {
        bail!("available snapshot needs working_day");
    }
    if !snapshot.available && (snapshot.clock_in.is_some() || snapshot.clock_out.is_some()) {
        bail!("unavailable snapshot contains clock times");
    }
    Ok(())
}

/// Empty EOF is normal; partial headers/bodies and oversized frames are errors.
pub fn read_message(reader: &mut impl Read) -> Result<Option<Vec<u8>>> {
    let mut length = [0; 4];
    if reader.read(&mut length[..1])? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut length[1..])
        .context("truncated native message header")?;
    let size = u32::from_ne_bytes(length) as usize;
    if size == 0 || size > MAX_MESSAGE {
        bail!("native message length outside 1..65536");
    }
    let mut payload = vec![0; size];
    reader
        .read_exact(&mut payload)
        .context("truncated native message")?;
    Ok(Some(payload))
}
pub fn write_message(writer: &mut impl Write, payload: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(payload)?;
    writer.write_all(&(bytes.len() as u32).to_ne_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn save_message(bytes: &[u8], config: &Config, directory: &Path) -> Result<()> {
    let snapshot: Snapshot = serde_json::from_slice(bytes).context("invalid snapshot JSON")?;
    validate(&snapshot, config)?;
    let stored = Stored {
        snapshot,
        received_at: Utc::now(),
    };
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(directory.join("browser-host.lock"))?;
    lock.lock_exclusive()?;
    // A missing widget or logged-out page is a collection failure, not evidence
    // that a previously observed check-in was cancelled.
    if !stored.snapshot.available {
        if let Ok(bytes) = std::fs::read(directory.join(SNAPSHOT_FILE)) {
            if bytes.len() <= MAX_MESSAGE + 1024 {
                if let Ok(previous) = serde_json::from_slice::<Stored>(&bytes) {
                    if previous.snapshot.available && validate(&previous.snapshot, config).is_ok() {
                        return Ok(());
                    }
                }
            }
        }
    }
    validate_receipt(&stored, config, stored.received_at)?;
    let temporary = directory.join("browser-attendance.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&serde_json::to_vec(&stored)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temporary, directory.join(SNAPSHOT_FILE))?;
    Ok(())
}

pub fn host(
    reader: &mut impl Read,
    writer: &mut impl Write,
    config: &Config,
    directory: &Path,
) -> Result<()> {
    while let Some(message) = read_message(reader)? {
        let result = save_message(&message, config, directory);
        // Do not echo input or sensitive values in error acknowledgements.
        write_message(writer, &serde_json::json!({"ok":result.is_ok()}))?;
        if result.is_err() {
            eprintln!("browser snapshot rejected; verify extension and configuration");
        }
    }
    Ok(())
}

fn validate_receipt(stored: &Stored, config: &Config, now: DateTime<Utc>) -> Result<()> {
    if stored.received_at > now + Duration::seconds(30) {
        bail!("browser receipt is in the future");
    }
    if stored.snapshot.available {
        let zone = config.validate()?;
        let received_day = stored.received_at.with_timezone(&zone).date_naive();
        let day = stored.snapshot.working_day.context("missing working_day")?;
        if day != received_day && Some(day) != received_day.pred_opt() {
            bail!("browser working day does not match its local receipt date");
        }
    }
    Ok(())
}

pub fn current_schedule(
    directory: &Path,
    config: &Config,
    now: DateTime<Utc>,
) -> Result<Option<Schedule>> {
    let bytes = std::fs::read(directory.join(SNAPSHOT_FILE)).context(
        "browser snapshot missing: install/connect the extension and open DaouOffice home",
    )?;
    if bytes.len() > MAX_MESSAGE + 1024 {
        bail!("browser snapshot file too large");
    }
    let stored: Stored = serde_json::from_slice(&bytes).context("invalid browser snapshot file")?;
    validate(&stored.snapshot, config)?;
    validate_receipt(&stored, config, now)?;
    let snapshot = stored.snapshot;
    if !snapshot.available {
        return Ok(None);
    }
    let zone = config.validate()?;
    let today = now.with_timezone(&zone).date_naive();
    let day = snapshot.working_day.context("missing working_day")?;
    if day != today && day != today.pred_opt().context("date underflow")? {
        bail!("browser snapshot has an outdated or future working day");
    }
    if snapshot.clock_out.is_some() {
        return Ok(None);
    }
    let Some(start) = clock(&snapshot.clock_in)? else {
        return Ok(None);
    };
    let start = zone
        .from_local_datetime(&day.and_time(start))
        .single()
        .context("ambiguous or nonexistent browser clock-in")?;
    if start.with_timezone(&Utc) > now + Duration::seconds(60) {
        bail!("browser clock-in is in the future");
    }
    let due =
        start.with_timezone(&Utc) + Duration::minutes(config.work_minutes + config.break_minutes);
    if day != today && due.with_timezone(&zone).date_naive() != today {
        return Ok(None);
    }
    Ok(Some(Schedule {
        key: format!("browser:{}:{}", config.browser_url, day),
        due,
        zone,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> serde_json::Value {
        serde_json::json!({"schema_version":1,"source_url":"https://example.daouoffice.com/home","working_day":"2026-11-03","clock_in":"09:50:00","clock_out":null,"observed_at":"2000-01-01T00:00:00Z"})
    }
    fn frame(value: &serde_json::Value) -> Vec<u8> {
        let bytes = serde_json::to_vec(value).unwrap();
        let mut frame = (bytes.len() as u32).to_ne_bytes().to_vec();
        frame.extend(bytes);
        frame
    }
    #[test]
    fn framing_rejects_partial_and_oversized_messages() {
        assert!(read_message(&mut &[][..]).unwrap().is_none());
        for bytes in [
            vec![1, 0],
            65537_u32.to_ne_bytes().to_vec(),
            vec![2, 0, 0, 0, b'a'],
        ] {
            assert!(read_message(&mut bytes.as_slice()).is_err());
        }
    }
    #[test]
    fn browser_host_acks_and_stamps_local_receipt_not_observed_time() {
        let directory = tempfile::tempdir().unwrap();
        let mut output = Vec::new();
        let mut payload = snapshot();
        payload["working_day"] = Utc::now()
            .with_timezone(&chrono_tz::Asia::Seoul)
            .date_naive()
            .to_string()
            .into();
        host(
            &mut frame(&payload).as_slice(),
            &mut output,
            &Config::default(),
            directory.path(),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &read_message(&mut output.as_slice()).unwrap().unwrap()
            )
            .unwrap()["ok"],
            true
        );
        let stored: Stored =
            serde_json::from_slice(&std::fs::read(directory.path().join(SNAPSHOT_FILE)).unwrap())
                .unwrap();
        assert!(
            Utc::now()
                .signed_duration_since(stored.received_at)
                .num_seconds()
                < 5
        );
    }
    #[test]
    fn wrong_origin_and_malformed_clocks_are_rejected() {
        for (field, value) in [
            ("source_url", "https://evil.example/home"),
            ("clock_in", "25:00:00"),
            ("clock_out", "9:20:00"),
        ] {
            let mut payload = snapshot();
            payload[field] = value.into();
            assert!(validate(
                &serde_json::from_value(payload).unwrap(),
                &Config::default()
            )
            .is_err());
        }
        assert!(validate_source_url("https://example.daouoffice.com.evil.example/home").is_err());
    }
    #[test]
    fn browser_default_needs_no_login_id_or_api_credentials() {
        assert!(Config::default().validate().is_ok());
    }
    #[test]
    fn configured_company_requires_one_canonical_https_tenant() {
        assert!(validate_source_url("https://my-team.daouoffice.com/home").is_ok());
        for url in [
            "https://other.example.daouoffice.com/home",
            "https://-example.daouoffice.com/home",
            "https://example-.daouoffice.com/home",
            "https://example.daouoffice.com:443/home",
            "https://example.daouoffice.com:8080/home",
            "http://example.daouoffice.com/home",
            "https://user:password@example.daouoffice.com/home",
            "https://example.daouoffice.com/home?secret=value",
            "https://example.daouoffice.com/home#section",
        ] {
            assert!(validate_source_url(url).is_err(), "accepted {url}");
        }
    }
    #[test]
    fn schedule_keeps_daily_snapshot_and_honors_completion() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::default();
        let now: DateTime<Utc> = "2026-11-03T03:00:00Z".parse().unwrap();
        let write = |snapshot: serde_json::Value, received_at: DateTime<Utc>| {
            std::fs::write(
                directory.path().join(SNAPSHOT_FILE),
                serde_json::to_vec(
                    &serde_json::json!({"snapshot":snapshot,"received_at":received_at}),
                )
                .unwrap(),
            )
            .unwrap();
        };
        write(snapshot(), now);
        assert_eq!(
            current_schedule(directory.path(), &config, now)
                .unwrap()
                .unwrap()
                .due
                .to_rfc3339(),
            "2026-11-03T09:50:00+00:00"
        );
        assert!(
            current_schedule(directory.path(), &config, now + Duration::hours(8))
                .unwrap()
                .is_some()
        );
        let mut payload = snapshot();
        payload["clock_out"] = "18:00:00".into();
        write(payload, now);
        assert!(current_schedule(directory.path(), &config, now)
            .unwrap()
            .is_none());
        let mut payload = snapshot();
        payload["available"] = false.into();
        payload["working_day"] = serde_json::Value::Null;
        payload["clock_in"] = serde_json::Value::Null;
        write(payload, now);
        assert!(current_schedule(directory.path(), &config, now)
            .unwrap()
            .is_none());
    }
    fn store(directory: &Path, payload: serde_json::Value, received: &str) {
        std::fs::write(
            directory.join(SNAPSHOT_FILE),
            serde_json::to_vec(&serde_json::json!({"snapshot":payload,"received_at":received}))
                .unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn transient_unavailable_does_not_replace_daily_record_or_refresh_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut payload = snapshot();
        payload["working_day"] = now
            .with_timezone(&chrono_tz::Asia::Seoul)
            .date_naive()
            .to_string()
            .into();
        save_message(
            &serde_json::to_vec(&payload).unwrap(),
            &Config::default(),
            directory.path(),
        )
        .unwrap();
        let saved = std::fs::read(directory.path().join(SNAPSHOT_FILE)).unwrap();
        payload["available"] = false.into();
        payload["working_day"] = serde_json::Value::Null;
        payload["clock_in"] = serde_json::Value::Null;
        // Each host invocation models a fresh native-host process after browser restart.
        for _ in 0..2 {
            let mut output = Vec::new();
            host(
                &mut frame(&payload).as_slice(),
                &mut output,
                &Config::default(),
                directory.path(),
            )
            .unwrap();
            assert_eq!(
                std::fs::read(directory.path().join(SNAPSHOT_FILE)).unwrap(),
                saved
            );
        }
    }
    #[test]
    fn explicit_clockout_or_removed_clockin_replaces_cached_record() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::default();
        let now = Utc::now();
        let mut payload = snapshot();
        payload["working_day"] = now
            .with_timezone(&chrono_tz::Asia::Seoul)
            .date_naive()
            .to_string()
            .into();
        for completed in [true, false] {
            let mut correction = payload.clone();
            if completed {
                correction["clock_out"] = "18:50:00".into();
            } else {
                correction["clock_in"] = serde_json::Value::Null;
            }
            save_message(
                &serde_json::to_vec(&payload).unwrap(),
                &config,
                directory.path(),
            )
            .unwrap();
            save_message(
                &serde_json::to_vec(&correction).unwrap(),
                &config,
                directory.path(),
            )
            .unwrap();
            assert!(current_schedule(directory.path(), &config, now)
                .unwrap()
                .is_none());
        }
    }
    #[test]
    fn daily_cache_expires_by_work_date_and_keeps_only_overnight_due_today() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::default();
        let now: DateTime<Utc> = "2026-11-04T01:00:00Z".parse().unwrap();
        store(directory.path(), snapshot(), "2026-11-03T03:00:00Z");
        assert!(current_schedule(directory.path(), &config, now)
            .unwrap()
            .is_none());
        let mut overnight = snapshot();
        overnight["clock_in"] = "18:00:00".into();
        store(directory.path(), overnight, "2026-11-03T09:00:00Z");
        assert_eq!(
            current_schedule(directory.path(), &config, now)
                .unwrap()
                .unwrap()
                .due
                .to_rfc3339(),
            "2026-11-03T18:00:00+00:00"
        );
        let after: DateTime<Utc> = "2026-11-05T01:00:00Z".parse().unwrap();
        assert!(current_schedule(directory.path(), &config, after).is_err());
    }
    #[test]
    fn rejects_future_receipt_and_work_date_inconsistent_with_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let now: DateTime<Utc> = "2026-11-03T03:00:00Z".parse().unwrap();
        store(directory.path(), snapshot(), "2026-11-03T04:00:00Z");
        assert!(current_schedule(directory.path(), &Config::default(), now).is_err());
        let mut old = snapshot();
        old["working_day"] = "2000-01-01".into();
        store(directory.path(), old, "2026-11-03T03:00:00Z");
        assert!(current_schedule(directory.path(), &Config::default(), now).is_err());
    }
    #[test]
    fn later_valid_clockin_correction_changes_saved_due_time() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config::default();
        let now = Utc::now();
        let mut payload = snapshot();
        payload["working_day"] = now
            .with_timezone(&chrono_tz::Asia::Seoul)
            .date_naive()
            .to_string()
            .into();
        payload["clock_in"] = "00:00:00".into();
        save_message(
            &serde_json::to_vec(&payload).unwrap(),
            &config,
            directory.path(),
        )
        .unwrap();
        let before = current_schedule(directory.path(), &config, now)
            .unwrap()
            .unwrap();
        payload["clock_in"] = "00:01:00".into();
        save_message(
            &serde_json::to_vec(&payload).unwrap(),
            &config,
            directory.path(),
        )
        .unwrap();
        let after = current_schedule(directory.path(), &config, now)
            .unwrap()
            .unwrap();
        assert_eq!(
            after.due.signed_duration_since(before.due),
            Duration::minutes(1)
        );
        assert_eq!(after.key, before.key);
    }
}
