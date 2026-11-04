//! Config updates are prepared completely before replacing the active runtime.
use anyhow::Result;
use day_chime::{config_document, Config};
use std::{collections::BTreeSet, path::Path};

pub fn parse(bytes: &[u8]) -> Result<Config> {
    Ok(config_document::parse(bytes)?.config)
}

pub enum Update<T> {
    Unchanged,
    Applied(T),
    Rejected(String),
    Recovered,
}

pub struct Watch {
    observed: Vec<u8>,
    required_fields: BTreeSet<String>,
    candidate_error: Option<String>,
    read_error: Option<String>,
}
impl Watch {
    pub fn new(initial: Vec<u8>) -> Result<Self> {
        let required_fields = config_document::parse(&initial)?.fields;
        Ok(Self {
            observed: initial,
            required_fields,
            candidate_error: None,
            read_error: None,
        })
    }
    pub fn poll<T>(&mut self, path: &Path, prepare: impl FnOnce(Config) -> Result<T>) -> Update<T> {
        let contents = match std::fs::read(path) {
            Ok(contents) => contents,
            Err(error) => {
                let message = format!("cannot read configuration file ({:?})", error.kind());
                if self.read_error.as_ref() == Some(&message) {
                    return Update::Unchanged;
                }
                self.read_error = Some(message.clone());
                return Update::Rejected(message);
            }
        };
        let read_recovered = self.read_error.take().is_some();
        if contents == self.observed {
            return if read_recovered {
                match &self.candidate_error {
                    Some(error) => Update::Rejected(error.clone()),
                    None => Update::Recovered,
                }
            } else {
                Update::Unchanged
            };
        }
        self.observed = contents;
        let candidate = config_document::parse_reload(&self.observed, &self.required_fields)
            .and_then(|document| {
                let prepared = prepare(document.config)?;
                Ok((prepared, document.fields))
            });
        match candidate {
            Ok((candidate, fields)) => {
                self.required_fields = fields;
                self.candidate_error = None;
                Update::Applied(candidate)
            }
            Err(error) => {
                let message = error.to_string();
                if !read_recovered && self.candidate_error.as_ref() == Some(&message) {
                    return Update::Unchanged;
                }
                self.candidate_error = Some(message.clone());
                Update::Rejected(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prepare(config: Config) -> Result<(Config, ())> {
        Ok((config, ()))
    }
    #[test]
    fn valid_edit_invalid_edit_and_recovery_are_transactional() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut watcher = Watch::new(b"work_minutes = 480".to_vec()).unwrap();
        let mut active = 480;
        assert_eq!(active, 480);
        std::fs::write(&path, b"work_minutes = 420").unwrap();
        if let Update::Applied((config, _)) = watcher.poll(&path, prepare) {
            active = config.work_minutes;
        } else {
            panic!("expected update");
        }
        assert_eq!(active, 420);
        std::fs::write(&path, b"work_minutes = -1").unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Rejected(_)));
        assert_eq!(active, 420);
        assert!(matches!(
            watcher.poll(&path, |_| panic!("unchanged invalid file reparsed")),
            Update::<()>::Unchanged
        ));
        std::fs::write(&path, b"work_minutes = 450").unwrap();
        if let Update::Applied((config, _)) = watcher.poll(&path, prepare) {
            active = config.work_minutes;
        } else {
            panic!("expected recovery");
        }
        assert_eq!(active, 450);
    }
    #[test]
    fn missing_file_error_is_logged_once_and_same_content_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut watcher = Watch::new(b"source='browser'".to_vec()).unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Rejected(_)));
        assert!(matches!(watcher.poll(&path, prepare), Update::Unchanged));
        std::fs::write(&path, b"source='browser'").unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Recovered));
    }
    #[test]
    fn resource_preparation_must_succeed_before_update_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut watcher = Watch::new(b"source='browser'".to_vec()).unwrap();
        std::fs::write(&path, b"source = 'api'\nlogin_id = 'me'").unwrap();
        let update = watcher.poll(&path, |candidate| {
            assert_eq!(candidate.source, "api");
            Err::<Config, _>(anyhow::anyhow!("set DAOU_CLIENT_ID"))
        });
        assert!(matches!(update, Update::Rejected(message) if message == "set DAOU_CLIENT_ID"));
        std::fs::write(&path, b"source = 'browser'").unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Applied(_)));
    }
    #[test]
    fn truncated_or_empty_reload_must_not_restore_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = b"source='browser'\nbrowser_url='https://other.daouoffice.com/home'\nwork_minutes=420\n";
        let mut watcher = Watch::new(original.to_vec()).unwrap();
        for contents in ["", "# being saved", "source='browser'\n"] {
            std::fs::write(&path, contents).unwrap();
            assert!(matches!(
                watcher.poll(&path, prepare),
                Update::Rejected(_) | Update::Unchanged
            ));
        }
    }
    #[test]
    fn only_accepted_new_fields_become_required_on_future_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut watcher = Watch::new(b"source='browser'".to_vec()).unwrap();
        std::fs::write(&path, "source='browser'\nwork_minutes=420").unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Applied(_)));
        std::fs::write(&path, "source='browser'").unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Rejected(_)));
        // The invalid candidate adds break_minutes but must not make it required.
        std::fs::write(
            &path,
            "source='browser'\nwork_minutes=420\nbreak_minutes=-1",
        )
        .unwrap();
        assert!(matches!(watcher.poll(&path, prepare), Update::Rejected(_)));
        std::fs::write(&path, "source='browser'\nwork_minutes=450").unwrap();
        match watcher.poll(&path, prepare) {
            Update::Applied((config, _)) => assert_eq!(config.work_minutes, 450),
            _ => panic!("complete recovery rejected after incomplete edits"),
        }
    }
    #[test]
    fn parser_does_not_echo_invalid_source_contents() {
        let error = match parse(b"unknown_secret = 'never-log-this'") {
            Ok(_) => panic!("unknown field accepted"),
            Err(e) => e.to_string(),
        };
        assert_eq!(error, "invalid TOML configuration");
        assert!(!error.contains("never-log-this"));
    }
}
