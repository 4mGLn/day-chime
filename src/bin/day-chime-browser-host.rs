use anyhow::{Context, Result};
use day_chime::Config;
fn main() -> Result<()> {
    let config_path = dirs::config_dir()
        .context("cannot resolve config directory")?
        .join("day-chime/config.toml");
    let config: Config = match std::fs::read_to_string(config_path) {
        Ok(contents) => toml::from_str(&contents)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e.into()),
    };
    config.validate()?;
    let directory = dirs::data_local_dir()
        .context("cannot resolve data directory")?
        .join("day-chime");
    day_chime::browser::host(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &config,
        &directory,
    )
}
