//! User-local updates reuse the checksum-verifying installer embedded at build time.
use anyhow::{bail, Context, Result};
use clap::{Args, ValueEnum};
use fs2::FileExt;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};

const INSTALLER: &str = include_str!("../install.sh");
const LATEST: &str = "https://api.github.com/repos/4mGLn/day-chime/releases/latest";

#[derive(Args)]
pub struct Options {
    /// Check the latest release without changing files or restarting services.
    #[arg(long, conflicts_with = "force")]
    pub check: bool,
    /// Reinstall the latest release when it matches this binary's version.
    #[arg(long)]
    pub force: bool,
    /// Actual unpacked extension directory (required for legacy manual installs).
    #[arg(long)]
    pub extension_dir: Option<PathBuf>,
    #[arg(long, value_enum)]
    pub browser: Option<Browser>,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Browser {
    Brave,
    Chrome,
    Chromium,
}
impl Browser {
    fn name(self) -> &'static str {
        match self {
            Self::Brave => "brave",
            Self::Chrome => "chrome",
            Self::Chromium => "chromium",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64);
impl Version {
    fn parse(value: &str) -> Result<Self> {
        let value = value.strip_prefix('v').unwrap_or(value);
        let parts: Vec<_> = value.split('.').collect();
        if parts.len() != 3 {
            bail!("expected a stable major.minor.patch version");
        }
        let mut numbers = [0; 3];
        for (index, part) in parts.into_iter().enumerate() {
            if part.is_empty()
                || !part.bytes().all(|c| c.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
            {
                bail!("expected a stable major.minor.patch version");
            }
            numbers[index] = part.parse().context("version component is too large")?;
        }
        Ok(Self(numbers[0], numbers[1], numbers[2]))
    }
    fn tag(self) -> String {
        format!("v{}.{}.{}", self.0, self.1, self.2)
    }
}
#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
}
impl Release {
    fn version(&self) -> Result<Version> {
        if self.draft || self.prerelease || !self.tag_name.starts_with('v') {
            bail!("latest release is not a published stable version");
        }
        let version = Version::parse(&self.tag_name)?;
        if self.tag_name != version.tag() {
            bail!("latest release tag is not canonical");
        }
        Ok(version)
    }
    fn ready(&self, version: Version) -> Result<()> {
        for name in [
            "SHA256SUMS".to_owned(),
            format!("day-chime-{}-linux-x86_64.tar.gz", version.tag()),
        ] {
            if self
                .assets
                .iter()
                .filter(|asset| asset.name == name)
                .count()
                != 1
            {
                bail!("latest release is missing a unique required asset: {name}");
            }
        }
        Ok(())
    }
}
fn latest_release() -> Result<Release> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("day-chime/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client
        .get(LATEST)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .context("cannot check the latest GitHub release")?
        .error_for_status()
        .context("GitHub release lookup failed")?;
    if response.content_length().is_some_and(|size| size > 65_536) {
        bail!("release metadata is unexpectedly large");
    }
    serde_json::from_reader(response.take(65_536)).context("invalid GitHub release metadata")
}

struct Paths {
    executable: PathBuf,
    config_root: PathBuf,
    data_root: PathBuf,
}
impl Paths {
    fn config(&self) -> PathBuf {
        self.config_root.join("day-chime/config.toml")
    }
    fn app(&self) -> PathBuf {
        self.data_root.join("day-chime")
    }
    fn prefix(&self) -> Result<PathBuf> {
        if self
            .executable
            .file_name()
            .is_none_or(|name| name != "day-chime")
            || self
                .executable
                .parent()
                .and_then(Path::file_name)
                .is_none_or(|name| name != "bin")
        {
            bail!("update requires an installed prefix/bin/day-chime executable; use install.sh for source builds");
        }
        Ok(self
            .executable
            .parent()
            .and_then(Path::parent)
            .context("cannot resolve installation prefix")?
            .to_owned())
    }
    fn globals(&self, config: Option<&Path>, state: Option<&Path>) -> Result<()> {
        if let Some(path) = config {
            if absolute(path)? != self.config() {
                bail!("update does not support a custom --config path; use the standard XDG configuration location");
            }
        }
        if let Some(path) = state {
            if absolute(path)? != self.app() {
                bail!("update does not support a custom --state-dir; use the standard XDG data location");
            }
        }
        Ok(())
    }
}
fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let Some(text) = path.to_str() else {
        bail!("update paths must be valid UTF-8");
    };
    if text.chars().any(char::is_control) {
        bail!("update paths must not contain control characters");
    }
    // Normalize lexical aliases without requiring the extension target to exist yet.
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized)
}
#[derive(Deserialize)]
struct Metadata {
    prefix: PathBuf,
    browser: String,
    version: String,
    extension_dir: Option<PathBuf>,
}
fn metadata(app: &Path) -> Result<Option<Metadata>> {
    match std::fs::read(app.join("install-state.json")) {
        Ok(bytes) => {
            if bytes.len() > 65_536 {
                bail!("installation metadata is unexpectedly large");
            }
            Ok(Some(
                serde_json::from_slice(&bytes).context("invalid installation metadata")?,
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("cannot read installation metadata"),
    }
}
struct Plan {
    prefix: PathBuf,
    extension: PathBuf,
    browser: String,
    target: Version,
}
impl Plan {
    fn build(paths: &Paths, options: &Options, target: Version) -> Result<Self> {
        let prefix = paths.prefix()?;
        let previous = metadata(&paths.app())?;
        if let Some(previous) = &previous {
            if std::fs::canonicalize(&previous.prefix).context("stored prefix is unavailable")?
                != prefix
            {
                bail!("stored installation prefix differs from the running executable; update the intended installation explicitly");
            }
            if Version::parse(&previous.version)? > target {
                bail!("a newer version is already installed; refusing to downgrade");
            }
        }
        let browser = options
            .browser
            .map(Browser::name)
            .map(str::to_owned)
            .or_else(|| previous.as_ref().map(|metadata| metadata.browser.clone()))
            .unwrap_or_else(|| "brave".into());
        if !matches!(browser.as_str(), "brave" | "chrome" | "chromium") {
            bail!("stored browser is unsupported; pass --browser explicitly");
        }
        let extension = options.extension_dir.as_ref().map(|path| absolute(path)).transpose()?
            .or_else(|| previous.as_ref().and_then(|metadata| metadata.extension_dir.clone()))
            .or_else(|| previous.as_ref().map(|_| paths.app().join("extension")))
            .context("legacy installation has no remembered extension directory; rerun with --extension-dir /path/to/loaded/extension [--browser brave]")?;
        if !extension.is_absolute() {
            bail!("stored extension directory must be absolute");
        }
        let extension = absolute(&extension)?;
        Ok(Self {
            prefix,
            extension,
            browser,
            target,
        })
    }
    fn installer(&self, paths: &Paths) -> Process {
        Process {
            program: "sh",
            args: vec![
                "-s".into(),
                "--".into(),
                "--prefix".into(),
                self.prefix.as_os_str().to_owned(),
                "--version".into(),
                self.target.tag().into(),
                "--extension-dir".into(),
                self.extension.as_os_str().to_owned(),
                "--browser".into(),
                self.browser.clone().into(),
                "--no-start".into(),
                "--no-downgrade".into(),
                "--preserve-service".into(),
            ],
            input: Some(INSTALLER),
            env: vec![
                (
                    "XDG_CONFIG_HOME".into(),
                    paths.config_root.as_os_str().to_owned(),
                ),
                (
                    "XDG_DATA_HOME".into(),
                    paths.data_root.as_os_str().to_owned(),
                ),
            ],
        }
    }
}

#[derive(Clone)]
struct Process {
    program: &'static str,
    args: Vec<OsString>,
    input: Option<&'static str>,
    env: Vec<(OsString, OsString)>,
}
impl Process {
    fn systemctl(args: &[&str]) -> Self {
        Self {
            program: "systemctl",
            args: std::iter::once("--user")
                .chain(args.iter().copied())
                .map(OsString::from)
                .collect(),
            input: None,
            env: Vec::new(),
        }
    }
}
trait Runner {
    fn run(&mut self, process: &Process) -> Result<Output>;
}
struct System;
impl Runner for System {
    fn run(&mut self, process: &Process) -> Result<Output> {
        let mut command = Command::new(process.program);
        command
            .args(&process.args)
            .envs(process.env.iter().cloned())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if process.input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("cannot run {}", process.program))?;
        if let Some(input) = process.input {
            if let Err(error) = child
                .stdin
                .take()
                .context("installer stdin unavailable")?
                .write_all(input.as_bytes())
            {
                let _ = child.wait();
                return Err(error).context("cannot pass the embedded installer to sh");
            }
        }
        child
            .wait_with_output()
            .context("cannot wait for update subprocess")
    }
}
fn checked(output: Output, action: &str, changed: bool) -> Result<Output> {
    if !output.status.success() {
        if changed {
            bail!("{action} failed after installation; files may be updated but service recovery is incomplete: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        bail!(
            "{action} failed; no update was completed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}
fn decode(value: &str) -> Result<String> {
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    let mut result = Vec::new();
    let mut chars = value.bytes();
    while let Some(c) = chars.next() {
        if c != b'\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some(b'\\') => result.push(b'\\'),
            Some(b'"') => result.push(b'"'),
            Some(b'x') => {
                let digits = [
                    chars.next().context("invalid systemctl escape")?,
                    chars.next().context("invalid systemctl escape")?,
                ];
                let hex = std::str::from_utf8(&digits)?;
                result.push(u8::from_str_radix(hex, 16).context("invalid systemctl escape")?);
            }
            _ => bail!("unsupported escaping in systemctl execution path"),
        }
    }
    String::from_utf8(result).context("systemctl execution path is not UTF-8")
}
struct Service {
    exists: bool,
    active: bool,
}
fn service(paths: &Paths, runner: &mut impl Runner) -> Result<Service> {
    let output = checked(
        runner.run(&Process::systemctl(&[
            "show",
            "day-chime.service",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=ExecStart",
            "--property=FragmentPath",
        ]))?,
        "user service inspection",
        false,
    )?;
    let text = std::str::from_utf8(&output.stdout).context("invalid systemctl output")?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line.split_once('=').context("invalid systemctl property")?;
        if fields.insert(key, value).is_some() {
            bail!("duplicate systemctl property");
        }
    }
    let load = fields
        .get("LoadState")
        .context("service load state unavailable")?;
    if *load == "not-found" {
        return Ok(Service {
            exists: false,
            active: false,
        });
    }
    if *load != "loaded" {
        bail!("user service cannot be safely updated in its current load state");
    }
    let active = fields
        .get("ActiveState")
        .context("service active state unavailable")?;
    if !matches!(*active, "active" | "inactive" | "failed") {
        bail!("user service is transitioning; retry the update after it settles");
    }
    let fragment = fields
        .get("FragmentPath")
        .context("service unit path unavailable")?;
    if absolute(Path::new(&decode(fragment)?))?
        != paths.config_root.join("systemd/user/day-chime.service")
    {
        bail!("existing service is not the expected user unit; update it manually");
    }
    let exec = fields
        .get("ExecStart")
        .context("service executable unavailable")?;
    if exec.matches("{ path=").count() != 1 {
        bail!("user service must have exactly one Day Chime command");
    }
    let (_, tail) = exec
        .split_once("{ path=")
        .context("unsupported service execution format")?;
    let (binary, tail) = tail
        .split_once(" ; argv[]=")
        .context("unsupported service execution format")?;
    if Path::new(&decode(binary)?) != paths.executable {
        bail!("user service points to another executable; refusing to replace it");
    }
    let (arguments, _) = tail
        .split_once(" ; ignore_errors=")
        .context("unsupported service argument format")?;
    let arguments = decode(arguments)?;
    let executable = paths
        .executable
        .to_str()
        .context("executable path is not UTF-8")?;
    let config = paths.config();
    let config = config.to_str().context("configuration path is not UTF-8")?;
    let state = paths.app();
    let state = state.to_str().context("state path is not UTF-8")?;
    let supported = [
        format!("{executable} run"),
        format!("{executable} --config {config} run"),
        format!("{executable} --state-dir {state} run"),
        format!("{executable} --config {config} --state-dir {state} run"),
        format!("{executable} --state-dir {state} --config {config} run"),
    ];
    if !supported.contains(&arguments) {
        bail!("service uses custom arguments or configuration paths; update it manually");
    }
    Ok(Service {
        exists: true,
        active: *active == "active",
    })
}
fn reject_symlink_components(path: &Path) -> Result<()> {
    for component in path.ancestors() {
        match std::fs::symlink_metadata(component) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("update data and lock paths must not contain symlinks")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("cannot validate update data path"),
        }
    }
    Ok(())
}
fn in_own_service(cgroup: &str) -> bool {
    cgroup
        .lines()
        .any(|line| line.split('/').any(|part| part == "day-chime.service"))
}
fn apply(paths: &Paths, plan: &Plan, runner: &mut impl Runner) -> Result<()> {
    let service = service(paths, runner)?;
    let output = checked(
        runner.run(&plan.installer(paths))?,
        "release installation",
        true,
    )?;
    print!("{}", String::from_utf8_lossy(&output.stdout));
    if !output.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
    }
    checked(
        runner.run(&Process::systemctl(&["daemon-reload"]))?,
        "user service reload",
        true,
    )?;
    if service.exists {
        checked(
            runner.run(&Process::systemctl(&["try-restart", "day-chime.service"]))?,
            "user service restart",
            true,
        )?;
        if service.active {
            checked(
                runner.run(&Process::systemctl(&[
                    "is-active",
                    "--quiet",
                    "day-chime.service",
                ]))?,
                "updated service verification",
                true,
            )?;
        }
    }
    println!("Day Chime {} installed. Reload Day Chime once in your browser's extension manager to apply the updated extension.", plan.target.tag());
    Ok(())
}

pub fn run(options: &Options, config: Option<&Path>, state: Option<&Path>) -> Result<()> {
    if std::env::consts::OS != "linux" || std::env::consts::ARCH != "x86_64" {
        bail!("prebuilt updates currently support Linux x86_64 only");
    }
    let paths = Paths {
        executable: std::env::current_exe().context("cannot resolve the running executable")?,
        config_root: absolute(
            &dirs::config_dir().context("cannot resolve configuration directory")?,
        )?,
        data_root: absolute(&dirs::data_local_dir().context("cannot resolve data directory")?)?,
    };
    paths.globals(config, state)?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let release = latest_release()?;
    run_release(&paths, options, current, release, &mut System, || {
        std::fs::read_to_string("/proc/self/cgroup")
            .context("cannot determine the updater's service context")
    })
}

fn run_release(
    paths: &Paths,
    options: &Options,
    current: Version,
    release: Release,
    runner: &mut impl Runner,
    cgroup: impl FnOnce() -> Result<String>,
) -> Result<()> {
    let target = release.version()?;
    println!("Current: {} · Latest: {}", current.tag(), target.tag());
    if options.check {
        return Ok(());
    }
    if target < current {
        println!("This binary is newer than the published release; no downgrade performed.");
        return Ok(());
    }
    if target == current && !options.force {
        println!("Already up to date.");
        return Ok(());
    }
    release.ready(target)?;
    if in_own_service(&cgroup()?) {
        bail!("run update from a terminal outside day-chime.service");
    }
    // Build once before touching files, then repeat metadata checks under the updater lock.
    Plan::build(paths, options, target)?;
    reject_symlink_components(&paths.app())?;
    reject_symlink_components(&paths.app().join("update.lock"))?;
    std::fs::create_dir_all(paths.app())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(paths.app(), std::fs::Permissions::from_mode(0o700))?;
    }
    let mut lock_options = OpenOptions::new();
    lock_options.create(true).truncate(false).write(true);
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Linux x86_64 O_NOFOLLOW: refuse a replaced symlink leaf as well.
        lock_options.custom_flags(0x20000);
    }
    let lock = lock_options.open(paths.app().join("update.lock"))?;
    lock.try_lock_exclusive()
        .context("another Day Chime update is running")?;
    let plan = Plan::build(paths, options, target)?;
    apply(paths, &plan, runner)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{collections::VecDeque, os::unix::process::ExitStatusExt};
    fn output(success: bool, stdout: impl Into<Vec<u8>>) -> Output {
        Output {
            status: std::process::ExitStatus::from_raw(if success { 0 } else { 256 }),
            stdout: stdout.into(),
            stderr: if success {
                Vec::new()
            } else {
                b"injected process failure".to_vec()
            },
        }
    }
    #[derive(Default)]
    struct Mock {
        outputs: VecDeque<Output>,
        calls: Vec<Process>,
    }
    impl Runner for Mock {
        fn run(&mut self, process: &Process) -> Result<Output> {
            self.calls.push(process.clone());
            Ok(self.outputs.pop_front().expect("unexpected subprocess"))
        }
    }
    fn options() -> Options {
        Options {
            check: false,
            force: false,
            extension_dir: None,
            browser: None,
        }
    }
    fn release(version: &str) -> Release {
        Release {
            tag_name: version.into(),
            draft: false,
            prerelease: false,
            assets: vec![
                Asset {
                    name: "SHA256SUMS".into(),
                },
                Asset {
                    name: format!("day-chime-{version}-linux-x86_64.tar.gz"),
                },
            ],
        }
    }
    fn fixture() -> (tempfile::TempDir, Paths) {
        let directory = tempfile::tempdir().unwrap();
        let prefix = directory.path().join("custom prefix");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let paths = Paths {
            executable: prefix.join("bin/day-chime"),
            config_root: directory.path().join("config root"),
            data_root: directory.path().join("data root"),
        };
        std::fs::write(&paths.executable, b"fixture executable").unwrap();
        (directory, paths)
    }
    fn write_metadata(paths: &Paths, version: &str, extension: &Path) {
        std::fs::create_dir_all(paths.app()).unwrap();
        std::fs::write(paths.app().join("install-state.json"), serde_json::to_vec(&serde_json::json!({"prefix": paths.prefix().unwrap(), "version": version, "browser": "chrome", "extension_dir": extension, "extension_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"})).unwrap()).unwrap();
    }
    fn show(paths: &Paths, active: &str, escaped: bool) -> String {
        let escape = |path: &Path| {
            let text = path.to_str().unwrap().to_owned();
            if escaped {
                text.replace(' ', "\\x20")
            } else {
                text
            }
        };
        format!("LoadState=loaded\nActiveState={active}\nFragmentPath={}\nExecStart={{ path={} ; argv[]={exe} --config {} run ; ignore_errors=no ; pid=1 }}\n", escape(&paths.config_root.join("systemd/user/day-chime.service")), escape(&paths.executable), escape(&paths.config()), exe=escape(&paths.executable))
    }
    #[test]
    fn strict_stable_versions_and_release_asset_names() {
        assert!(Version::parse("0.9.9").unwrap() < Version::parse("v0.10.0").unwrap());
        for invalid in [
            "1.2",
            "1.2.3.4",
            "v1.2.3-rc1",
            "v01.2.3",
            "v1.2.3+build",
            "v1.2.3;sh",
            " 1.2.3",
            "1.2.18446744073709551616",
        ] {
            assert!(Version::parse(invalid).is_err(), "{invalid}");
        }
        let mut metadata = release("v1.2.3");
        let version = metadata.version().unwrap();
        metadata.ready(version).unwrap();
        metadata.assets.push(Asset {
            name: "SHA256SUMS".into(),
        });
        assert!(metadata.ready(version).is_err());
        metadata.prerelease = true;
        assert!(metadata.version().is_err());
        metadata.prerelease = false;
        metadata.draft = true;
        assert!(metadata.version().is_err());
    }
    #[test]
    fn check_equal_and_older_release_never_touch_installation_or_run_processes() {
        let (directory, mut paths) = fixture();
        paths.executable = directory.path().join("source-build/day-chime");
        for (check, force, latest) in [
            (true, false, "v1.2.4"),
            (false, false, "v1.2.3"),
            (false, true, "v1.2.2"),
        ] {
            let mut settings = options();
            settings.check = check;
            settings.force = force;
            let mut runner = Mock::default();
            run_release(
                &paths,
                &settings,
                Version(1, 2, 3),
                release(latest),
                &mut runner,
                || panic!("no-op inspected mutable service context"),
            )
            .unwrap();
            assert!(runner.calls.is_empty());
            assert!(!paths.app().exists());
        }
    }
    #[test]
    fn force_same_version_preserves_prefix_browser_extension_and_pins_installer() {
        let (_directory, paths) = fixture();
        let extension = paths.data_root.join("actual loaded extension");
        write_metadata(&paths, "v1.2.3", &extension);
        let mut settings = options();
        settings.force = true;
        let mut runner = Mock {
            outputs: VecDeque::from([
                output(true, show(&paths, "active", false)),
                output(true, b"installed"),
                output(true, b""),
                output(true, b""),
                output(true, b""),
            ]),
            ..Mock::default()
        };
        run_release(
            &paths,
            &settings,
            Version(1, 2, 3),
            release("v1.2.3"),
            &mut runner,
            || Ok("0::/user.slice/app.slice/terminal.scope".into()),
        )
        .unwrap();
        assert_eq!(runner.calls.len(), 5);
        let install = &runner.calls[1];
        assert_eq!(install.program, "sh");
        let args: Vec<_> = install
            .args
            .iter()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(args.windows(2).any(|args| args == ["--version", "v1.2.3"]));
        assert!(args.windows(2).any(|args| args == ["--browser", "chrome"]));
        assert!(args
            .windows(2)
            .any(|args| args[0] == "--extension-dir" && Path::new(args[1]) == extension));
        for flag in ["--no-start", "--no-downgrade", "--preserve-service"] {
            assert!(args.contains(&flag));
        }
        assert!(install.input.unwrap().contains("verify_checksum"));
        assert_eq!(
            install.env,
            vec![
                (
                    "XDG_CONFIG_HOME".into(),
                    paths.config_root.as_os_str().to_owned()
                ),
                (
                    "XDG_DATA_HOME".into(),
                    paths.data_root.as_os_str().to_owned()
                )
            ]
        );
        let commands: Vec<_> = runner
            .calls
            .iter()
            .filter(|call| call.program == "systemctl")
            .map(|call| call.args[1].to_str().unwrap())
            .collect();
        assert_eq!(
            commands,
            ["show", "daemon-reload", "try-restart", "is-active"]
        );
    }
    #[test]
    fn inactive_service_is_not_started_and_systemd_escaped_spaces_are_supported() {
        let (_directory, paths) = fixture();
        let mut settings = options();
        settings.extension_dir = Some(paths.data_root.join("manual extension"));
        let plan = Plan::build(&paths, &settings, Version(1, 2, 4)).unwrap();
        let mut runner = Mock {
            outputs: VecDeque::from([
                output(true, show(&paths, "inactive", true)),
                output(true, b""),
                output(true, b""),
                output(true, b""),
            ]),
            ..Mock::default()
        };
        apply(&paths, &plan, &mut runner).unwrap();
        assert_eq!(runner.calls.len(), 4);
        assert_eq!(runner.calls[3].args[1], "try-restart");
        assert!(!runner.calls.iter().any(|call| call
            .args
            .iter()
            .any(|arg| arg == "enable" || arg == "start")));
    }
    #[test]
    fn missing_service_remains_inactive_and_restart_failure_is_reported_after_install() {
        let (_directory, paths) = fixture();
        let mut settings = options();
        settings.extension_dir = Some(paths.data_root.join("manual extension"));
        let plan = Plan::build(&paths, &settings, Version(1, 2, 4)).unwrap();
        let mut runner = Mock {
            outputs: VecDeque::from([
                output(true, b"LoadState=not-found\nActiveState=inactive\n"),
                output(true, b""),
                output(true, b""),
            ]),
            ..Mock::default()
        };
        apply(&paths, &plan, &mut runner).unwrap();
        assert_eq!(runner.calls.len(), 3);
        let mut runner = Mock {
            outputs: VecDeque::from([
                output(true, show(&paths, "active", false)),
                output(true, b""),
                output(true, b""),
                output(false, b""),
            ]),
            ..Mock::default()
        };
        let error = apply(&paths, &plan, &mut runner).unwrap_err().to_string();
        assert!(error.contains("files may be updated but service recovery is incomplete"));
    }
    #[test]
    fn wrong_service_executable_or_custom_config_refuses_before_installer() {
        let (_directory, paths) = fixture();
        let mut settings = options();
        settings.extension_dir = Some(paths.data_root.join("manual"));
        let plan = Plan::build(&paths, &settings, Version(1, 2, 4)).unwrap();
        for text in [
            show(&paths, "active", false).replace("/bin/day-chime ;", "/bin/day-chime-other ;"),
            show(&paths, "active", false)
                .replace("/day-chime/config.toml run", "/elsewhere/config.toml run"),
        ] {
            let mut runner = Mock {
                outputs: VecDeque::from([output(true, text)]),
                ..Mock::default()
            };
            assert!(apply(&paths, &plan, &mut runner).is_err());
            assert_eq!(runner.calls.len(), 1);
        }
        assert!(paths
            .globals(Some(Path::new("/custom/config.toml")), None)
            .is_err());
        assert!(paths
            .globals(None, Some(Path::new("/custom/state")))
            .is_err());
        paths
            .globals(Some(&paths.config()), Some(&paths.app()))
            .unwrap();
    }
    #[test]
    fn legacy_target_must_be_explicit_and_newer_metadata_blocks_downgrade() {
        let (_directory, paths) = fixture();
        let mut settings = options();
        assert!(Plan::build(&paths, &settings, Version(1, 2, 4))
            .err()
            .unwrap()
            .to_string()
            .contains("--extension-dir"));
        settings.extension_dir = Some(paths.data_root.join("manual"));
        settings.browser = Some(Browser::Brave);
        assert_eq!(
            Plan::build(&paths, &settings, Version(1, 2, 4))
                .unwrap()
                .browser,
            "brave"
        );
        write_metadata(
            &paths,
            "v1.2.5",
            &paths.data_root.join("installed extension"),
        );
        assert!(Plan::build(&paths, &settings, Version(1, 2, 4)).is_err());
    }
    #[test]
    fn concurrent_update_lock_and_own_service_context_block_before_install() {
        let (_directory, paths) = fixture();
        let mut settings = options();
        settings.extension_dir = Some(paths.data_root.join("manual"));
        let mut runner = Mock::default();
        assert!(run_release(
            &paths,
            &settings,
            Version(1, 2, 3),
            release("v1.2.4"),
            &mut runner,
            || Ok("0::/user.slice/day-chime.service".into())
        )
        .is_err());
        assert!(!paths.app().exists());
        std::fs::create_dir_all(paths.app()).unwrap();
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.app().join("update.lock"))
            .unwrap();
        lock.try_lock_exclusive().unwrap();
        let error = run_release(
            &paths,
            &settings,
            Version(1, 2, 3),
            release("v1.2.4"),
            &mut runner,
            || Ok("0::/terminal.scope".into()),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("another Day Chime update"));
        assert!(runner.calls.is_empty());
        assert!(!in_own_service(
            "0::/user.slice/day-chime.service-helper.scope"
        ));
    }
    #[test]
    fn symlink_data_directory_or_lock_is_rejected_without_permission_changes() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let (directory, paths) = fixture();
        let outside = directory.path().join("unrelated");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(&paths.data_root).unwrap();
        symlink(&outside, paths.app()).unwrap();
        let mut settings = options();
        settings.extension_dir = Some(paths.data_root.join("manual"));
        let mut runner = Mock::default();
        assert!(run_release(
            &paths,
            &settings,
            Version(1, 2, 3),
            release("v1.2.4"),
            &mut runner,
            || Ok("0::/terminal.scope".into())
        )
        .is_err());
        assert_eq!(
            std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!outside.join("update.lock").exists());
        std::fs::remove_file(paths.app()).unwrap();
        std::fs::create_dir_all(paths.app()).unwrap();
        let outside_file = outside.join("unrelated-file");
        std::fs::write(&outside_file, b"do not touch").unwrap();
        symlink(&outside_file, paths.app().join("update.lock")).unwrap();
        assert!(run_release(
            &paths,
            &settings,
            Version(1, 2, 3),
            release("v1.2.4"),
            &mut runner,
            || Ok("0::/terminal.scope".into())
        )
        .is_err());
        assert_eq!(std::fs::read(&outside_file).unwrap(), b"do not touch");
        assert!(runner.calls.is_empty());
    }
}
