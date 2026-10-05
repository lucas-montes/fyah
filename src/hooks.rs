use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use valuable::Valuable;

use crate::executable::{Executable, ExecutableError, ExecutableParams, FailMode, run_command};
use crate::model::Message;

/// A hook's decision plus the captured streams.
///
/// The typed wrapper needs the raw streams when the decision turns out not to
/// match the slot's shape: that is the one case where `stdout` is the most
/// useful thing to log, and the erased runner is the only place they survive.
pub struct HookDecision {
    value: Value,
    stdout_raw: String,
    stderr: String,
}

impl std::fmt::Debug for HookDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookDecision")
            .field("value", &self.value)
            .finish_non_exhaustive()
    }
}

impl HookDecision {
    /// No decision: the slot should use its default.
    pub fn none() -> Self {
        Self {
            value: Value::Null,
            stdout_raw: String::new(),
            stderr: String::new(),
        }
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// A decision with no captured streams (tests, builtins).
    pub fn from_value(value: Value) -> Self {
        Self {
            value,
            stdout_raw: String::new(),
            stderr: String::new(),
        }
    }
}

/// Structured hook failure. Replaces every `reason: String` payload; pure
/// enum (breaking wire format by design — scripts emit the new shape).
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    const NAME: &'static str;
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
/// knobs — never JSON schema (tools own that). The slot is known from `T`,
/// so no user-supplied `name` is needed; logging uses `T::NAME`.
#[derive(Debug, Deserialize, PartialEq)]
pub struct Hook<T: Slot> {
    executable: String,
    #[serde(flatten)]
    options: T::Options,
}

impl<T: Slot> Hook<T> {
    /// Build a hook reference programmatically (e.g. when the executable key
    /// lives in a sibling config table rather than a `Hook` TOML table).
    /// Options take their `Default`; TOML-supplied options still deserialize.
    pub fn new(executable: String) -> Self
    where
        T::Options: Default,
    {
        Self {
            executable,
            options: T::Options::default(),
        }
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

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct EmptyOptions;

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct SessionStartOptions;

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct SessionEndOptions;

// T01: turn_start/turn_end removed (use [agents.<name>.context] strategy="custom").
// T03: TurnStartInput/TurnStartOutcome replaced by CustomInput/CustomOutcome.

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct VerifyOptions {
    min_iterations: Option<usize>,
}

impl VerifyOptions {
    pub fn min_iterations(&self) -> Option<usize> {
        self.min_iterations
    }
}

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct StepOptions {
    abort_on_fail: Option<bool>,
}

impl StepOptions {
    pub fn abort_on_fail(&self) -> Option<bool> {
        self.abort_on_fail
    }
}

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct PrePromptOptions {
    max_tokens: Option<usize>,
}

impl PrePromptOptions {
    pub fn max_tokens(&self) -> Option<usize> {
        self.max_tokens
    }
}

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct PreToolOptions {
    mode: Option<String>,
}

impl PreToolOptions {
    pub fn mode(&self) -> Option<&str> {
        self.mode.as_deref()
    }
}

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
pub struct PostToolOptions {
    max_bytes: Option<usize>,
}

impl PostToolOptions {
    pub fn max_bytes(&self) -> Option<usize> {
        self.max_bytes
    }
}

#[derive(Debug, Deserialize, PartialEq, Default, Valuable)]
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

#[derive(Debug, PartialEq, Eq)]
pub struct SessionStartSlot;
#[derive(Debug, PartialEq, Eq)]
pub struct SessionEndSlot;
// T01: TurnStartSlot/TurnEndSlot removed (use context custom strategy).
#[derive(Debug, PartialEq, Eq)]
pub struct VerifySlot;
#[derive(Debug, PartialEq, Eq)]
pub struct StepSlot;
#[derive(Debug, PartialEq, Eq)]
pub struct PrePromptSlot;
#[derive(Debug, PartialEq, Eq)]
pub struct PreToolSlot;
#[derive(Debug, PartialEq, Eq)]
pub struct PostToolSlot;
#[derive(Debug, PartialEq, Eq)]
pub struct OnErrorSlot;
#[derive(Debug, PartialEq, Eq)]
pub struct LoopGuardSlot;
#[derive(Debug, PartialEq, Eq)]
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
pub struct CustomInput<'a> {
    agent: &'a str,
    iteration: usize,
    n_messages: usize,
    token_estimate: usize,
    tool_output_bytes: usize,
    max_tokens: Option<u64>,
    max_messages: Option<usize>,
    max_output_bytes: Option<usize>,
}

impl<'a> CustomInput<'a> {
    // Nine fields mirror the meter snapshot; a struct literal at call sites
    // would be noisier than the lint it silences.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: &'a str,
        iteration: usize,
        n_messages: usize,
        token_estimate: usize,
        tool_output_bytes: usize,
        max_tokens: Option<u64>,
        max_messages: Option<usize>,
        max_output_bytes: Option<usize>,
    ) -> Self {
        Self {
            agent,
            iteration,
            n_messages,
            token_estimate,
            tool_output_bytes,
            max_tokens,
            max_messages,
            max_output_bytes,
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

    pub fn token_estimate(&self) -> usize {
        self.token_estimate
    }

    pub fn tool_output_bytes(&self) -> usize {
        self.tool_output_bytes
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
    iteration: usize,
    n_messages: usize,
}

impl<'a> PrePromptInput<'a> {
    pub fn new(agent: &'a str, iteration: usize, n_messages: usize) -> Self {
        Self {
            agent,
            iteration,
            n_messages,
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

/// Snapshot for the `[agents.<name>.limits]` guard executable: every meter
/// the harness tracks, so custom logic can stop the loop on any condition.
#[derive(Debug, Serialize)]
pub struct LoopGuardInput<'a> {
    agent: &'a str,
    iteration: usize,
    n_messages: usize,
    token_estimate: usize,
    total_tokens_est: u64,
    tool_calls_total: usize,
    elapsed_secs: u64,
    consecutive_errors: u32,
    stalled_turns: u32,
}

impl<'a> LoopGuardInput<'a> {
    // Nine fields mirror the meter snapshot; a struct literal at call sites
    // would be noisier than the lint it silences.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent: &'a str,
        iteration: usize,
        n_messages: usize,
        token_estimate: usize,
        total_tokens_est: u64,
        tool_calls_total: usize,
        elapsed_secs: u64,
        consecutive_errors: u32,
        stalled_turns: u32,
    ) -> Self {
        Self {
            agent,
            iteration,
            n_messages,
            token_estimate,
            total_tokens_est,
            tool_calls_total,
            elapsed_secs,
            consecutive_errors,
            stalled_turns,
        }
    }
}

// ---------------------------------------------------------------------------
// Outputs (concrete per slot, Deserialize from stdout JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SessionStartOutcome {
    #[default]
    Continue,
    Abort { error: HookError },
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum SessionEndOutcome {
    #[default]
    Continue,
}

// T03: Custom compaction outcome. Shapes of continue/prune_to/abort match
// the old TurnStartOutcome so existing scripts keep parsing.
#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum CustomOutcome {
    #[default]
    Continue,
    PruneTo {
        keep: usize,
    },
    TruncateOutputs {
        max_bytes: usize,
    },
    DropToolResults {
        ids: Vec<String>,
    },
    Abort { error: HookError },
}

// T01: TurnEndOutcome removed with turn_end (no replacement; verify_done covers done-checks).

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
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

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum StepHookOutcome {
    #[default]
    Continue,
    AbortStep { error: HookError },
    AbortWorkflow { error: HookError },
}

/// Injection scope for [`PrePromptOutcome::Inject`].
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InjectMode {
    /// Visible to this LLM call only; never persisted to history.
    Ephemeral,
    /// Appended as a `User` message; auditable and billed like any message.
    Persistent,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PrePromptOutcome {
    #[default]
    Continue,
    Inject { mode: InjectMode, content: String },
    Abort { error: HookError },
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PreToolOutcome {
    #[default]
    Continue,
    Deny { error: HookError },
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PostToolOutcome {
    #[default]
    Continue,
    DropResult,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum OnErrorOutcome {
    #[default]
    Continue,
    Abort { error: HookError },
}

/// Decision of the `[agents.<name>.limits]` guard executable.
#[derive(Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LoopGuardOutcome {
    #[default]
    Continue,
    Stop { reason: String },
}

// ---------------------------------------------------------------------------
// Slot impls
// ---------------------------------------------------------------------------

impl Slot for SessionStartSlot {
    const NAME: &'static str = "session_start";
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
    const NAME: &'static str = "session_end";
    type Options = SessionEndOptions;
    type Input<'a> = SessionEndInput<'a>;
    type Output = SessionEndOutcome;
}

impl Slot for VerifySlot {
    const NAME: &'static str = "verify_done";
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
    const NAME: &'static str = "step_hook";
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
    const NAME: &'static str = "pre_prompt";
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
    const NAME: &'static str = "pre_tool";
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
    const NAME: &'static str = "post_tool";
    type Options = PostToolOptions;
    type Input<'a> = PostToolInput<'a>;
    type Output = PostToolOutcome;
}

impl Slot for OnErrorSlot {
    const NAME: &'static str = "on_error";
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

impl Slot for LoopGuardSlot {
    const NAME: &'static str = "loop_guard";
    type Options = EmptyOptions;
    type Input<'a> = LoopGuardInput<'a>;
    type Output = LoopGuardOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => LoopGuardOutcome::Stop {
                reason: error.to_string(),
            },
        }
    }
}

impl Slot for ContextSlot {
    const NAME: &'static str = "context";
    type Options = ContextHookOptions;
    type Input<'a> = CustomInput<'a>;
    type Output = CustomOutcome;

    fn fallback(error: &ExecutableError, fail_mode: &FailMode) -> Self::Output {
        match fail_mode {
            FailMode::Message => Self::Output::default(),
            FailMode::AbortStep | FailMode::AbortWorkflow => CustomOutcome::Abort {
                error: HookError::aborted(AbortScope::Turn, &error.to_string()),
            },
        }
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

    /// Truncate every `Tool` message longer than `max_bytes`, mirroring
    /// [`crate::context::ContextOutcome::TruncateOutputs`]. Returns the
    /// number of messages truncated.
    pub fn truncate_outputs(&mut self, max_bytes: usize) -> usize {
        let mut truncated = 0;
        for msg in self.messages.iter_mut() {
            if let Message::Tool { content, .. } = msg
                && content.len() > max_bytes
            {
                content.truncate(max_bytes);
                content.push_str("\n[truncated]");
                truncated += 1;
            }
        }
        truncated
    }

    /// Drop `Tool` messages whose `tool_call_id` is listed. Returns the
    /// number of messages removed.
    pub fn drop_tool_results(&mut self, ids: &[String]) -> usize {
        let before = self.messages.len();
        self.messages.retain(|msg| match msg {
            Message::Tool { tool_call_id, .. } => !ids.contains(tool_call_id),
            _ => true,
        });
        before - self.messages.len()
    }
}

// ---------------------------------------------------------------------------
// Runner: mockable seam. Real impl below; tests inject fakes.
// ---------------------------------------------------------------------------

/// Mockable execution seam. Production wires [`RealHookRunner`] driven purely
/// by `fyah.toml`; tests inject a fake returning canned `Output`s.
///
/// Futures are returned directly — `impl Future`, no heap boxing — now that
/// nothing uses this trait as a trait object. The typed entry point is the
/// provided [`HookRunner::run_hook`] method, which wraps [`HookRunner::run_json`]
/// and applies [`Slot::fallback`].
pub trait HookRunner: std::fmt::Debug {
    /// Run a hook, type-erased: slot and decision are plain JSON.
    ///
    /// `Ok` decision with a `Null` value means "no decision", which every slot reads as its
    /// `Default`. `Err` is a transport or parse failure; applying the slot's
    /// `fail_mode` needs `S`, so that happens in [`HookRunner::run_hook`].
    fn run_json<'a, 'b>(
        &'a self,
        exe: &'a Executable,
        slot: &'a str,
        executable: &'a str,
        input: &'a Value,
        state: &'a mut HookState<'b>,
    ) -> impl Future<Output = Result<HookDecision, ExecutableError>> + 'a
    where
        'b: 'a;

    /// Typed hook execution: serialise the input, call [`HookRunner::run_json`],
    /// parse the decision back, and apply [`Slot::fallback`] on either failure.
    ///
    /// A provided generic method — possible because the trait is never used
    /// behind `dyn`, where any generic method would be forbidden.
    ///
    /// All call borrows share one region: the future cannot outlive the
    /// shortest of them, so callers simply lend everything for the call.
    fn run_hook<'a, 't, S: Slot>(
        &'a self,
        exe: &'a Executable,
        hook: &'a Hook<S>,
        input: &'a S::Input<'a>,
        state: &'a mut HookState<'t>,
    ) -> impl Future<Output = S::Output> + 'a
    where
        // The state cannot be lent for longer than the messages it borrows.
        't: 'a,
    {
        async move {
            let json = serde_json::to_value(input).unwrap_or(Value::Null);
            let decision =
                match self.run_json(exe, S::NAME, hook.executable(), &json, state).await {
                    Ok(decision) => decision,
                    Err(error) => return S::fallback(&error, exe.fail_mode()),
                };
            // Empty stdout means "no decision": every slot's Default.
            if decision.value().is_null() {
                return S::Output::default();
            }
            match serde_json::from_value::<S::Output>(decision.value().clone()) {
                Ok(parsed) => parsed,
                Err(error) => {
                    // The slot rejected a well-formed JSON decision. `run_json`
                    // cannot do this check (it has no `S`), so the warning lives here.
                    tracing::warn!(
                        slot = S::NAME,
                        executable = hook.executable(),
                        error = %error,
                        stdout_raw = decision.stdout_raw,
                        stderr = decision.stderr,
                        fail_mode = ?exe.fail_mode(),
                        "hook output not understood"
                    );
                    S::fallback(
                        &ExecutableError::InvalidOutput {
                            detail: error.to_string(),
                        },
                        exe.fail_mode(),
                    )
                }
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct RealHookRunner;

impl HookRunner for RealHookRunner {
    async fn run_json<'a, 'b>(
        &'a self,
        exe: &'a Executable,
        slot: &'a str,
        executable: &'a str,
        input: &'a Value,
        // Reserved for builtin handlers that mutate state directly;
        // external executables receive IDs + JSON and return patches.
        _state: &'a mut HookState<'b>,
    ) -> Result<HookDecision, ExecutableError>
    where
        'b: 'a,
    {
        let kind = exe.params().kind();
        let start = std::time::Instant::now();
        tracing::debug!(
            slot,
            executable,
            kind,
            fail_mode = ?exe.fail_mode(),
            "hook start"
        );
        if tracing::enabled!(tracing::Level::TRACE) {
            tracing::trace!(slot, executable, input = %input, "hook input");
        }
        {
            match exe.params() {
                ExecutableParams::Builtin { builtin } => match run_builtin_name(builtin) {
                    Ok(()) => {
                        tracing::debug!(
                            slot,
                            executable,
                            builtin,
                            elapsed_ms = start.elapsed().as_millis() as u64,
                            "builtin hook executed"
                        );
                        // No decision: the wrapper turns this into the slot default.
                        Ok(HookDecision::none())
                    }
                    Err(error) => {
                        tracing::warn!(
                            slot,
                            executable,
                            builtin,
                            error = %error,
                            fail_mode = ?exe.fail_mode(),
                            elapsed_ms = start.elapsed().as_millis() as u64,
                            "unknown builtin"
                        );
                        Err(error)
                    }
                },
                ExecutableParams::Command { command, args } => {
                    let static_args: &[String] = args.as_deref().unwrap_or(&[]);
                    tracing::debug!(
                        slot,
                        executable,
                        command,
                        args = ?static_args,
                        stdin_len = input.to_string().len(),
                        "spawning hook command"
                    );
                    match run_command(exe, command, static_args, input).await {
                        Ok(out) if *out.value() == Value::Null => {
                            tracing::debug!(
                                slot,
                                executable,
                                command,
                                exit_code = ?out.exit_code(),
                                success = out.success(),
                                elapsed_ms = out.elapsed_ms(),
                                stdin_len = out.stdin_len(),
                                stdout_len = out.stdout_len(),
                                stderr_len = out.stderr_len(),
                                stderr = out.stderr(),
                                truncated = out.truncated(),
                                "hook command completed with empty output (default outcome)"
                            );
                            Ok(HookDecision::none())
                        }
                        Ok(out) => {
                            let decision = HookDecision {
                                value: out.value().clone(),
                                stdout_raw: out.stdout_raw().to_string(),
                                stderr: out.stderr().to_string(),
                            };
                            tracing::debug!(
                                slot,
                                executable,
                                command,
                                exit_code = ?out.exit_code(),
                                success = out.success(),
                                elapsed_ms = out.elapsed_ms(),
                                stdin_len = out.stdin_len(),
                                stdout_len = out.stdout_len(),
                                stderr_len = out.stderr_len(),
                                stderr = out.stderr(),
                                truncated = out.truncated(),
                                outcome = %decision.value,
                                "hook command completed"
                            );
                            Ok(decision)
                        }
                        Err(failure) => {
                            tracing::warn!(
                                slot,
                                executable,
                                command,
                                error = %failure.error(),
                                exit_code = ?failure.exit_code(),
                                success = failure.success(),
                                timed_out = failure.timed_out(),
                                elapsed_ms = failure.elapsed_ms(),
                                stdin_len = failure.stdin_len(),
                                stdout_len = failure.stdout_len(),
                                stderr_len = failure.stderr_len(),
                                stdout_raw = failure.stdout_raw(),
                                stderr = failure.stderr(),
                                truncated = failure.truncated(),
                                fail_mode = ?exe.fail_mode(),
                                "hook command failed"
                            );
                            // `ExecutableError` is not `Clone`, so give up the
                            // telemetry we just logged to return the error.
                            Err(failure.into_error())
                        }
                    }
                }
                ExecutableParams::Wasm { path, function } => {
                    let error = ExecutableError::NotImplemented {
                        kind: exe.params().kind().to_string(),
                    };
                    tracing::warn!(
                        slot,
                        executable,
                        path,
                        function = ?function,
                        error = %error,
                        fail_mode = ?exe.fail_mode(),
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "executable kind not implemented"
                    );
                    Err(error)
                }
                ExecutableParams::Plugin { name, function } => {
                    let error = ExecutableError::NotImplemented {
                        kind: exe.params().kind().to_string(),
                    };
                    tracing::warn!(
                        slot,
                        executable,
                        name,
                        function = ?function,
                        error = %error,
                        fail_mode = ?exe.fail_mode(),
                        elapsed_ms = start.elapsed().as_millis() as u64,
                        "executable kind not implemented"
                    );
                    Err(error)
                }
            }
        }
    }
}

//TODO: not sure this is useful
/// Built-in name check. Unknown names surface as
/// [`ExecutableError::UnknownBuiltin`] so `fail_mode` decides fail-open vs
/// fail-closed; validation catches typos at load time.
fn run_builtin_name(builtin: &str) -> Result<(), ExecutableError> {
    match builtin {
        "noop" | "read_file" | "write_file" | "prune_outputs" | "audit_append"
        | "budget_guard" => Ok(()),
        _ => Err(ExecutableError::UnknownBuiltin {
            name: builtin.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_outcome_shapes() {
        // Legacy shapes keep parsing (scripts emitting prune_to/continue/abort).
        let out: CustomOutcome =
            serde_json::from_str(r#"{"decision":"continue"}"#).expect("parse");
        assert_eq!(out, CustomOutcome::Continue);
        let out: CustomOutcome =
            serde_json::from_str(r#"{"decision":"prune_to","keep":5}"#).expect("parse");
        assert_eq!(out, CustomOutcome::PruneTo { keep: 5 });
        // New ops.
        let out: CustomOutcome =
            serde_json::from_str(r#"{"decision":"truncate_outputs","max_bytes":100}"#)
                .expect("parse");
        assert_eq!(out, CustomOutcome::TruncateOutputs { max_bytes: 100 });
        let out: CustomOutcome =
            serde_json::from_str(r#"{"decision":"drop_tool_results","ids":["a"]}"#)
                .expect("parse");
        assert_eq!(
            out,
            CustomOutcome::DropToolResults {
                ids: vec!["a".to_string()]
            }
        );
    }

    #[test]
    fn hook_state_truncate_and_drop() {
        let mut messages = vec![
            Message::new_user("hello".to_string()),
            Message::new_tool("a".to_string(), "0123456789".to_string()),
            Message::new_tool("b".to_string(), "xy".to_string()),
        ];
        let mut state = HookState::new(&mut messages, "s");
        assert_eq!(state.truncate_outputs(4), 1);
        assert_eq!(state.drop_tool_results(&["b".to_string()]), 1);
        assert_eq!(state.message_count(), 2);
    }
}

#[cfg(test)]
mod fallback_tests {
    use super::*;

    /// A runner that answers with a fixed result, so the *wrapper's* split —
    /// erased call, then `S::fallback` — is what is under test.
    struct Stub {
        result: Result<Value, ExecutableError>,
    }

    impl std::fmt::Debug for Stub {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Stub")
        }
    }

    impl HookRunner for Stub {
        async fn run_json<'a, 'b>(
            &'a self,
            _exe: &'a Executable,
            _slot: &'a str,
            _executable: &'a str,
            _input: &'a Value,
            _state: &'a mut HookState<'b>,
        ) -> Result<HookDecision, ExecutableError>
        where
            'b: 'a,
        {
            match &self.result {
                Ok(v) => Ok(HookDecision::from_value(v.clone())),
                Err(_) => Err(ExecutableError::NotImplemented {
                    kind: "stub".to_string(),
                }),
            }
        }
    }

    fn command_exe(fail_mode: &str) -> Executable {
        toml::from_str(&format!(
            "type = \"command\"\ncommand = \"true\"\nfail_mode = \"{fail_mode}\"\n"
        ))
        .expect("parse executable")
    }

    #[tokio::test]
    async fn a_transport_failure_obeys_fail_mode() {
        let mut messages = vec![Message::new_user("hello".to_string())];
        let mut state = HookState::new(&mut messages, "s");
        let hook = Hook::<PrePromptSlot>::new("pp".to_string());
        let input = PrePromptInput::new("coder", 1, 1);

        // fail-open: the default outcome, i.e. Continue.
        let open = command_exe("message");
        let stub = Stub {
            result: Err(ExecutableError::NotImplemented {
                kind: "stub".to_string(),
            }),
        };
        let out = stub.run_hook( &open, &hook, &input, &mut state).await;
        assert_eq!(out, PrePromptOutcome::Continue);

        // fail-closed: Abort, mapped through the hook's own fail_mode.
        let closed = command_exe("abortworkflow");
        let out = stub.run_hook( &closed, &hook, &input, &mut state).await;
        assert!(
            matches!(out, PrePromptOutcome::Abort { .. }),
            "expected Abort, got {out:?}"
        );
    }

    #[tokio::test]
    async fn unparseable_output_obeys_fail_mode() {
        let mut messages = vec![Message::new_user("hello".to_string())];
        let mut state = HookState::new(&mut messages, "s");
        let hook = Hook::<PrePromptSlot>::new("pp".to_string());
        let input = PrePromptInput::new("coder", 1, 1);
        // Well-formed JSON the slot does not understand: same shape as a
        // script printing garbage, so it takes the InvalidOutput path.
        let stub = Stub {
            result: Ok(serde_json::json!({ "decision": "not_a_decision" })),
        };

        let open = command_exe("message");
        assert_eq!(
            stub.run_hook( &open, &hook, &input, &mut state).await,
            PrePromptOutcome::Continue,
            "fail-open turns an unreadable decision into the default"
        );

        let closed = command_exe("abortworkflow");
        assert!(
            matches!(
                stub.run_hook( &closed, &hook, &input, &mut state).await,
                PrePromptOutcome::Abort { .. }
            ),
            "fail-closed turns an unreadable decision into an abort"
        );
    }

    #[tokio::test]
    async fn an_empty_decision_is_the_slot_default() {
        let mut messages = vec![Message::new_user("hello".to_string())];
        let mut state = HookState::new(&mut messages, "s");
        let hook = Hook::<PrePromptSlot>::new("pp".to_string());
        let input = PrePromptInput::new("coder", 1, 1);
        let exe = command_exe("abortworkflow");
        let stub = Stub {
            result: Ok(Value::Null),
        };

        assert_eq!(stub.run_hook( &exe, &hook, &input, &mut state).await, PrePromptOutcome::Continue);
    }
}
