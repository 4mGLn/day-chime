//! In-process delivery recovery. Persisted daily keys remain wire-compatible.
use crate::{Reminder, State};
use anyhow::Result;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

const RETRY_SECONDS: [u64; 10] = [3, 5, 15, 20, 30, 45, 60, 120, 180, 300];

#[derive(Default)]
struct Retry {
    failures: usize,
    next_attempt: Option<Instant>,
}
impl Retry {
    fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|deadline| now >= deadline)
    }
    fn failed(&mut self, now: Instant) -> Duration {
        let delay = Duration::from_secs(RETRY_SECONDS[self.failures.min(RETRY_SECONDS.len() - 1)]);
        self.failures = self.failures.saturating_add(1);
        self.next_attempt = Some(now + delay);
        delay
    }
}

pub enum DeliveryOutcome {
    Skipped,
    Delivered,
    Failed {
        error: anyhow::Error,
        retry_after: Duration,
    },
}
pub enum SaveOutcome {
    /// Nothing is dirty, or the next persistence retry is not due yet.
    Idle,
    Saved,
    Failed {
        error: anyhow::Error,
        retry_after: Duration,
    },
}

/// Retain this object and `State` across schedule/configuration refreshes.
/// Notification success is remembered before disk persistence, so a temporary
/// write failure does not terminate the daemon or redeliver the same event.
/// A process crash before a successful save can still cause redelivery.
#[derive(Default)]
pub struct Delivery {
    notifications: BTreeMap<String, Retry>,
    dirty: bool,
    persistence: Retry,
}
impl Delivery {
    pub fn attempt(
        &mut self,
        reminder: &Reminder,
        state: &mut State,
        now: Instant,
        dry_run: bool,
        notify: impl FnOnce(&Reminder) -> Result<()>,
    ) -> DeliveryOutcome {
        self.attempt_with_clock(reminder, state, now, dry_run, notify, Instant::now)
    }

    fn attempt_with_clock(
        &mut self,
        reminder: &Reminder,
        state: &mut State,
        now: Instant,
        dry_run: bool,
        notify: impl FnOnce(&Reminder) -> Result<()>,
        completed: impl FnOnce() -> Instant,
    ) -> DeliveryOutcome {
        if state.notified.contains(&reminder.key) {
            self.notifications.remove(&reminder.key);
            return DeliveryOutcome::Skipped;
        }
        let retry = self.notifications.entry(reminder.key.clone()).or_default();
        if !retry.ready(now) {
            return DeliveryOutcome::Skipped;
        }
        match notify(reminder) {
            Ok(()) => {
                self.notifications.remove(&reminder.key);
                state.notified.insert(reminder.key.clone());
                if !dry_run {
                    self.dirty = true;
                }
                DeliveryOutcome::Delivered
            }
            Err(error) => DeliveryOutcome::Failed {
                error,
                // Notification APIs may block; count the backoff after they return.
                retry_after: retry.failed(completed()),
            },
        }
    }

    /// Call each loop, including when there is no current attendance schedule.
    /// New deliveries accumulate in the same dirty state without resetting an
    /// existing persistence backoff, and all are saved together on recovery.
    pub fn flush(
        &mut self,
        state: &State,
        now: Instant,
        save: impl FnOnce(&State) -> Result<()>,
    ) -> SaveOutcome {
        self.flush_with_clock(state, now, save, Instant::now)
    }

