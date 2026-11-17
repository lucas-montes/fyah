//! Runs `[executables.*]` entries of type `wasm`: WASI preview 1 modules that
//! read the call's JSON arguments on stdin and answer on stdout.
//!
//! One [`WasmRuntime`] serves the whole workspace. It holds the engine, the
//! WASI host functions and the compiled modules, so agents share one
//! compile of each module. Everything a call touches (its store, its
//! sandbox, its pipes, its limits) is built for that call and dropped with
//! it.

use std::{
    collections::HashMap,
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, PoisonError},
    time::{Duration, SystemTime},
};

use tracing::{debug, warn};
use wasmtime::{
    Cache, Config, Engine, EngineWeak, InstancePre, Linker, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};
use wasmtime_wasi::{
    FsPerms, I32Exit, WasiCtxBuilder,
    p1::{self, WasiP1Ctx},
    p2::pipe::{MemoryInputPipe, MemoryOutputPipe},
};

use crate::{
    config::WasmSpec,
    tools::{errors::ExecError, output::ToolOutput},
};

/// How often a running guest gives its thread back, so a timeout or a
/// dropped call can stop it even while it never calls the host.
const EPOCH_TICK: Duration = Duration::from_millis(10);
/// What a module may print, per stream. Writing past it traps the guest.
const MAX_OUTPUT_BYTES: usize = 1 << 20;

pub struct WasmRuntime {
    engine: Engine,
    linker: Linker<Guest>,
    /// Only held to look up or insert, never across an await: compiling
    /// happens outside it.
    modules: Mutex<HashMap<PathBuf, Cached>>,
}

impl std::fmt::Debug for WasmRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmRuntime").finish_non_exhaustive()
    }
}

/// What one call's store holds.
struct Guest {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

/// What a cached module was compiled from. Any difference means recompile.
#[derive(PartialEq, Eq)]
struct Stamp {
    /// Where the path really points. Nix store files all carry the same
    /// modification time, but a rebuild moves the link to a new store path.
    target: PathBuf,
    modified: SystemTime,
    len: u64,
}

struct Cached {
    stamp: Stamp,
    module: InstancePre<Guest>,
}

impl WasmRuntime {
    /// Starts the epoch ticker, which stops by itself once the runtime is
    /// gone.
    pub fn new() -> wasmtime::Result<Self> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        // Compiled code is kept on disk across sessions; a large module such
        // as an interpreter otherwise compiles again at every start.
        match Cache::from_file(None) {
            Ok(cache) => {
                config.cache(Some(cache));
            }
            Err(error) => warn!(%error, "wasm compile cache unavailable; compiling every session"),
        }
        let engine = Engine::new(&config)?;
        let mut linker = Linker::new(&engine);
        p1::add_to_linker_async(&mut linker, |guest: &mut Guest| &mut guest.wasi)?;
        // An OS thread, not a task: spinning guests can hold every runtime
        // worker, and a task would then never run to let them yield.
        let weak = engine.weak();
        std::thread::Builder::new()
            .name("wasm-epoch".to_owned())
            .spawn(move || tick(&weak))?;
        Ok(Self {
            engine,
            linker,
            modules: Mutex::default(),
        })
    }

