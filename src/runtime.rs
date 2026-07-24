use std::{collections::HashMap, path::PathBuf, time::Duration};

struct Workspace;

struct AgentContext;
struct SessionContext;

trait ContextBuilder {
    fn build(&self, session: SessionContext, workspace: Workspace) -> AgentContext;
}

trait PromptBuilder {
    fn build(&self, context: AgentContext, workspace: Workspace) -> Prompt;
}

struct Prompt;

enum Error {}

trait Client {
    fn execute(&self, prompt: Prompt) -> Result<Response, Error>;
}

struct Response;

struct Context {}
/// The result of executing a state: continue to the next state, or stop.
pub enum StateMachine<S> {
    Continue(StateFn<S>),
    Done,
}

struct Session {
    context: Context,
    cancelled: bool,
}

type StateFn<S> = fn(&mut S) -> StateMachine<S>;

// trait Step {
//     fn execute(&mut self) -> impl Future<Output = Result<(), Error>>;
// }

impl Session {
    pub fn run<S: Step<Self>>(mut self) {
        let mut step: StateFn<Self> = <S as Step<Self>>::run;

        while let StateMachine::Continue(next_step) = step(&mut self)
            && !self.cancelled
        {
            step = next_step;
        }
    }
}

pub trait Step<S> {
    fn run(rt: &mut S) -> StateMachine<S> {
        let _before_hook = Self::before_hook(rt);
        let result = Self::execute(rt);
        let _after_hook = Self::after_hook(rt);
        result
    }

    fn before_hook(rt: &mut S);

    fn after_hook(rt: &mut S);

    fn execute(rt: &mut S) -> StateMachine<S>;

    fn advance() -> StateMachine<S>;

    fn backtrack() -> StateMachine<S>;
}

struct Patient;
pub trait Department {
    fn execute(&mut self, patient: &mut Patient) {
        self.handle(patient);

        if let Some(next) = &mut self.next() {
            next.execute(patient);
        }
    }

    fn handle(&mut self, patient: &mut Patient);
    fn next(&mut self) -> &mut Option<Box<dyn Department>>;
}

struct AgentRuntime {
    steps: Vec<Phases>,
}

struct PhaseConfig {
    before_hook: Option<Box<dyn Fn(&mut Session)>>,
    after_hook: Option<Box<dyn Fn(&mut Session)>>,
}

#[derive(Debug)]
enum Phases {
    Plan,
    Implement,
    Test,
    Commit,
    Custom(String),
}

// ─── Shared base ──────────────────────────────────
pub struct StepBase {
    pub id: String,
    pub backend: Backend,
    pub on_success: String,
    pub on_failure: String,
    pub on_error: String,
    pub timeout: Option<Duration>,
    pub retry: Option<RetryPolicy>,
    pub description: Option<String>,
}
pub enum Backend {
    Command {
        command: String,
        shell: Option<String>,
    },
    Wasm {
        module: PathBuf,
        function: Option<String>,
    },
    Native {
        name: String,
    },
    Agent {
        strategy: String,
        prompt: Option<String>,
        model: Option<String>,
        provider: Option<String>,
        agent: Option<String>,
    },
}
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub backoff: Duration,
}
// ─── Session step ─────────────────────────────────
pub struct SessionStepDef {
    pub base: StepBase,
    /// Prompt/template passed to the step (used by Agent backend)
    pub input: Option<String>,
    /// Which agent config to use (provider, model, system_prompt from fyah.toml)
    pub agent_name: Option<String>,
}
// ─── Agent phase ──────────────────────────────────
pub struct AgentPhaseDef {
    pub base: StepBase,
    /// Max LLM calls in this phase before forced transition
    pub max_iterations: u32,
    /// Which context pipeline config to use
    pub pipeline: Option<String>,
    /// Which tool sets are available in this phase
    pub tools: Option<Vec<String>>,
    /// Per-phase overrides for the LLM call
    pub temperature: Option<f64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Which agent config (system prompt, etc.)
    pub agent_name: Option<String>,
}
// ─── Workflow definition (holds steps or phases) ──
pub struct WorkflowDef {
    pub name: String,
    pub initial: String,
    pub scope: StepLevel,
    pub steps: HashMap<String, StepKind>, // ordered, keyed by id
    pub max_iterations: Option<u32>,
}
pub enum StepLevel {
    Session,
    Agent,
}
pub enum StepKind {
    Session(SessionStepDef),
    Agent(AgentPhaseDef),
}
