use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use valuable::Valuable;

use crate::executable::{Executable, ExecutableError, ExecutableParams, FailMode, run_command};
use crate::model::Message;

/// Structured hook failure. Replaces every `reason: String` payload; pure
/// enum (breaking wire format by design — scripts emit the new shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum HookError {
    Denied { rule: String, detail: String },
    Aborted { scope: AbortScope, detail: String },
    CheckFailed { check: String, detail: String },
}

impl HookError {
    pub fn detail(&self) -> &str {
        match self {
            Self::Denied { detail, .. }
            | Self::Aborted { detail, .. }
            | Self::CheckFailed { detail, .. } => detail,
        }
    }

    pub fn denied(rule: &str, detail: &str) -> Self {
        Self::Denied {
            rule: rule.to_string(),
            detail: detail.to_string(),
        }
    }

    pub fn aborted(scope: AbortScope, detail: &str) -> Self {
        Self::Aborted {
            scope,
            detail: detail.to_string(),
        }
    }

    pub fn check_failed(check: &str, detail: &str) -> Self {
        Self::CheckFailed {
            check: check.to_string(),
            detail: detail.to_string(),
        }
    }
}

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied { rule, detail } => write!(f, "denied by {rule}: {detail}"),
            Self::Aborted { scope, detail } => write!(f, "aborted ({scope}): {detail}"),
            Self::CheckFailed { check, detail } => {
                write!(f, "check {check} failed: {detail}")
            }
        }
    }
}

impl std::error::Error for HookError {}

/// Abort level for [`HookError::Aborted`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortScope {
    Step,
    Workflow,
    Session,
    Turn,
}

impl std::fmt::Display for AbortScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Step => write!(f, "step"),
            Self::Workflow => write!(f, "workflow"),
            Self::Session => write!(f, "session"),
            Self::Turn => write!(f, "turn"),
        }
    }
}

/// A hook slot: static options (from TOML) plus typed harness-built input
/// and concrete output. Options stay plain data; execution lives in
/// [`HookRunner`]. Each slot maps runner failures into its own outcome via
/// [`Slot::fallback`] so timeouts honor [`FailMode`].
pub trait Slot: std::fmt::Debug + 'static {
    type Options: std::fmt::Debug + PartialEq + for<'de> Deserialize<'de>;
    type Input<'a>: Serialize;
    type Output: std::fmt::Debug + PartialEq + DeserializeOwned + Default;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        let _ = (error, fail_mode);
        Self::Output::default()
    }
}

/// Reusable hook reference. All fields private; resolve `executable` via the
/// [`crate::executable::ExecutableRegistry`]. `T::Options` holds only static
/// knobs — never JSON schema (tools own that).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Hook<T: Slot> {
    name: String,
    executable: String,
    #[serde(flatten)]
    options: T::Options,
}

impl<T: Slot> Hook<T> {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn executable(&self) -> &str {
        &self.executable
    }

    pub fn options(&self) -> &T::Options {
        &self.options
    }
}

// ---------------------------------------------------------------------------
// Option structs (private fields + accessors)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct EmptyOptions;

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct SessionStartOptions;

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct SessionEndOptions;

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct TurnStartOptions {
    max_tokens: Option<usize>,
}

