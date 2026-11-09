//! Main window: state, background message pump and rendering.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Div, ElementId, Entity, ExternalPaths, FocusHandle, Focusable,
    PathPromptOptions, Rgba, SharedString, Stateful, Window, actions, div, prelude::*, px, rgb,
    rgba,
};

use crate::protocol::{JobEvent, LoginEvent, StatusEvent, parse_line};
use crate::runner::{self, JobRequest, Msg};
use crate::settings::{Backend, OutputMode, Settings};
use crate::text_input::TextInput;

actions!(app, [ChooseFile, StartTranslation, CancelCurrent, Quit]);

const BG: u32 = 0xf4f5f7;
const CARD: u32 = 0xffffff;
const BORDER: u32 = 0xd9dce1;
const TEXT: u32 = 0x1f2328;
const MUTED: u32 = 0x6b7280;
const PRIMARY: u32 = 0x2563eb;
const OK: u32 = 0x15803d;
const ERR: u32 = 0xb91c1c;
const WARN: u32 = 0xb45309;
const LOG_LIMIT: usize = 400;
const GRACE: Duration = Duration::from_secs(10);

pub fn register(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("secondary-o", ChooseFile, Some("App")),
        gpui::KeyBinding::new("secondary-enter", StartTranslation, Some("App")),
        gpui::KeyBinding::new("secondary-.", CancelCurrent, Some("App")),
        gpui::KeyBinding::new("secondary-q", Quit, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

enum AppMsg {
    Env(EnvReport),
    Status(Backend, BackendStatus),
    Validation { seq: u64, outcome: Result<ValidationOk, (String, String)> },
    Job { id: u64, msg: Msg },
    Login { id: u64, msg: Msg },
}

struct EnvReport {
    root: Option<PathBuf>,
    node: Result<String, String>,
    runner: Result<String, String>,
}

#[derive(Clone, Debug)]
enum BackendStatus {
    Unknown,
    Ready { signed_in: Option<bool>, version: Option<String> },
    Unavailable(String),
}

struct ValidationOk {
    output_file: Option<String>,
    warnings: Vec<String>,
}

enum Validation {
    NoInput,
    Checking,
    Ready(ValidationOk),
    Failed { message: String },
}

#[derive(Clone, Copy, PartialEq)]
enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

struct Job {
    id: u64,
    request: JobRequest,
    status: JobStatus,
    stage: String,
    progress: Option<f32>,
    started: Option<Instant>,
    finished: Option<Instant>,
    log: Vec<String>,
    warnings: Vec<String>,
    pid: Option<u32>,
    cancel_requested: Option<Instant>,
    expected_output: Option<String>,
    output: Option<PathBuf>,
    error: Option<(String, String)>,
    show_log: bool,
}

struct LoginSession {
    id: u64,
    backend: Backend,
    pid: u32,
    stdin: Option<ChildStdin>,
    notices: Vec<String>,
    prompt: Option<LoginEvent>,
    auth_url: Option<String>,
    device: Option<(String, String)>,
    done: bool,
    error: Option<String>,
    input: Entity<TextInput>,
}

pub struct AppView {
    focus_handle: FocusHandle,
    settings: Settings,
    tx: Sender<AppMsg>,
    rx: Receiver<AppMsg>,
    root: Option<PathBuf>,
    env: Option<EnvReport>,
    statuses: HashMap<Backend, BackendStatus>,
    input_pdf: Option<PathBuf>,
    model: Entity<TextInput>,
    pages: Entity<TextInput>,
    output_dir: Entity<TextInput>,
    glossary: Entity<TextInput>,
    code_font: Entity<TextInput>,
    base_url: Entity<TextInput>,
    api_key_env: Entity<TextInput>,
    claude_bin: Entity<TextInput>,
    show_advanced: bool,
    validation: Validation,
    validation_seq: u64,
    validate_at: Option<Instant>,
    last_validated: Option<JobRequest>,
    jobs: Vec<Job>,
    next_id: u64,
    login: Option<LoginSession>,
    notice: Option<String>,
}

impl Focusable for AppView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn new_input(cx: &mut Context<AppView>, placeholder: &str, text: &str) -> Entity<TextInput> {
    let input = cx.new(|cx| TextInput::new(cx, placeholder, text, false));
    cx.observe(&input, |this, _, cx| this.mark_dirty(cx)).detach();
    input
}

impl AppView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = Settings::load();
        let (tx, rx) = channel();
        let root = runner::find_root();
        let this = Self {
            focus_handle: cx.focus_handle(),
            tx,
            rx,
            root: root.clone(),
            env: None,
            statuses: HashMap::new(),
            input_pdf: None,
            model: new_input(cx, "Model ID", &settings.model),
            pages: new_input(cx, "All pages, or e.g. 1,5,31", ""),
            output_dir: new_input(cx, "Output folder", &settings.output_dir),
            glossary: new_input(cx, "Default DDD glossary", &settings.glossary),
            code_font: new_input(cx, "e.g. Courier.*", &settings.code_font_pattern),
            base_url: new_input(cx, "https://api.openai.com/v1", &settings.base_url),
            api_key_env: new_input(cx, "OPENAI_API_KEY", &settings.api_key_env),
            claude_bin: new_input(cx, "claude", &settings.claude_bin),
            settings,
            show_advanced: false,
            validation: Validation::NoInput,
            validation_seq: 0,
            validate_at: None,
            last_validated: None,
            jobs: Vec::new(),
            next_id: 1,
            login: None,
            notice: None,
        };
        this.check_environment();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        this
    }

    /// Stops child processes; called when the window closes.
    pub fn shutdown(&mut self) {
        self.persist_settings_now();
        for job in &self.jobs {
            if job.status == JobStatus::Running {
                if let Some(pid) = job.pid {
                    runner::terminate(pid);
                }
            }
        }
        if let Some(login) = &self.login {
            runner::terminate(login.pid);
        }
    }

    // -- background work ---------------------------------------------------

    fn check_environment(&self) {
        let tx = self.tx.clone();
        let root = self.root.clone();
        std::thread::spawn(move || {
            let report = match root {
                None => EnvReport {
                    root: None,
                    node: Err("repository not found".into()),
                    runner: Err("repository not found".into()),
                },
                Some(root) => {
                    let version = |program: &str| -> Result<String, String> {
                        let launch = runner::Launch { program: program.into(), args: vec!["--version".into()] };
                        match runner::run_blocking(&launch, &root) {
                            Ok((lines, Some(0))) => Ok(lines.first().cloned().unwrap_or_default()),
                            Ok((_, code)) => Err(format!("{program} --version exited with {code:?}")),
                            Err(error) => Err(format!("{program} not found ({error})")),
                        }
                    };
                    let venv = runner::translate_launch(&root, &[]).program != std::ffi::OsString::from("uv");
                    EnvReport {
                        node: version("node"),
                        runner: if venv { Ok("project .venv".into()) } else { version("uv") },
                        root: Some(root),
                    }
                }
            };
            let _ = tx.send(AppMsg::Env(report));
        });
    }

    fn refresh_status(&self, backend: Backend) {
        let (Some(root), true) = (self.root.clone(), backend.uses_bridge()) else {
            return;
        };
        let tx = self.tx.clone();
        let claude_bin = self.settings.claude_bin.clone();
        std::thread::spawn(move || {
            let mut args = vec!["status".to_string(), "--provider".into(), backend.cli_name().into()];
            if backend == Backend::ClaudeCode && !claude_bin.trim().is_empty() {
                args.extend(["--claude-bin".into(), claude_bin.trim().into()]);
            }
            let status = match runner::run_blocking(&runner::bridge_launch(&root, &args), &root) {
                Ok((lines, _)) => lines
                    .iter()
                    .find_map(|line| parse_line::<StatusEvent>(line))
                    .map(|s| {
                        if s.installed {
                            BackendStatus::Ready { signed_in: s.signed_in, version: s.version }
                        } else {
                            BackendStatus::Unavailable(s.message.unwrap_or_else(|| "not installed".into()))
                        }
                    })
                    .unwrap_or_else(|| BackendStatus::Unavailable("no status reported".into())),
                Err(error) => BackendStatus::Unavailable(error.to_string()),
            };
            let _ = tx.send(AppMsg::Status(backend, status));
        });
    }

    fn current_request(&self, cx: &App) -> Option<JobRequest> {
        let input = self.input_pdf.clone()?;
        let settings = &self.settings;
        Some(JobRequest {
            input,
            backend: settings.backend,
            model: self.model.read(cx).text().trim().to_string(),
            output_mode: settings.output_mode,
            output_dir: self.output_dir.read(cx).text().trim().to_string(),
            pages: self.pages.read(cx).text().trim().to_string(),
            glossary: self.glossary.read(cx).text().trim().to_string(),
            concurrency: settings.concurrency,
            code_font_pattern: self.code_font.read(cx).text().trim().to_string(),
            base_url: self.base_url.read(cx).text().trim().to_string(),
            api_key_env: self.api_key_env.read(cx).text().trim().to_string(),
            claude_bin: self.claude_bin.read(cx).text().trim().to_string(),
        })
    }

    fn mark_dirty(&mut self, cx: &mut Context<Self>) {
        self.validate_at = Some(Instant::now() + Duration::from_millis(400));
        cx.notify();
    }

    fn persist_settings_now(&mut self) {
        // Entities are read in `sync_settings`; this saves whatever was last synced.
        self.settings.save();
    }

    fn sync_settings(&mut self, cx: &App) {
        self.settings.model = self.model.read(cx).text().trim().to_string();
        self.settings.output_dir = self.output_dir.read(cx).text().trim().to_string();
        self.settings.glossary = self.glossary.read(cx).text().trim().to_string();
        self.settings.code_font_pattern = self.code_font.read(cx).text().trim().to_string();
        self.settings.base_url = self.base_url.read(cx).text().trim().to_string();
        self.settings.api_key_env = self.api_key_env.read(cx).text().trim().to_string();
        self.settings.claude_bin = self.claude_bin.read(cx).text().trim().to_string();
    }

    fn start_validation(&mut self, cx: &mut Context<Self>) {
        self.sync_settings(cx);
        self.settings.save();
        let (Some(root), Some(request)) = (self.root.clone(), self.current_request(cx)) else {
            self.validation = Validation::NoInput;
            return;
        };
        if self.last_validated.as_ref() == Some(&request) {
            return;
        }
        self.last_validated = Some(request.clone());
        self.validation_seq += 1;
        self.validation = Validation::Checking;
        let seq = self.validation_seq;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let launch = runner::translate_launch(&root, &request.script_args(true));
            let outcome = match runner::run_blocking(&launch, &root) {
                Err(error) => Err(("spawn_failed".to_string(), error.to_string())),
                Ok((lines, _)) => {
                    let mut result = Err(("no_result".to_string(), "validation produced no result".to_string()));
                    for event in lines.iter().filter_map(|line| parse_line::<JobEvent>(line)) {
                        match event {
                            JobEvent::Config { config } => {
                                let mut warnings = Vec::new();
                                if config["api_key_configured"] == serde_json::Value::Bool(false) {
                                    warnings.push(format!(
                                        "Environment variable {} is not set for this app.",
                                        config["api_key_env"].as_str().unwrap_or("OPENAI_API_KEY")
                                    ));
                                }
                                result = Ok(ValidationOk {
                                    output_file: config["output_file"].as_str().map(str::to_string),
                                    warnings,
                                });
                            }
                            JobEvent::Error { code, message } => result = Err((code, message)),
                            _ => {}
                        }
                    }
                    result
                }
            };
            let _ = tx.send(AppMsg::Validation { seq, outcome });
        });
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(message) = self.rx.try_recv() {
            changed = true;
            match message {
                AppMsg::Env(report) => self.env = Some(report),
                AppMsg::Status(backend, status) => {
                    self.statuses.insert(backend, status);
                }
                AppMsg::Validation { seq, outcome } => {
                    if seq == self.validation_seq {
                        self.validation = match outcome {
                            Ok(ok) => Validation::Ready(ok),
                            Err((_, message)) => Validation::Failed { message },
                        };
                    }
                }
                AppMsg::Job { id, msg } => self.on_job_msg(id, msg, cx),
                AppMsg::Login { id, msg } => self.on_login_msg(id, msg, cx),
            }
            if self.env.is_some() && self.statuses.is_empty() && self.root.is_some() {
                for backend in Backend::ALL {
                    self.statuses.insert(backend, BackendStatus::Unknown);
                    self.refresh_status(backend);
                }
            }
        }
        if self.validate_at.is_some_and(|at| Instant::now() >= at) {
            self.validate_at = None;
            self.start_validation(cx);
            changed = true;
        }
        for job in &self.jobs {
            if let (JobStatus::Running, Some(requested), Some(pid)) = (job.status, job.cancel_requested, job.pid) {
                if requested.elapsed() > GRACE {
                    runner::kill(pid);
                }
            }
        }
        if changed || self.jobs.iter().any(|job| job.status == JobStatus::Running) {
            cx.notify();
        }
    }

    // -- jobs ----------------------------------------------------------------

    fn enqueue(&mut self, cx: &mut Context<Self>) {
        self.sync_settings(cx);
        self.settings.save();
        let Some(request) = self.current_request(cx) else {
            self.notice = Some("Choose a PDF first.".into());
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            request,
            status: JobStatus::Queued,
            stage: "Queued".into(),
            progress: None,
            started: None,
            finished: None,
            log: Vec::new(),
            warnings: Vec::new(),
            pid: None,
            cancel_requested: None,
            expected_output: None,
            output: None,
            error: None,
            show_log: false,
        });
        self.notice = None;
        self.start_next(cx);
    }

    fn start_next(&mut self, cx: &mut Context<Self>) {
        if self.jobs.iter().any(|job| job.status == JobStatus::Running) {
            return;
        }
        let Some(root) = self.root.clone() else { return };
        let Some(job) = self.jobs.iter_mut().find(|job| job.status == JobStatus::Queued) else {
            return;
        };
        let id = job.id;
        let launch = runner::translate_launch(&root, &job.request.script_args(false));
        let tx = self.tx.clone();
        match runner::spawn_streaming(&launch, &root, false, move |msg| {
            let _ = tx.send(AppMsg::Job { id, msg });
        }) {
            Ok(spawned) => {
                job.status = JobStatus::Running;
                job.pid = Some(spawned.pid);
                job.started = Some(Instant::now());
                job.stage = "Starting".into();
            }
            Err(error) => {
                job.status = JobStatus::Failed;
                job.error = Some(("spawn_failed".into(), format!("could not start translate_pdf.py: {error}")));
                job.finished = Some(Instant::now());
                self.start_next(cx);
            }
        }
        cx.notify();
    }

    fn on_job_msg(&mut self, id: u64, msg: Msg, cx: &mut Context<Self>) {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        match msg {
            Msg::Stderr(line) => {
                job.log.push(line);
                if job.log.len() > LOG_LIMIT {
                    job.log.remove(0);
                }
            }
            Msg::Stdout(line) => match parse_line::<JobEvent>(&line) {
                Some(JobEvent::Config { config }) => {
                    job.expected_output = config["output_file"].as_str().map(str::to_string);
                }
                Some(JobEvent::Stage { stage }) => job.stage = stage_label(&stage).into(),
                Some(JobEvent::Progress { stage, overall, .. }) => {
                    if let Some(stage) = stage {
                        job.stage = stage;
                    }
                    if let Some(overall) = overall {
                        job.progress = Some(overall as f32);
                    }
                }
                Some(JobEvent::Warning { message }) => job.warnings.push(message),
                Some(JobEvent::Result { output_file, .. }) => {
                    job.output = Some(PathBuf::from(output_file));
                    job.status = JobStatus::Done;
                    job.progress = Some(1.0);
                    job.stage = "Finished".into();
                }
                Some(JobEvent::Error { code, message }) => {
                    job.error = Some((code, message));
                }
                Some(JobEvent::Unknown) | None => {
                    job.log.push(line);
                }
            },
            Msg::Exited(code) => {
                job.finished = Some(Instant::now());
                job.pid = None;
                if job.status == JobStatus::Running {
                    let cancelled = job.cancel_requested.is_some()
                        || job.error.as_ref().is_some_and(|(code, _)| code == "cancelled");
                    if cancelled {
                        job.status = JobStatus::Cancelled;
                        job.error = None;
                        job.stage = "Cancelled".into();
                    } else {
                        job.status = JobStatus::Failed;
                        if job.error.is_none() {
                            job.error = Some(("exited".into(), format!("translate_pdf.py exited with status {code:?}")));
                        }
                    }
                }
                self.start_next(cx);
            }
        }
        cx.notify();
    }

    fn cancel_job(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        match job.status {
            JobStatus::Queued => {
                job.status = JobStatus::Cancelled;
                job.stage = "Cancelled".into();
            }
            JobStatus::Running if job.cancel_requested.is_none() => {
                job.cancel_requested = Some(Instant::now());
                job.stage = "Cancelling…".into();
                if let Some(pid) = job.pid {
                    runner::terminate(pid);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    fn cancel_current(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.jobs.iter().find(|job| job.status == JobStatus::Running).map(|job| job.id) {
            self.cancel_job(id, cx);
        }
    }

    // -- login ---------------------------------------------------------------

    fn start_login(&mut self, backend: Backend, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else { return };
        if self.login.is_some() || !backend.has_login() {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        let args: Vec<String> = ["login", "--provider", backend.cli_name(), "--events", "json"]
            .into_iter()
            .map(String::from)
            .collect();
        let tx = self.tx.clone();
        let result = runner::spawn_streaming(&runner::bridge_launch(&root, &args), &root, true, move |msg| {
            let _ = tx.send(AppMsg::Login { id, msg });
        });
        match result {
            Ok(spawned) => {
                let input = cx.new(|cx| TextInput::new(cx, "", "", false));
                self.login = Some(LoginSession {
                    id,
                    backend,
                    pid: spawned.pid,
                    stdin: spawned.stdin,
                    notices: Vec::new(),
                    prompt: None,
                    auth_url: None,
                    device: None,
                    done: false,
                    error: None,
                    input,
                });
            }
            Err(error) => self.notice = Some(format!("Could not start sign-in: {error}")),
        }
        cx.notify();
    }

    fn on_login_msg(&mut self, id: u64, msg: Msg, cx: &mut Context<Self>) {
        let Some(login) = self.login.as_mut().filter(|login| login.id == id) else {
            return;
        };
        match msg {
            Msg::Stderr(_) => {}
            Msg::Stdout(line) => match parse_line::<LoginEvent>(&line) {
                Some(event @ (LoginEvent::Select { .. } | LoginEvent::Text { .. })) => {
                    login.input.update(cx, |input, cx| {
                        input.set_masked(false, cx);
                        input.clear(cx);
                    });
                    login.prompt = Some(event);
                }
                Some(event @ LoginEvent::Secret { .. }) => {
                    login.input.update(cx, |input, cx| {
                        input.set_masked(true, cx);
                        input.clear(cx);
                    });
                    login.prompt = Some(event);
                }
                Some(LoginEvent::AuthUrl { url, instructions }) => {
                    if let Some(text) = instructions {
                        login.notices.push(text);
                    }
                    cx.open_url(&url);
                    login.auth_url = Some(url);
                }
                Some(LoginEvent::DeviceCode { verification_uri, user_code }) => {
                    cx.open_url(&verification_uri);
                    login.device = Some((verification_uri, user_code));
                }
                Some(LoginEvent::Info { message }) => login.notices.push(message),
                Some(LoginEvent::Done { .. }) => {
                    login.done = true;
                    login.prompt = None;
                }
                Some(LoginEvent::Error { message, .. }) => login.error = Some(message),
                Some(LoginEvent::Unknown) | None => {}
            },
            Msg::Exited(_) => {
                login.stdin = None;
                let backend = login.backend;
                if !login.done && login.error.is_none() {
                    login.error = Some("Sign-in ended before it finished.".into());
                }
                if login.done {
                    self.refresh_status(backend);
                }
            }
        }
        cx.notify();
    }

    fn answer_login(&mut self, value: String, cx: &mut Context<Self>) {
        let Some(login) = self.login.as_mut() else { return };
        let id = match &login.prompt {
            Some(LoginEvent::Select { id, .. } | LoginEvent::Secret { id, .. } | LoginEvent::Text { id, .. }) => id.clone(),
            _ => return,
        };
        if let Some(stdin) = login.stdin.as_mut() {
            let line = serde_json::json!({ "id": id, "value": value }).to_string();
            if writeln!(stdin, "{line}").and_then(|_| stdin.flush()).is_err() {
                login.error = Some("Could not talk to the sign-in process.".into());
            }
        }
        login.prompt = None;
        login.input.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    fn close_login(&mut self, cx: &mut Context<Self>) {
        if let Some(login) = self.login.take() {
            if !login.done && login.error.is_none() {
                runner::terminate(login.pid);
            }
            self.refresh_status(login.backend);
        }
        cx.notify();
    }

    // -- user actions ----------------------------------------------------------

    fn set_input(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.input_pdf = Some(path);
        self.last_validated = None;
        self.mark_dirty(cx);
    }

    fn choose_file(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a PDF".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update(cx, |this, cx| this.set_input(path, cx)).ok();
                }
            }
        })
        .detach();
    }

    fn choose_directory(&mut self, target: Entity<TextInput>, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose a folder".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update(cx, |_, cx| {
                        target.update(cx, |input, cx| input.set_text(&path.display().to_string(), cx));
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn choose_glossary(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a glossary CSV".into()),
        });
        let target = self.glossary.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update(cx, |_, cx| {
                        target.update(cx, |input, cx| input.set_text(&path.display().to_string(), cx));
                    })
                    .ok();
                }
            }
        })
        .detach();
    }

    fn select_backend(&mut self, backend: Backend, cx: &mut Context<Self>) {
        if self.settings.backend == backend {
            return;
        }
        let current = self.model.read(cx).text().trim().to_string();
        let was_default = current.is_empty() || current == self.settings.backend.default_model();
        self.settings.backend = backend;
        if was_default {
            self.model.update(cx, |input, cx| input.set_text(backend.default_model(), cx));
        }
        self.mark_dirty(cx);
    }

    fn can_start(&self) -> bool {
        self.root.is_some()
            && self.input_pdf.is_some()
            && !matches!(self.validation, Validation::Failed { .. } | Validation::NoInput)
    }
}

