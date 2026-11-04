//! Configuration parsing and safe reload field-presence policy, shared by both processes.
use crate::Config;
use anyhow::{bail, Result};
use std::collections::BTreeSet;

pub struct Document {
    pub config: Config,
    pub fields: BTreeSet<String>,
}

pub fn parse(bytes: &[u8]) -> Result<Document> {
    // Parse diagnostics can echo source lines; return fixed messages instead.
    let text =
        std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("configuration is not UTF-8"))?;
    let table: toml::Table =
        toml::from_str(text).map_err(|_| anyhow::anyhow!("invalid TOML configuration"))?;
    if table.is_empty() {
        bail!("configuration must contain at least one setting");
    }
    let fields = table.keys().cloned().collect();
    let config: Config = toml::Value::Table(table)
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid TOML configuration"))?;
    config
        .validate()
        .map_err(|_| anyhow::anyhow!("configuration validation failed"))?;
    Ok(Document { config, fields })
}

/// A reload must retain all explicitly configured fields from the last accepted
/// document. Reset a field by assigning its default value explicitly, not deleting it.
pub fn parse_reload(bytes: &[u8], required: &BTreeSet<String>) -> Result<Document> {
    let document = parse(bytes)?;
    if !required.is_subset(&document.fields) {
        bail!("configuration update is missing previously configured settings");
    }
    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_accepts_partial_but_never_empty_or_comment_only_documents() {
        for bytes in [b"".as_slice(), b"# saving settings".as_slice()] {
            assert!(parse(bytes).is_err());
        }
        let initial = parse(b"work_minutes=420").unwrap();
        assert_eq!(initial.config.work_minutes, 420);
        assert_eq!(initial.config.break_minutes, 60);
    }
    #[test]
    fn reload_preserves_explicit_fields_and_accepts_explicit_defaults() {
        let initial = parse(
            b"source='browser'\nbrowser_url='https://other.daouoffice.com/home'\nwork_minutes=420",
        )
        .unwrap();
        assert!(parse_reload(b"source='browser'", &initial.fields).is_err());
        let reset = parse_reload(
            b"source='browser'\nbrowser_url='https://other.daouoffice.com/home'\nwork_minutes=480",
            &initial.fields,
        )
        .unwrap();
        assert_eq!(reset.config.work_minutes, 480);
    }
    #[test]
    fn invalid_documents_never_echo_source_content() {
        let error = parse(b"secret_value='never-echo-this'")
            .err()
            .unwrap()
            .to_string();
        assert_eq!(error, "invalid TOML configuration");
    }
}
