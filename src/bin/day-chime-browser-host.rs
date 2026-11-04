use anyhow::{Context, Result};
use day_chime::config_document;
fn main() -> Result<()> {
    let config_path = dirs::config_dir()
        .context("cannot resolve config directory")?
        .join("day-chime/config.toml");
    let contents = std::fs::read(&config_path).context("cannot read browser host configuration")?;
    let config = config_document::parse(&contents)?.config;
    let directory = dirs::data_local_dir()
        .context("cannot resolve data directory")?
        .join("day-chime");
    day_chime::browser::host_reloading(
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        config,
        &contents,
        &config_path,
        &directory,
    )
}
