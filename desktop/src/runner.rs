//! Spawns the repository's Python/Node scripts and relays their output.

use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread;

use crate::settings::{Backend, OutputMode};

/// One line (or the end) of a child process's output.
#[derive(Debug)]
pub enum Msg {
    Stdout(String),
    Stderr(String),
    Exited(Option<i32>),
}

/// Finds the repository root: `$PI_DOCS_ROOT`, then the current directory and
/// the executable's location, walking upward until `scripts/translate_pdf.py`.
pub fn find_root() -> Option<PathBuf> {
    let marker = Path::new("scripts").join("translate_pdf.py");
    if let Some(root) = std::env::var_os("PI_DOCS_ROOT").map(PathBuf::from) {
        return root.join(&marker).is_file().then_some(root);
    }
    let mut starts = Vec::new();
    if let Ok(dir) = std::env::current_dir() {
        starts.push(dir);
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        starts.push(dir);
    }
    starts
        .into_iter()
        .flat_map(|start| start.ancestors().map(Path::to_path_buf).collect::<Vec<_>>())
        .find(|candidate| candidate.join(&marker).is_file())
}

/// A GUI app launched from Finder/Explorer has a minimal PATH, so add the
/// usual install locations for `uv`, `node` and `claude`.
pub fn augmented_path() -> OsString {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    let mut extra: Vec<PathBuf> = Vec::new();
    if cfg!(unix) {
        extra.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    }
    if let Some(dirs) = directories::BaseDirs::new() {
        let home = dirs.home_dir();
        extra.extend([
            home.join(".local").join("bin"),
            home.join(".cargo").join("bin"),
            home.join(".claude").join("local"),
        ]);
    }
    for dir in extra {
        if !paths.contains(&dir) {
            paths.push(dir);
        }
    }
    std::env::join_paths(paths).unwrap_or_default()
}

fn venv_executable(root: &Path, name: &str) -> PathBuf {
    if cfg!(windows) {
        root.join(".venv").join("Scripts").join(format!("{name}.exe"))
    } else {
        root.join(".venv").join("bin").join(name)
    }
}

/// How to launch a program, including its leading arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub program: OsString,
    pub args: Vec<OsString>,
}

/// `translate_pdf.py` launcher: the project venv when present, else `uv run`.
pub fn translate_launch(root: &Path, script_args: &[String]) -> Launch {
    let script = root.join("scripts").join("translate_pdf.py").into_os_string();
    let python = venv_executable(root, "python");
    let mut args: Vec<OsString> = Vec::new();
    let program = if python.is_file() {
        args.push(script);
        args.extend(["--babeldoc".into(), venv_executable(root, "babeldoc").into_os_string()]);
        python.into_os_string()
    } else {
        args.extend(["run".into(), "python".into(), script]);
        "uv".into()
    };
    args.extend(script_args.iter().map(OsString::from));
    Launch { program, args }
}

pub fn bridge_launch(root: &Path, bridge_args: &[String]) -> Launch {
    let mut args: Vec<OsString> = vec![root.join("scripts").join("translation_bridge.ts").into_os_string()];
    args.extend(bridge_args.iter().map(OsString::from));
    Launch { program: "node".into(), args }
}

/// Everything needed to start (or dry-run) one translation.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRequest {
    pub input: PathBuf,
    pub backend: Backend,
    pub model: String,
    pub output_mode: OutputMode,
    pub output_dir: String,
    pub pages: String,
    pub glossary: String,
    pub concurrency: u32,
    pub code_font_pattern: String,
    pub base_url: String,
    pub api_key_env: String,
    pub claude_bin: String,
}

impl JobRequest {
    /// Arguments for `translate_pdf.py`, always in JSON event mode.
    pub fn script_args(&self, dry_run: bool) -> Vec<String> {
        let mut args = vec![
            self.input.display().to_string(),
            "--backend".into(),
            self.backend.cli_name().into(),
            "--model".into(),
            self.model.trim().into(),
            "--output-mode".into(),
            self.output_mode.cli_name().into(),
            "--output-dir".into(),
            self.output_dir.trim().into(),
            "--concurrency".into(),
            self.concurrency.to_string(),
            "--events".into(),
            "json".into(),
        ];
        let mut optional = |flag: &str, value: &str| {
            if !value.trim().is_empty() {
                args.push(flag.into());
                args.push(value.trim().into());
            }
        };
        optional("--pages", &self.pages);
        optional("--glossary", &self.glossary);
        optional("--code-font-pattern", &self.code_font_pattern);
        match self.backend {
            Backend::OpenaiCompatible => {
                optional("--base-url", &self.base_url);
                optional("--api-key-env", &self.api_key_env);
            }
            Backend::ClaudeCode => optional("--claude-bin", &self.claude_bin),
            _ => {}
        }
        if dry_run {
            args.push("--dry-run".into());
        }
        args
    }
}