fn stage_label(stage: &str) -> &str {
    match stage {
        "bridge_starting" => "Starting provider bridge",
        "bridge_ready" => "Provider bridge ready",
        "translating" => "Translating",
        "validating" => "Checking output",
        other => other,
    }
}

fn format_elapsed(duration: Duration) -> String {
    let seconds = duration.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Kind {
    Primary,
    Secondary,
    Danger,
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, kind: Kind, enabled: bool) -> Stateful<Div> {
    let (bg, fg, border): (Rgba, Rgba, Rgba) = match kind {
        Kind::Primary => (rgb(PRIMARY), rgb(0xffffff), rgb(PRIMARY)),
        Kind::Secondary => (rgb(CARD), rgb(TEXT), rgb(BORDER)),
        Kind::Danger => (rgb(CARD), rgb(ERR), rgb(ERR)),
    };
    let element = div()
        .id(id)
        .px_3()
        .py_1()
        .rounded_md()
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_color(fg)
        .text_sm()
        .flex_none()
        .child(label.into());
    if enabled {
        element.cursor_pointer().hover(|style| style.opacity(0.85))
    } else {
        element.opacity(0.45)
    }
}

fn label(text: impl Into<SharedString>) -> Div {
    div().text_xs().text_color(rgb(MUTED)).child(text.into())
}

fn field(title: &str, content: impl IntoElement) -> Div {
    div().flex().flex_col().gap_1().child(label(title.to_string())).child(content)
}

fn card() -> Div {
    div().bg(rgb(CARD)).border_1().border_color(rgb(BORDER)).rounded_lg().p_3()
}

impl AppView {
    fn segmented<T: Copy + PartialEq + 'static>(
        &self,
        prefix: &'static str,
        options: &[(T, &'static str)],
        selected: T,
        cx: &mut Context<Self>,
        on_select: impl Fn(&mut Self, T, &mut Context<Self>) + Clone + 'static,
    ) -> Div {
        let mut row = div().flex().flex_wrap().gap_1();
        for (index, (value, text)) in options.iter().enumerate() {
            let value = *value;
            let on_select = on_select.clone();
            let active = value == selected;
            let mut item = button((prefix, index), *text, if active { Kind::Primary } else { Kind::Secondary }, true);
            item = item.on_click(cx.listener(move |this, _, _, cx| on_select(this, value, cx)));
            row = row.child(item);
        }
        row
    }

    fn render_input_card(&mut self, cx: &mut Context<Self>) -> Div {
        let name = self.input_pdf.as_ref().map(|path| file_name(path));
        let hint = match &name {
            Some(_) => "Drop another PDF or click to replace",
            None => "Drop a PDF here, or click to choose  (Ctrl/Cmd+O)",
        };
        card().child(
            div()
                .id("drop-zone")
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_1()
                .h(px(86.))
                .rounded_md()
                .border_1()
                .border_color(rgb(BORDER))
                .cursor_pointer()
                .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgba(0x2563eb22)).border_color(rgb(PRIMARY)))
                .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                    let pdf = paths
                        .paths()
                        .iter()
                        .find(|path| path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")));
                    match pdf {
                        Some(path) => this.set_input(path.clone(), cx),
                        None => {
                            this.notice = Some("Only PDF files are supported.".into());
                            cx.notify();
                        }
                    }
                }))
                .on_click(cx.listener(|this, _, _, cx| this.choose_file(cx)))
                .child(
                    div()
                        .text_color(rgb(TEXT))
                        .child(name.unwrap_or_else(|| "No PDF selected".into())),
                )
                .child(label(hint)),
        )
    }

    fn render_backend_card(&mut self, cx: &mut Context<Self>) -> Div {
        let backend = self.settings.backend;
        let options: Vec<(Backend, &'static str)> = Backend::ALL.iter().map(|b| (*b, b.label())).collect();
        let selector = self.segmented("backend", &options, backend, cx, |this, value, cx| this.select_backend(value, cx));

        let (status_text, status_color, show_sign_in) = match (backend, self.statuses.get(&backend)) {
            (Backend::OpenaiCompatible, _) => (
                "Uses an API key from the environment variable below.".to_string(),
                MUTED,
                false,
            ),
            (_, None | Some(BackendStatus::Unknown)) => ("Checking…".to_string(), MUTED, false),
            (_, Some(BackendStatus::Unavailable(message))) => (message.clone(), ERR, false),
            (Backend::ClaudeCode, Some(BackendStatus::Ready { version, .. })) => (
                format!(
                    "Claude Code CLI found{}. Sign in once by running `claude` and typing /login.",
                    version.as_ref().map(|v| format!(" ({v})")).unwrap_or_default()
                ),
                OK,
                false,
            ),
            (_, Some(BackendStatus::Ready { signed_in: Some(true), .. })) => ("Signed in".to_string(), OK, true),
            (_, Some(BackendStatus::Ready { .. })) => ("Not signed in".to_string(), WARN, true),
        };

        let mut status_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().flex_1().text_sm().text_color(rgb(status_color)).child(status_text));
        if backend.has_login() && show_sign_in {
            status_row = status_row.child(
                button("sign-in", "Sign in…", Kind::Secondary, self.login.is_none())
                    .on_click(cx.listener(move |this, _, _, cx| this.start_login(backend, cx))),
            );
        }
        if backend.uses_bridge() {
            status_row = status_row.child(
                button("refresh-status", "Recheck", Kind::Secondary, true)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.refresh_status(backend);
                        cx.notify();
                    })),
            );
        }

        card()
            .flex()
            .flex_col()
            .gap_2()
            .child(field("Backend", selector))
            .child(status_row)
            .child(field("Model", self.model.clone()))
    }

    fn render_output_card(&mut self, cx: &mut Context<Self>) -> Div {
        let mode = self.settings.output_mode;
        let modes = [(OutputMode::Bilingual, OutputMode::Bilingual.label()), (OutputMode::TranslatedOnly, OutputMode::TranslatedOnly.label())];
        let selector = self.segmented("mode", &modes, mode, cx, |this, value, cx| {
            this.settings.output_mode = value;
            this.mark_dirty(cx);
        });
        let target = self.output_dir.clone();
        let folder_row = div().flex().gap_2().items_center().child(div().flex_1().child(self.output_dir.clone())).child(
            button("browse-output", "Browse…", Kind::Secondary, true)
                .on_click(cx.listener(move |this, _, _, cx| this.choose_directory(target.clone(), cx))),
        );
        card()
            .flex()
            .flex_col()
            .gap_2()
            .child(field("Output", selector))
            .child(field("Output folder", folder_row))
    }

    fn render_advanced_card(&mut self, cx: &mut Context<Self>) -> Div {
        let toggle = button("advanced", if self.show_advanced { "Hide advanced options" } else { "Advanced options" }, Kind::Secondary, true)
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_advanced = !this.show_advanced;
                cx.notify();
            }));
        let mut card = card().flex().flex_col().gap_2().child(div().flex().child(toggle));
        if !self.show_advanced {
            return card;
        }
        let concurrency = self.settings.concurrency;
        let stepper = div()
            .flex()
            .items_center()
            .gap_2()
            .child(button("conc-minus", "−", Kind::Secondary, concurrency > 1).on_click(cx.listener(|this, _, _, cx| {
                this.settings.concurrency = this.settings.concurrency.saturating_sub(1).max(1);
                this.mark_dirty(cx);
            })))
            .child(div().w(px(28.)).flex().justify_center().child(concurrency.to_string()))
            .child(button("conc-plus", "+", Kind::Secondary, concurrency < 16).on_click(cx.listener(|this, _, _, cx| {
                this.settings.concurrency = (this.settings.concurrency + 1).min(16);
                this.mark_dirty(cx);
            })))
            .child(label("parallel requests (1–16)"));
        let glossary_row = div().flex().gap_2().items_center().child(div().flex_1().child(self.glossary.clone())).child(
            button("browse-glossary", "Browse…", Kind::Secondary, true).on_click(cx.listener(|this, _, _, cx| this.choose_glossary(cx))),
        );
        card = card
            .child(field("Pages", self.pages.clone()))
            .child(field("Concurrency", stepper))
            .child(field("Glossary CSV", glossary_row))
            .child(field("Extra code-font regex", self.code_font.clone()));
        match self.settings.backend {
            Backend::OpenaiCompatible => {
                card = card
                    .child(field("Base URL", self.base_url.clone()))
                    .child(field("API key environment variable (name only)", self.api_key_env.clone()));
            }
            Backend::ClaudeCode => {
                card = card.child(field("Claude Code executable", self.claude_bin.clone()));
            }
            _ => {}
        }
        card
    }

    fn render_start_card(&mut self, cx: &mut Context<Self>) -> Div {
        let (text, color): (String, u32) = match &self.validation {
            _ if self.root.is_none() => ("Repository not found — set PI_DOCS_ROOT.".into(), ERR),
            Validation::NoInput => ("Choose a PDF to begin.".into(), MUTED),
            Validation::Checking => ("Checking settings…".into(), MUTED),
            Validation::Ready(ok) => (
                match &ok.output_file {
                    Some(path) => format!("Will write {}", file_name(Path::new(path))),
                    None => "Ready".into(),
                },
                OK,
            ),
            Validation::Failed { message, .. } => (message.clone(), ERR),
        };
        let can_start = self.can_start();
        let queued = self.jobs.iter().any(|job| job.status == JobStatus::Running);
        let mut column = card().flex().flex_col().gap_2();
        if let Validation::Ready(ok) = &self.validation {
            for warning in &ok.warnings {
                column = column.child(div().text_sm().text_color(rgb(WARN)).child(warning.clone()));
            }
        }
        if let Some(notice) = &self.notice {
            column = column.child(div().text_sm().text_color(rgb(WARN)).child(notice.clone()));
        }
        column
            .child(div().text_sm().text_color(rgb(color)).child(text))
            .child(
                button("translate", if queued { "Add to queue" } else { "Translate" }, Kind::Primary, can_start)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.can_start() {
                            this.enqueue(cx);
                        }
                    })),
            )
    }

    fn render_env_banner(&self) -> Option<Div> {
        let env = self.env.as_ref()?;
        let mut problems = Vec::new();
        if env.root.is_none() {
            problems.push("Could not find the pi-docs repository (scripts/translate_pdf.py). Set PI_DOCS_ROOT.".to_string());
        } else {
            if let Err(error) = &env.node {
                problems.push(format!("Node.js >= 22.19 is required: {error}"));
            }
            if let Err(error) = &env.runner {
                problems.push(format!("Run `uv sync` in the repository, or install uv: {error}"));
            }
        }
        if problems.is_empty() {
            return None;
        }
        Some(
            div()
                .p_3()
                .rounded_lg()
                .bg(rgba(0xb91c1c18))
                .border_1()
                .border_color(rgb(ERR))
                .text_sm()
                .text_color(rgb(ERR))
                .flex()
                .flex_col()
                .gap_1()
                .children(problems),
        )
    }

    fn render_job(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let job = &self.jobs[index];
        let id = job.id;
        let (status_text, status_color) = match job.status {
            JobStatus::Queued => ("Queued", MUTED),
            JobStatus::Running => ("Running", PRIMARY),
            JobStatus::Done => ("Done", OK),
            JobStatus::Failed => ("Failed", ERR),
            JobStatus::Cancelled => ("Cancelled", MUTED),
        };
        let elapsed = job.started.map(|start| format_elapsed(job.finished.unwrap_or_else(Instant::now).duration_since(start)));
        let mut title_row = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(div().flex_1().truncate().child(file_name(&job.request.input)))
            .child(div().text_sm().text_color(rgb(status_color)).child(status_text));
        if let Some(elapsed) = elapsed {
            title_row = title_row.child(label(elapsed));
        }

        let mut column = card().flex().flex_col().gap_2().child(title_row).child(label(format!(
            "{} · {} · {}",
            job.request.backend.label(),
            job.request.model,
            job.request.output_mode.label()
        )));

        if matches!(job.status, JobStatus::Running | JobStatus::Done) {
            let fraction = job.progress.unwrap_or(0.0).clamp(0.0, 1.0);
            column = column
                .child(
                    div()
                        .h(px(6.))
                        .w_full()
                        .rounded_md()
                        .bg(rgb(0xe5e7eb))
                        .child(div().h_full().rounded_md().bg(rgb(PRIMARY)).w(gpui::relative(fraction))),
                )
                .child(label(if job.progress.is_some() || job.status == JobStatus::Done {
                    format!("{} · {:.0}%", job.stage, fraction * 100.0)
                } else {
                    format!("{} · working…", job.stage)
                }));
        } else if job.status == JobStatus::Queued {
            column = column.child(label("Waiting for the current job"));
        }

        for warning in job.warnings.iter().take(3) {
            column = column.child(div().text_xs().text_color(rgb(WARN)).child(warning.clone()));
        }

        if let Some((code, message)) = &job.error {
            column = column.child(div().text_sm().text_color(rgb(ERR)).child(message.clone()));
            let mut actions = div().flex().flex_wrap().gap_2();
            if code == "output_exists" {
                if let Some(path) = job.expected_output.clone() {
                    let path = PathBuf::from(path);
                    actions = actions.child(
                        button(("reveal-existing", id), "Show existing file", Kind::Secondary, true)
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&path))),
                    );
                }
                let target = self.output_dir.clone();
                actions = actions.child(
                    button(("other-folder", id), "Choose another folder…", Kind::Secondary, true)
                        .on_click(cx.listener(move |this, _, _, cx| this.choose_directory(target.clone(), cx))),
                );
            }
            if job.request.backend.has_login() && code == "bridge_failed" {
                let backend = job.request.backend;
                actions = actions.child(
                    button(("fix-sign-in", id), "Sign in…", Kind::Secondary, self.login.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| this.start_login(backend, cx))),
                );
            }
            column = column.child(actions);
        }

        let mut buttons = div().flex().flex_wrap().gap_2();
        match job.status {
            JobStatus::Queued | JobStatus::Running => {
                buttons = buttons.child(
                    button(("cancel", id), "Cancel", Kind::Danger, job.cancel_requested.is_none())
                        .on_click(cx.listener(move |this, _, _, cx| this.cancel_job(id, cx))),
                );
            }
            JobStatus::Done => {
                if let Some(path) = job.output.clone() {
                    let open_path = path.clone();
                    buttons = buttons
                        .child(
                            button(("open", id), "Open", Kind::Primary, true).on_click(cx.listener(move |this, _, _, cx| {
                                if let Err(error) = open::that_detached(&open_path) {
                                    this.notice = Some(format!("Could not open the file: {error}"));
                                    cx.notify();
                                }
                            })),
                        )
                        .child(
                            button(("reveal", id), "Show in folder", Kind::Secondary, true)
                                .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&path))),
                        );
                }
            }
            _ => {}
        }
        if !job.log.is_empty() {
            buttons = buttons.child(
                button(("log", id), if job.show_log { "Hide log" } else { "Show log" }, Kind::Secondary, true).on_click(
                    cx.listener(move |this, _, _, cx| {
                        if let Some(job) = this.jobs.iter_mut().find(|job| job.id == id) {
                            job.show_log = !job.show_log;
                        }
                        cx.notify();
                    }),
                ),
            );
        }
        column = column.child(buttons);

        if job.show_log {
            let start = job.log.len().saturating_sub(40);
            column = column.child(
                div()
                    .id(("log-view", id))
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0xf3f4f6))
                    .text_xs()
                    .text_color(rgb(TEXT))
                    .flex()
                    .flex_col()
                    .children(job.log[start..].iter().map(|line| div().child(line.clone()))),
            );
        }
        column.into_any_element()
    }

    fn render_login(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let login = self.login.as_ref()?;
        let backend = login.backend;
        let mut body = div().flex().flex_col().gap_3();
        for notice in &login.notices {
            body = body.child(div().text_sm().text_color(rgb(MUTED)).child(notice.clone()));
        }
        if let Some((uri, code)) = &login.device {
            body = body
                .child(div().text_sm().child(format!("Open {uri} and enter this code:")))
                .child(div().text_2xl().child(code.clone()));
        } else if let Some(url) = &login.auth_url {
            let url_open = url.clone();
            body = body
                .child(div().text_sm().child("Complete sign-in in your browser. If it did not open:"))
                .child(
                    button("open-auth-url", "Open sign-in page", Kind::Secondary, true)
                        .on_click(cx.listener(move |_, _, _, cx| cx.open_url(&url_open))),
                );
        }
        match &login.prompt {
            Some(LoginEvent::Select { message, options, .. }) => {
                body = body.child(div().text_sm().child(message.clone()));
                for (index, option) in options.iter().enumerate() {
                    let value = option.id.clone();
                    body = body.child(
                        button(("login-option", index), option.label.clone(), Kind::Secondary, true)
                            .on_click(cx.listener(move |this, _, _, cx| this.answer_login(value.clone(), cx))),
                    );
                }
            }
            Some(LoginEvent::Secret { message, .. }) | Some(LoginEvent::Text { message, .. }) => {
                let input = login.input.clone();
                body = body
                    .child(div().text_sm().child(message.clone()))
                    .child(login.input.clone())
                    .child(
                        button("login-submit", "Submit", Kind::Primary, true).on_click(cx.listener(move |this, _, _, cx| {
                            let value = input.read(cx).text().to_string();
                            this.answer_login(value, cx);
                        })),
                    );
            }
            _ => {}
        }
        if login.done {
            body = body.child(div().text_sm().text_color(rgb(OK)).child("Signed in. Credentials are stored outside the repository."));
        }
        if let Some(error) = &login.error {
            body = body.child(div().text_sm().text_color(rgb(ERR)).child(error.clone()));
        }
        let finished = login.done || login.error.is_some();
        let panel = div()
            .id("login-panel")
            .w(px(420.))
            .p_4()
            .rounded_lg()
            .bg(rgb(CARD))
            .border_1()
            .border_color(rgb(BORDER))
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().child(format!("Sign in to {}", backend.label())))
            .child(body)
            .child(
                div().flex().justify_end().child(
                    button("login-close", if finished { "Close" } else { "Cancel" }, Kind::Secondary, true)
                        .on_click(cx.listener(|this, _, _, cx| this.close_login(cx))),
                ),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(rgba(0x00000066))
                .flex()
                .items_center()
                .justify_center()
                .child(panel)
                .into_any_element(),
        )
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let banner = self.render_env_banner();
        let left = div()
            .id("left-column")
            .w(px(420.))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(div().text_xl().child("pi-docs"))
            .child(label("Translate English PDFs into Simplified Chinese with BabelDOC"))
            .children(banner)
            .child(self.render_input_card(cx))
            .child(self.render_backend_card(cx))
            .child(self.render_output_card(cx))
            .child(self.render_advanced_card(cx))
            .child(self.render_start_card(cx));

        let mut jobs = div().flex().flex_col().gap_3();
        if self.jobs.is_empty() {
            jobs = jobs.child(div().p_6().text_color(rgb(MUTED)).child("Translations will appear here."));
        } else {
            for index in (0..self.jobs.len()).rev() {
                jobs = jobs.child(self.render_job(index, cx));
            }
        }
        let right = div()
            .id("right-column")
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .p_4()
            .border_l_1()
            .border_color(rgb(BORDER))
            .child(div().pb_3().text_lg().child("Translations"))
            .child(jobs);

        div()
            .key_context("App")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ChooseFile, _, cx| this.choose_file(cx)))
            .on_action(cx.listener(|this, _: &StartTranslation, _, cx| {
                if this.can_start() {
                    this.enqueue(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CancelCurrent, _, cx| this.cancel_current(cx)))
            .relative()
            .size_full()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .text_sm()
            .child(left)
            .child(right)
            .children(self.render_login(cx))
    }
}