impl TurnStartOptions {
    pub fn max_tokens(&self) -> Option<usize> {
        self.max_tokens
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct TurnEndOptions;

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct VerifyOptions {
    min_iterations: Option<usize>,
}

impl VerifyOptions {
    pub fn min_iterations(&self) -> Option<usize> {
        self.min_iterations
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct StepOptions {
    abort_on_fail: Option<bool>,
}

impl StepOptions {
    pub fn abort_on_fail(&self) -> Option<bool> {
        self.abort_on_fail
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct PrePromptOptions {
    max_tokens: Option<usize>,
}

impl PrePromptOptions {
    pub fn max_tokens(&self) -> Option<usize> {
        self.max_tokens
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct PreToolOptions {
    mode: Option<String>,
}

impl PreToolOptions {
    pub fn mode(&self) -> Option<&str> {
        self.mode.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct PostToolOptions {
    max_bytes: Option<usize>,
}

impl PostToolOptions {
    pub fn max_bytes(&self) -> Option<usize> {
        self.max_bytes
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default, Valuable)]
pub struct ContextHookOptions {
    max_tokens: Option<usize>,
}

impl ContextHookOptions {
    pub fn max_tokens(&self) -> Option<usize> {
        self.max_tokens
    }
}

// ---------------------------------------------------------------------------
// Slot markers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionStartSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionEndSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnStartSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnEndSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifySlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StepSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrePromptSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreToolSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostToolSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnErrorSlot;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextSlot;

// ---------------------------------------------------------------------------
// Inputs (harness-built, Serialize to stdin JSON). All fields private;
// construct via `new()` and read via accessors.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SessionStartInput<'a> {
    session_id: &'a str,
    workflow: &'a str,
    vcs_rev: Option<&'a str>,
}

impl<'a> SessionStartInput<'a> {
    pub fn new(session_id: &'a str, workflow: &'a str, vcs_rev: Option<&'a str>) -> Self {
        Self {
            session_id,
            workflow,
            vcs_rev,
        }
    }

    pub fn session_id(&self) -> &str {
        self.session_id
    }

    pub fn workflow(&self) -> &str {
        self.workflow
    }

    pub fn vcs_rev(&self) -> Option<&str> {
        self.vcs_rev
    }
}

#[derive(Debug, Serialize)]
pub struct SessionEndInput<'a> {
    session_id: &'a str,
    workflow: &'a str,
    success: bool,
    n_messages: usize,
}

impl<'a> SessionEndInput<'a> {
    pub fn new(
        session_id: &'a str,
        workflow: &'a str,
        success: bool,
        n_messages: usize,
    ) -> Self {
        Self {
            session_id,
            workflow,
            success,
            n_messages,
        }
    }

    pub fn session_id(&self) -> &str {
        self.session_id
    }

    pub fn workflow(&self) -> &str {
        self.workflow
    }

    pub fn success(&self) -> bool {
        self.success
    }

    pub fn n_messages(&self) -> usize {
        self.n_messages
    }
}

#[derive(Debug, Serialize)]
pub struct TurnStartInput<'a> {
    agent: &'a str,
    iteration: usize,
    n_messages: usize,
    queued_len: usize,
    token_estimate: usize,
}

impl<'a> TurnStartInput<'a> {
    pub fn new(
        agent: &'a str,
        iteration: usize,
        n_messages: usize,
        queued_len: usize,
        token_estimate: usize,
    ) -> Self {
        Self {
            agent,
            iteration,
            n_messages,
            queued_len,
            token_estimate,
        }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn iteration(&self) -> usize {
        self.iteration
    }

    pub fn n_messages(&self) -> usize {
        self.n_messages
    }

    pub fn queued_len(&self) -> usize {
        self.queued_len
    }

    pub fn token_estimate(&self) -> usize {
        self.token_estimate
    }
}

#[derive(Debug, Serialize)]
pub struct TurnEndInput<'a> {
    agent: &'a str,
    iteration: usize,
    n_tool_calls: usize,
    has_text: bool,
}

impl<'a> TurnEndInput<'a> {
    pub fn new(agent: &'a str, iteration: usize, n_tool_calls: usize, has_text: bool) -> Self {
        Self {
            agent,
            iteration,
            n_tool_calls,
            has_text,
        }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn iteration(&self) -> usize {
        self.iteration
    }

    pub fn n_tool_calls(&self) -> usize {
        self.n_tool_calls
    }

    pub fn has_text(&self) -> bool {
        self.has_text
    }
}

#[derive(Debug, Serialize)]
pub struct VerifyInput<'a> {
    agent: &'a str,
    iteration: usize,
    last_text: Option<&'a str>,
    n_messages: usize,
}

impl<'a> VerifyInput<'a> {
    pub fn new(
        agent: &'a str,
        iteration: usize,
        last_text: Option<&'a str>,
        n_messages: usize,
    ) -> Self {
        Self {
            agent,
            iteration,
            last_text,
            n_messages,
        }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn iteration(&self) -> usize {
        self.iteration
    }

    pub fn last_text(&self) -> Option<&str> {
        self.last_text
    }

    pub fn n_messages(&self) -> usize {
        self.n_messages
    }
}

#[derive(Debug, Serialize)]
pub struct StepHookInput<'a> {
    workflow: &'a str,
    step: &'a str,
    session_id: &'a str,
}

impl<'a> StepHookInput<'a> {
    pub fn new(workflow: &'a str, step: &'a str, session_id: &'a str) -> Self {
        Self {
            workflow,
            step,
            session_id,
        }
    }

    pub fn workflow(&self) -> &str {
        self.workflow
    }

    pub fn step(&self) -> &str {
        self.step
    }

    pub fn session_id(&self) -> &str {
        self.session_id
    }
}

#[derive(Debug, Serialize)]
pub struct PrePromptInput<'a> {
    agent: &'a str,
    n_messages: usize,
}

impl<'a> PrePromptInput<'a> {
    pub fn new(agent: &'a str, n_messages: usize) -> Self {
        Self { agent, n_messages }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn n_messages(&self) -> usize {
        self.n_messages
    }
}

#[derive(Debug, Serialize)]
pub struct PreToolInput<'a> {
    agent: &'a str,
    call_id: &'a str,
    tool: &'a str,
    args: &'a Value,
}

impl<'a> PreToolInput<'a> {
    pub fn new(agent: &'a str, call_id: &'a str, tool: &'a str, args: &'a Value) -> Self {
        Self {
            agent,
            call_id,
            tool,
            args,
        }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn call_id(&self) -> &str {
        self.call_id
    }

    pub fn tool(&self) -> &str {
        self.tool
    }

    pub fn args(&self) -> &Value {
        self.args
    }
}

#[derive(Debug, Serialize)]
pub struct PostToolInput<'a> {
    agent: &'a str,
    call_id: &'a str,
    tool: &'a str,
    output_len: usize,
}

impl<'a> PostToolInput<'a> {
    pub fn new(agent: &'a str, call_id: &'a str, tool: &'a str, output_len: usize) -> Self {
        Self {
            agent,
            call_id,
            tool,
            output_len,
        }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn call_id(&self) -> &str {
        self.call_id
    }

    pub fn tool(&self) -> &str {
        self.tool
    }

    pub fn output_len(&self) -> usize {
        self.output_len
    }
}

#[derive(Debug, Serialize)]
pub struct OnErrorInput<'a> {
    agent: &'a str,
    phase: &'a str,
    error: &'a str,
}

impl<'a> OnErrorInput<'a> {
    pub fn new(agent: &'a str, phase: &'a str, error: &'a str) -> Self {
        Self { agent, phase, error }
    }

    pub fn agent(&self) -> &str {
        self.agent
    }

    pub fn phase(&self) -> &str {
        self.phase
    }

    pub fn error(&self) -> &str {
        self.error
    }
}

// ---------------------------------------------------------------------------
// Outputs (concrete per slot, Deserialize from stdout JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SessionStartOutcome {
    #[default]
    Continue,
    Abort { error: HookError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SessionEndOutcome {
    #[default]
    Continue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum TurnStartOutcome {
    #[default]
    Continue,
    PruneTo {
        keep: usize,
    },
    Abort { error: HookError },
}

impl TurnStartOutcome {
    pub fn is_continue(&self) -> bool {
        matches!(self, Self::Continue)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum TurnEndOutcome {
    #[default]
    Continue,
    DropTurn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum VerifyOutcome {
    #[default]
    Done,
    NotDone { error: HookError },
}

impl VerifyOutcome {
    pub fn is_done(&self) -> bool {
        matches!(self, Self::Done)
    }

    pub fn feedback(&self) -> Option<&str> {
        match self {
            Self::Done => None,
            Self::NotDone { error } => Some(error.detail()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum StepHookOutcome {
    #[default]
    Continue,
    AbortStep { error: HookError },
    AbortWorkflow { error: HookError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PrePromptOutcome {
    #[default]
    Continue,
    Abort { error: HookError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PreToolOutcome {
    #[default]
    Continue,
    Deny { error: HookError },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PostToolOutcome {
    #[default]
    Continue,
    DropResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum OnErrorOutcome {
    #[default]
    Continue,
    Abort { error: HookError },
}

// ---------------------------------------------------------------------------
// Slot impls
// ---------------------------------------------------------------------------

impl Slot for SessionStartSlot {
    type Options = SessionStartOptions;
    type Input<'a> = SessionStartInput<'a>;
    type Output = SessionStartOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => SessionStartOutcome::Abort {
                error: HookError::aborted(AbortScope::Session, &error.to_string()),
            },
        }
    }
}

impl Slot for SessionEndSlot {
    type Options = SessionEndOptions;
    type Input<'a> = SessionEndInput<'a>;
    type Output = SessionEndOutcome;
}

impl Slot for TurnStartSlot {
    type Options = TurnStartOptions;
    type Input<'a> = TurnStartInput<'a>;
    type Output = TurnStartOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => TurnStartOutcome::Abort {
                error: HookError::aborted(AbortScope::Turn, &error.to_string()),
            },
        }
    }
}

impl Slot for TurnEndSlot {
    type Options = TurnEndOptions;
    type Input<'a> = TurnEndInput<'a>;
    type Output = TurnEndOutcome;
}

impl Slot for VerifySlot {
    type Options = VerifyOptions;
    type Input<'a> = VerifyInput<'a>;
    type Output = VerifyOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => VerifyOutcome::NotDone {
                error: HookError::check_failed("hook", &error.to_string()),
            },
        }
    }
}

impl Slot for StepSlot {
    type Options = StepOptions;
    type Input<'a> = StepHookInput<'a>;
    type Output = StepHookOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep => StepHookOutcome::AbortStep {
                error: HookError::aborted(AbortScope::Step, &error.to_string()),
            },
            FailMode::AbortWorkflow => StepHookOutcome::AbortWorkflow {
                error: HookError::aborted(AbortScope::Workflow, &error.to_string()),
            },
        }
    }
}

impl Slot for PrePromptSlot {
    type Options = PrePromptOptions;
    type Input<'a> = PrePromptInput<'a>;
    type Output = PrePromptOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => PrePromptOutcome::Abort {
                error: HookError::aborted(AbortScope::Turn, &error.to_string()),
            },
        }
    }
}

impl Slot for PreToolSlot {
    type Options = PreToolOptions;
    type Input<'a> = PreToolInput<'a>;
    type Output = PreToolOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => PreToolOutcome::Deny {
                error: HookError::denied("hook", &error.to_string()),
            },
        }
    }
}

impl Slot for PostToolSlot {
    type Options = PostToolOptions;
    type Input<'a> = PostToolInput<'a>;
    type Output = PostToolOutcome;
}

impl Slot for OnErrorSlot {
    type Options = EmptyOptions;
    type Input<'a> = OnErrorInput<'a>;
    type Output = OnErrorOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => OnErrorOutcome::Abort {
                error: HookError::aborted(AbortScope::Step, &error.to_string()),
            },
        }
    }
}

impl Slot for ContextSlot {
    type Options = ContextHookOptions;
    type Input<'a> = TurnStartInput<'a>;
    type Output = TurnStartOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        TurnStartSlot::fallback(error, fail_mode)
    }
}

// ---------------------------------------------------------------------------
// Mutable state for builtins. Externals get IDs + JSON and return patches.
// ---------------------------------------------------------------------------

/// Mutable harness state available to `builtin` executables only.
#[derive(Debug)]
pub struct HookState<'a> {
    messages: &'a mut Vec<Message>,
    session_id: &'a str,
}

impl<'a> HookState<'a> {
    pub fn new(messages: &'a mut Vec<Message>, session_id: &'a str) -> Self {
        Self {
            messages,
            session_id,
        }
    }

    pub fn session_id(&self) -> &str {
        self.session_id
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn push_user(&mut self, content: String) {
        self.messages.push(Message::new_user(content));
    }

    pub fn prune_to(&mut self, keep: usize) -> usize {
        if self.messages.len() <= keep {
            return 0;
        }
        let remove = self.messages.len() - keep;
        self.messages.drain(..remove);
        remove
    }
}

// ---------------------------------------------------------------------------
// Runner: mockable seam. Real impl below; tests inject fakes.
// ---------------------------------------------------------------------------

/// Mockable execution seam. Production wires [`RealHookRunner`] driven purely
/// by `fyah.toml`; tests inject a fake returning canned `Output`s.
pub trait HookRunner: std::fmt::Debug {
    fn run_hook<S: Slot>(
        &self,
        exe: &Executable,
        hook: &Hook<S>,
        input: &S::Input<'_>,
        state: &mut HookState<'_>,
    ) -> impl std::future::Future<Output = S::Output>;
}

#[derive(Debug, Default)]
pub struct RealHookRunner;

impl HookRunner for RealHookRunner {
    async fn run_hook<S: Slot>(
        &self,
        exe: &Executable,
        hook: &Hook<S>,
        input: &S::Input<'_>,
        // Reserved for builtin handlers that mutate state directly;
        // external executables receive IDs + JSON and return patches.
        _state: &mut HookState<'_>,
    ) -> S::Output {
        match exe.params() {
            ExecutableParams::Builtin { builtin } => {
                match run_builtin_name(builtin, hook.name()) {
                    Ok(()) => {
                        tracing::debug!(builtin, "builtin hook executed");
                        S::Output::default()
                    }
                    Err(error) => {
                        tracing::warn!(hook = hook.name(), error = %error, "unknown builtin");
                        S::fallback(&error, exe.fail_mode())
                    }
                }
            }
            ExecutableParams::Command { command, args } => {
                let stdin = serde_json::to_value(input).unwrap_or(Value::Null);
                let static_args: &[String] = args.as_deref().unwrap_or(&[]);
                match run_command(exe, command, static_args, &stdin).await {
                    Ok(Value::Null) => S::Output::default(),
                    Ok(v) => match serde_json::from_value::<S::Output>(v) {
                        Ok(out) => out,
                        Err(e) => {
                            let error = ExecutableError::InvalidOutput {
                                detail: e.to_string(),
                            };
                            tracing::warn!(hook = hook.name(), error = %error, "hook output not understood");
                            S::fallback(&error, exe.fail_mode())
                        }
                    },
                    Err(error) => {
                        tracing::warn!(hook = hook.name(), error = %error, "hook command failed");
                        S::fallback(&error, exe.fail_mode())
                    }
                }
            }
            ExecutableParams::Wasm { .. } | ExecutableParams::Plugin { .. } => {
                let error = ExecutableError::NotImplemented {
                    kind: exe.params().kind().to_string(),
                };
                tracing::warn!(hook = hook.name(), error = %error, "executable kind not implemented");
                S::fallback(&error, exe.fail_mode())
            }
        }
    }
}

/// Built-in name check. Unknown names surface as
/// [`ExecutableError::UnknownBuiltin`] so `fail_mode` decides fail-open vs
/// fail-closed; validation catches typos at load time.
fn run_builtin_name(builtin: &str, _hook_name: &str) -> Result<(), ExecutableError> {
    match builtin {
        "noop" | "read_file" | "write_file" | "bash" | "prune_outputs" | "audit_append"
        | "budget_guard" => Ok(()),
        _ => Err(ExecutableError::UnknownBuiltin {
            name: builtin.to_string(),
        }),
    }
}