fn command_for(launch: &Launch, cwd: &Path) -> Command {
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .current_dir(cwd)
        .env("PATH", augmented_path())
        .env("PYTHONUNBUFFERED", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

pub struct Spawned {
    pub pid: u32,
    pub stdin: Option<ChildStdin>,
}

fn pump<R: std::io::Read + Send + 'static>(
    reader: R,
    wrap: fn(String) -> Msg,
    on_msg: std::sync::Arc<dyn Fn(Msg) + Send + Sync>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            on_msg(wrap(line));
        }
    })
}

/// Starts a process and reports its output through `on_msg` from helper
/// threads. `Msg::Exited` is always delivered last.
pub fn spawn_streaming(
    launch: &Launch,
    cwd: &Path,
    pipe_stdin: bool,
    on_msg: impl Fn(Msg) + Send + Sync + 'static,
) -> std::io::Result<Spawned> {
    let on_msg: std::sync::Arc<dyn Fn(Msg) + Send + Sync> = std::sync::Arc::new(on_msg);
    let mut command = command_for(launch, cwd);
    command.stdin(if pipe_stdin { Stdio::piped() } else { Stdio::null() });
    let mut child: Child = command.spawn()?;
    let pid = child.id();
    let stdin = child.stdin.take();
    let readers: Vec<_> = [
        child.stdout.take().map(|out| pump(out, Msg::Stdout, on_msg.clone())),
        child.stderr.take().map(|err| pump(err, Msg::Stderr, on_msg.clone())),
    ]
    .into_iter()
    .flatten()
    .collect();
    thread::spawn(move || {
        let status = child.wait().ok().and_then(|status| status.code());
        for reader in readers {
            let _ = reader.join();
        }
        on_msg(Msg::Exited(status));
    });
    Ok(Spawned { pid, stdin })
}

/// Runs a process to completion; returns stdout lines and the exit code.
pub fn run_blocking(launch: &Launch, cwd: &Path) -> std::io::Result<(Vec<String>, Option<i32>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    spawn_streaming(launch, cwd, false, move |msg| {
        let _ = tx.lock().map(|tx| tx.send(msg));
    })?;
    let mut lines = Vec::new();
    for msg in rx {
        match msg {
            Msg::Stdout(line) => lines.push(line),
            Msg::Stderr(_) => {}
            Msg::Exited(code) => return Ok((lines, code)),
        }
    }
    Ok((lines, None))
}

/// Politely asks the process (and, on Windows, its tree) to stop. The Python
/// script handles SIGTERM so it can stop the bridge and delete its temp config.
pub fn terminate(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).status();
}

/// Forcefully stops the process after the grace period.
pub fn kill(pid: u32) {
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill").args(["/T", "/F", "/PID", &pid.to_string()]).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(backend: Backend) -> JobRequest {
        JobRequest {
            input: PathBuf::from("/tmp/book.pdf"),
            backend,
            model: " sonnet ".into(),
            output_mode: OutputMode::TranslatedOnly,
            output_dir: "/tmp/out".into(),
            pages: "1,2".into(),
            glossary: String::new(),
            concurrency: 4,
            code_font_pattern: String::new(),
            base_url: "https://example.test/v1".into(),
            api_key_env: "MY_KEY".into(),
            claude_bin: String::new(),
        }
    }

    #[test]
    fn script_args_match_the_cli() {
        let args = request(Backend::ClaudeCode).script_args(true);
        assert_eq!(
            args,
            [
                "/tmp/book.pdf", "--backend", "claude-code", "--model", "sonnet", "--output-mode",
                "translated-only", "--output-dir", "/tmp/out", "--concurrency", "4", "--events", "json",
                "--pages", "1,2", "--dry-run"
            ]
        );
    }

    #[test]
    fn only_openai_compatible_gets_endpoint_flags() {
        let direct = request(Backend::OpenaiCompatible).script_args(false);
        assert!(direct.windows(2).any(|w| w == ["--base-url", "https://example.test/v1"]));
        assert!(direct.windows(2).any(|w| w == ["--api-key-env", "MY_KEY"]));
        assert!(!direct.contains(&"--dry-run".to_string()));
        let bridge = request(Backend::Codex).script_args(false);
        assert!(!bridge.contains(&"--base-url".to_string()));
    }

    #[test]
    fn falls_back_to_uv_without_a_venv() {
        let launch = translate_launch(Path::new("/nonexistent-root"), &["x".into()]);
        assert_eq!(launch.program, OsString::from("uv"));
        assert_eq!(&launch.args[..2], ["run", "python"].map(OsString::from));
    }

    #[test]
    fn run_blocking_collects_stdout() {
        let launch = if cfg!(windows) {
            Launch { program: "cmd".into(), args: vec!["/C".into(), "echo hi".into()] }
        } else {
            Launch { program: "sh".into(), args: vec!["-c".into(), "echo hi; echo err >&2".into()] }
        };
        let (lines, code) = run_blocking(&launch, Path::new(".")).unwrap();
        assert_eq!(lines, ["hi"]);
        assert_eq!(code, Some(0));
    }
}