    /// Runs `spec` with `stdin`. `Err` means the module never got to answer:
    /// it failed to load, trapped, or ran out of time. Exiting non-zero is an
    /// answer. The `timeout` covers running, not compiling: a compile cut
    /// short would be thrown away and started over by the next call.
    pub fn run(
        &self,
        spec: &WasmSpec,
        stdin: String,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send {
        async move {
            let module = self.load(spec.path()).await?;

            let stdout = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
            let stderr = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
            let wasi = sandbox(spec, stdin, &stdout, &stderr)?;
            let limits = StoreLimitsBuilder::new()
                .memory_size(spec.memory_bytes())
                .instances(1)
                .build();
            let mut store = Store::new(&self.engine, Guest { wasi, limits });
            store.limiter(|guest| &mut guest.limits);
            // Also the initial deadline: without one the guest traps at once.
            store.epoch_deadline_async_yield_and_update(1);

            let call = call(&module, &mut store, spec.function());
            let outcome = match timeout {
                Some(limit) => tokio::time::timeout(limit, call)
                    .await
                    .map_err(|_| ExecError::Timeout)?,
                None => call.await,
            };
            let status = match outcome {
                Ok(()) => 0,
                Err(error) => match error.downcast_ref::<I32Exit>() {
                    Some(I32Exit(code)) => *code,
                    None => return Err(ExecError::Wasm(format!("{error:#}"))),
                },
            };

            // Releases the guest's ends of the pipes, so taking their buffers
            // copies nothing.
            drop(store);
            Ok(ToolOutput::from_bytes(
                status,
                into_bytes(stdout),
                into_bytes(stderr),
            ))
        }
    }

    /// The module at `path`, compiled from its file as it is now. Two calls
    /// that both miss compile twice; the last one in is kept.
    fn load(
        &self,
        path: &Path,
    ) -> impl Future<Output = Result<InstancePre<Guest>, ExecError>> + Send {
        async move {
            let io = |source| ExecError::Io {
                path: path.display().to_string(),
                source,
            };
            let target = tokio::fs::canonicalize(path).await.map_err(io)?;
            // Of the target itself, so both describe the same file.
            let metadata = tokio::fs::metadata(&target).await.map_err(io)?;
            let stamp = Stamp {
                target,
                modified: metadata.modified().map_err(io)?,
                len: metadata.len(),
            };
            if let Some(cached) = self.cache().get(path)
                && cached.stamp == stamp
            {
                // A reference-count bump.
                return Ok(cached.module.clone());
            }

            debug!(path = %path.display(), "compiling wasm module");
            // Compiling is CPU-heavy: kept off the async workers.
            let engine = self.engine.clone();
            let owned = path.to_path_buf();
            let (module, owned) = tokio::task::spawn_blocking(move || {
                let module = Module::from_file(&engine, &owned);
                (module, owned)
            })
            .await
            .map_err(|join| match join.try_into_panic() {
                Ok(payload) => ExecError::panicked(&*payload),
                Err(_) => ExecError::ExecutorGone,
            })?;
            let module = module.map_err(|error| ExecError::Wasm(format!("{error:#}")))?;
            let module = self
                .linker
                .instantiate_pre(&module)
                .map_err(|error| ExecError::Wasm(format!("{error:#}")))?;

            self.cache().insert(
                owned,
                Cached {
                    stamp,
                    module: module.clone(),
                },
            );
            Ok(module)
        }
    }

    fn cache(&self) -> MutexGuard<'_, HashMap<PathBuf, Cached>> {
        self.modules.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Instantiates and calls `function`. A reactor (anything but `_start`)
/// gets its `_initialize` first, when it has one.
fn call(
    module: &InstancePre<Guest>,
    store: &mut Store<Guest>,
    function: &str,
) -> impl Future<Output = wasmtime::Result<()>> + Send {
    async move {
        let instance = module.instantiate_async(&mut *store).await?;
        if function != "_start"
            && let Some(initialize) = instance.get_func(&mut *store, "_initialize")
        {
            initialize
                .typed::<(), ()>(&*store)?
                .call_async(&mut *store, ())
                .await?;
        }
        instance
            .get_typed_func::<(), ()>(&mut *store, function)?
            .call_async(&mut *store, ())
            .await
    }
}

/// Denies everything, then grants exactly what `spec` lists: its arguments,
/// its variables and its directories. Stdio goes to memory.
fn sandbox(
    spec: &WasmSpec,
    stdin: String,
    stdout: &MemoryOutputPipe,
    stderr: &MemoryOutputPipe,
) -> Result<WasiP1Ctx, ExecError> {
    let mut wasi = WasiCtxBuilder::new();
    wasi.stdin(MemoryInputPipe::new(stdin))
        .stdout(stdout.clone())
        .stderr(stderr.clone())
        .arg(program_name(spec.path()))
        .args(spec.args());
    for (key, value) in spec.env() {
        wasi.env(key, value);
    }
    for name in spec.inherit_env() {
        if let Ok(value) = std::env::var(name) {
            wasi.env(name, value);
        }
    }
    for dir in spec.dirs() {
        let perms = if dir.write() {
            FsPerms::ReadWrite
        } else {
            FsPerms::ReadOnly
        };
        wasi.preopened_dir(dir.host(), dir.guest(), perms)
            .map_err(|error| ExecError::Wasm(format!("{}: {error:#}", dir.host().display())))?;
    }
    Ok(wasi.build_p1())
}

/// The module's `argv[0]`: its file name.
fn program_name(path: &Path) -> &str {
    path.file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("module")
}

/// The pipe's buffer, moved out. `contents()` would copy it twice: once to
/// snapshot it under the lock, once more into the `Vec`.
fn into_bytes(pipe: MemoryOutputPipe) -> Vec<u8> {
    pipe.try_into_inner().map_or_default(Vec::from)
}

/// Moves the engine's epoch forward until the engine is dropped.
fn tick(engine: &EngineWeak) {
    loop {
        std::thread::sleep(EPOCH_TICK);
        let Some(engine) = engine.upgrade() else {
            return;
        };
        engine.increment_epoch();
    }
}