    fn flush_with_clock(
        &mut self,
        state: &State,
        now: Instant,
        save: impl FnOnce(&State) -> Result<()>,
        completed: impl FnOnce() -> Instant,
    ) -> SaveOutcome {
        if !self.dirty || !self.persistence.ready(now) {
            return SaveOutcome::Idle;
        }
        match save(state) {
            Ok(()) => {
                self.dirty = false;
                self.persistence = Retry::default();
                SaveOutcome::Saved
            }
            Err(error) => SaveOutcome::Failed {
                error,
                // A slow write must not consume its own recovery delay.
                retry_after: self.persistence.failed(completed()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    impl Delivery {
        fn attempt_at(
            &mut self,
            reminder: &Reminder,
            state: &mut State,
            now: Instant,
            dry_run: bool,
            notify: impl FnOnce(&Reminder) -> Result<()>,
        ) -> DeliveryOutcome {
            self.attempt_with_clock(reminder, state, now, dry_run, notify, || now)
        }
        fn flush_at(
            &mut self,
            state: &State,
            now: Instant,
            save: impl FnOnce(&State) -> Result<()>,
        ) -> SaveOutcome {
            self.flush_with_clock(state, now, save, || now)
        }
    }
    fn reminder(key: &str) -> Reminder {
        Reminder {
            key: key.into(),
            summary: "summary".into(),
            body: "body".into(),
        }
    }
    #[test]
    fn notification_backoff_matches_every_requested_step_and_caps() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let mut now = Instant::now();
        for seconds in RETRY_SECONDS.into_iter().chain([300, 300]) {
            match delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| {
                bail!("offline")
            }) {
                DeliveryOutcome::Failed { retry_after, .. } => {
                    assert_eq!(retry_after.as_secs(), seconds)
                }
                _ => panic!("failure expected"),
            }
            assert!(!state.notified.contains("due"));
            assert!(matches!(
                delivery.attempt_at(
                    &reminder("due"),
                    &mut state,
                    now + Duration::from_secs(seconds) - Duration::from_nanos(1),
                    false,
                    |_| panic!("too early")
                ),
                DeliveryOutcome::Skipped
            ));
            now += Duration::from_secs(seconds);
        }
        assert!(matches!(
            delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| Ok(())),
            DeliveryOutcome::Delivered
        ));
        assert!(state.notified.contains("due"));
        assert!(delivery.notifications.is_empty());
        assert!(matches!(
            delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| panic!(
                "duplicate"
            )),
            DeliveryOutcome::Skipped
        ));
    }
    #[test]
    fn event_failures_do_not_block_other_events() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let now = Instant::now();
        delivery.attempt_at(&reminder("arrival"), &mut state, now, false, |_| {
            bail!("offline")
        });
        assert!(matches!(
            delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| Ok(())),
            DeliveryOutcome::Delivered
        ));
        assert!(!state.notified.contains("arrival"));
        assert!(state.notified.contains("due"));
    }
    #[test]
    fn persistence_failure_preserves_delivery_and_coalesces_later_events() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let now = Instant::now();
        delivery.attempt_at(&reminder("arrival"), &mut state, now, false, |_| Ok(()));
        assert!(
            matches!(delivery.flush_at(&state, now, |_| bail!("disk full")), SaveOutcome::Failed { retry_after, .. } if retry_after.as_secs() == 3)
        );
        assert!(matches!(
            delivery.attempt_at(&reminder("arrival"), &mut state, now, false, |_| panic!(
                "must not redeliver"
            )),
            DeliveryOutcome::Skipped
        ));
        delivery.attempt_at(
            &reminder("due"),
            &mut state,
            now + Duration::from_secs(1),
            false,
            |_| Ok(()),
        );
        assert!(matches!(
            delivery.flush_at(&state, now + Duration::from_secs(2), |_| panic!(
                "too early"
            )),
            SaveOutcome::Idle
        ));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notifications.json");
        assert!(matches!(
            delivery.flush_at(&state, now + Duration::from_secs(3), |s| s.save(&path)),
            SaveOutcome::Saved
        ));
        let saved = State::load(&path).unwrap();
        assert!(saved.notified.contains("arrival"));
        assert!(saved.notified.contains("due"));
        assert!(matches!(
            delivery.flush_at(&state, now + Duration::from_secs(60), |_| panic!(
                "already clean"
            )),
            SaveOutcome::Idle
        ));
    }
    #[test]
    fn persistence_backoff_caps_and_resets_after_recovery() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let mut now = Instant::now();
        delivery.attempt_at(&reminder("arrival"), &mut state, now, false, |_| Ok(()));
        for seconds in RETRY_SECONDS.into_iter().chain([300]) {
            assert!(
                matches!(delivery.flush_at(&state, now, |_| bail!("disk full")), SaveOutcome::Failed { retry_after, .. } if retry_after.as_secs() == seconds)
            );
            assert!(matches!(
                delivery.flush_at(
                    &state,
                    now + Duration::from_secs(seconds) - Duration::from_nanos(1),
                    |_| panic!("too early")
                ),
                SaveOutcome::Idle
            ));
            now += Duration::from_secs(seconds);
        }
        assert!(matches!(
            delivery.flush_at(&state, now, |_| Ok(())),
            SaveOutcome::Saved
        ));
        delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| Ok(()));
        assert!(
            matches!(delivery.flush_at(&state, now, |_| bail!("disk full again")), SaveOutcome::Failed { retry_after, .. } if retry_after.as_secs() == 3)
        );
    }
    #[test]
    fn real_write_failure_recovers_without_duplicate_notification() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let now = Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing-parent/notifications.json");
        delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| Ok(()));
        assert!(matches!(
            delivery.flush_at(&state, now, |s| s.save(&path)),
            SaveOutcome::Failed { .. }
        ));
        assert!(matches!(
            delivery.attempt_at(
                &reminder("due"),
                &mut state,
                now + Duration::from_secs(1),
                false,
                |_| panic!("duplicate")
            ),
            DeliveryOutcome::Skipped
        ));
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        assert!(matches!(
            delivery.flush_at(&state, now + Duration::from_secs(3), |s| s.save(&path)),
            SaveOutcome::Saved
        ));
        assert!(State::load(&path).unwrap().notified.contains("due"));
    }
    #[test]
    fn dry_run_deduplicates_in_memory_without_writing_state() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let now = Instant::now();
        assert!(matches!(
            delivery.attempt_at(&reminder("due"), &mut state, now, true, |_| Ok(())),
            DeliveryOutcome::Delivered
        ));
        assert!(matches!(
            delivery.attempt_at(&reminder("due"), &mut state, now, true, |_| panic!(
                "duplicate"
            )),
            DeliveryOutcome::Skipped
        ));
        assert!(matches!(
            delivery.flush_at(&state, now, |_| panic!("dry run must not save")),
            SaveOutcome::Idle
        ));
    }
    #[test]
    fn notification_deadline_is_based_on_failure_completion_not_stale_start() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let started = Instant::now() - Duration::from_secs(10);
        let completed_floor = Instant::now();
        assert!(matches!(
            delivery.attempt(&reminder("due"), &mut state, started, false, |_| bail!(
                "slow failure"
            )),
            DeliveryOutcome::Failed { .. }
        ));
        assert!(matches!(
            delivery.attempt_at(
                &reminder("due"),
                &mut state,
                completed_floor,
                false,
                |_| panic!("must wait after failure")
            ),
            DeliveryOutcome::Skipped
        ));
    }
    #[test]
    fn persistence_deadline_is_based_on_failure_completion_not_stale_start() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        delivery.attempt(&reminder("due"), &mut state, Instant::now(), false, |_| {
            Ok(())
        });
        let started = Instant::now() - Duration::from_secs(10);
        let completed_floor = Instant::now();
        assert!(matches!(
            delivery.flush(&state, started, |_| bail!("slow save failure")),
            SaveOutcome::Failed { .. }
        ));
        assert!(matches!(
            delivery.flush_at(&state, completed_floor, |_| panic!(
                "must wait after failed save"
            )),
            SaveOutcome::Idle
        ));
    }

    #[test]
    fn slow_notification_failure_waits_full_backoff_after_completion_at_each_step() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let mut now = Instant::now();
        for seconds in RETRY_SECONDS.into_iter().chain([300]) {
            // Model a blocking failure taking longer than every requested retry delay.
            let completed = now + Duration::from_secs(400);
            assert!(
                matches!(delivery.attempt_with_clock(&reminder("due"), &mut state, now, false, |_| bail!("slow failure"), || completed), DeliveryOutcome::Failed { retry_after, .. } if retry_after.as_secs() == seconds)
            );
            assert!(matches!(
                delivery.attempt_at(
                    &reminder("due"),
                    &mut state,
                    completed + Duration::from_secs(seconds) - Duration::from_nanos(1),
                    false,
                    |_| panic!("retry before full delay")
                ),
                DeliveryOutcome::Skipped
            ));
            now = completed + Duration::from_secs(seconds);
        }
        assert!(matches!(
            delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| Ok(())),
            DeliveryOutcome::Delivered
        ));
    }
    #[test]
    fn slow_save_failure_waits_full_backoff_after_completion_at_each_step() {
        let mut delivery = Delivery::default();
        let mut state = State::default();
        let mut now = Instant::now();
        delivery.attempt_at(&reminder("due"), &mut state, now, false, |_| Ok(()));
        for seconds in RETRY_SECONDS.into_iter().chain([300]) {
            let completed = now + Duration::from_secs(400);
            assert!(
                matches!(delivery.flush_with_clock(&state, now, |_| bail!("slow save failure"), || completed), SaveOutcome::Failed { retry_after, .. } if retry_after.as_secs() == seconds)
            );
            assert!(matches!(
                delivery.flush_at(
                    &state,
                    completed + Duration::from_secs(seconds) - Duration::from_nanos(1),
                    |_| panic!("retry before full delay")
                ),
                SaveOutcome::Idle
            ));
            now = completed + Duration::from_secs(seconds);
        }
        assert!(matches!(
            delivery.flush_at(&state, now, |_| Ok(())),
            SaveOutcome::Saved
        ));
    }
}
