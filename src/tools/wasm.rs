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

use futures::{
    FutureExt, TryFutureExt,
    future::{self, Either},
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
    tools::{
        deadline::{blocking, within},
        errors::ExecError,
        output::ToolOutput,
    },
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
        self.load(spec.path())
            .and_then(move |module| self.execute(module, spec, stdin, timeout))
    }

    /// Builds the call's store, then runs it. Everything up to the call is
    /// synchronous, so a sandbox that cannot be built answers at once.
    fn execute(
        &self,
        module: InstancePre<Guest>,
        spec: &WasmSpec,
        stdin: String,
        timeout: Option<Duration>,
    ) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send {
        let stdout = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stderr = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let wasi = match sandbox(spec, stdin, &stdout, &stderr) {
            Ok(wasi) => wasi,
            Err(error) => return Either::Right(future::ready(Err(error))),
        };
        let limits = StoreLimitsBuilder::new()
            .memory_size(spec.memory_bytes())
            .instances(1)
            .build();
        let mut store = Store::new(&self.engine, Guest { wasi, limits });
        store.limiter(|guest| &mut guest.limits);
        // Also the initial deadline: without one the guest traps at once.
        store.epoch_deadline_async_yield_and_update(1);

        Either::Left(
            within(timeout, call(module, store, spec.function()).map(exit_status))
                .map(Result::flatten)
                // The store is gone with the call, releasing the guest's ends
                // of the pipes, so taking their buffers copies nothing.
                .map_ok(move |status| {
                    ToolOutput::from_bytes(status, into_bytes(stdout), into_bytes(stderr))
                }),
        )
    }

    /// The module at `path`, compiled from its file as it is now. Two calls
    /// that both miss compile twice; the last one in is kept.
    fn load(
        &self,
        path: &Path,
    ) -> impl Future<Output = Result<InstancePre<Guest>, ExecError>> + Send {
        // One trip to the blocking pool for both syscalls, and one copy of
        // the path, which a miss reuses as the cache key.
        let owned = path.to_path_buf();
        blocking(move || stamp(owned))
            .map(Result::flatten)
            .and_then(move |(owned, stamp)| match self.cached(&owned, &stamp) {
                Some(module) => Either::Left(future::ready(Ok(module))),
                None => Either::Right(self.compile(owned, stamp)),
            })
    }

    /// A reference-count bump, when the module is still what was compiled.
    fn cached(&self, path: &Path, stamp: &Stamp) -> Option<InstancePre<Guest>> {
        self.cache()
            .get(path)
            .filter(|cached| cached.stamp == *stamp)
            .map(|cached| cached.module.clone())
    }

    /// Compiling is CPU-heavy: kept off the async workers. Linking and
    /// caching are not, and need the runtime, so they happen back here.
    fn compile(
        &self,
        path: PathBuf,
        stamp: Stamp,
    ) -> impl Future<Output = Result<InstancePre<Guest>, ExecError>> + Send {
        debug!(path = %path.display(), "compiling wasm module");
        let engine = self.engine.clone();
        blocking(move || {
            let module = Module::from_file(&engine, &path);
            (module, path)
        })
        .map(move |compiled| {
            let (module, path) = compiled?;
            let module = self
                .linker
                .instantiate_pre(&module?)?;
            self.cache().insert(
                path,
                Cached {
                    stamp,
                    module: module.clone(),
                },
            );
            Ok(module)
        })
    }

    fn cache(&self) -> MutexGuard<'_, HashMap<PathBuf, Cached>> {
        self.modules.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The file `path` really is now. Blocking; the path comes back to be
/// reused, in the error too.
fn stamp(path: PathBuf) -> Result<(PathBuf, Stamp), ExecError> {
    match read_stamp(&path) {
        Ok(stamp) => Ok((path, stamp)),
        Err(source) => Err(ExecError::ModuleFile { path, source }),
    }
}

/// Where `path` points, when it changed and how big it is.
fn read_stamp(path: &Path) -> std::io::Result<Stamp> {
    let target = std::fs::canonicalize(path)?;
    // Of the target itself, so both describe the same file.
    let metadata = std::fs::metadata(&target)?;
    Ok(Stamp {
        target,
        modified: metadata.modified()?,
        len: metadata.len(),
    })
}

/// Instantiates and calls `function`. A reactor (anything but `_start`)
/// gets its `_initialize` first, when it has one. An async block: it owns
/// the module and the store while every step borrows them, and dropping
/// the store as it finishes frees the pipes.
fn call(
    module: InstancePre<Guest>,
    mut store: Store<Guest>,
    function: &str,
) -> impl Future<Output = wasmtime::Result<()>> + Send {
    async move {
        let instance = module.instantiate_async(&mut store).await?;
        if function != "_start"
            && let Some(initialize) = instance.get_func(&mut store, "_initialize")
        {
            initialize
                .typed::<(), ()>(&store)?
                .call_async(&mut store, ())
                .await?;
        }
        instance
            .get_typed_func::<(), ()>(&mut store, function)?
            .call_async(&mut store, ())
            .await
    }
}

/// Exiting through `proc_exit` is an answer, with its code; any other
/// error means the module never got to answer.
fn exit_status(outcome: wasmtime::Result<()>) -> Result<i32, ExecError> {
    match outcome {
        Ok(()) => Ok(0),
        Err(error) => match error.downcast_ref::<I32Exit>() {
            Some(I32Exit(code)) => Ok(*code),
            None => Err(error.into()),
        },
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
            // An error outlives the borrow of the spec: only the failing path is copied.
            .map_err(|source| ExecError::PreopenDir {
                dir: dir.host().to_path_buf(),
                source,
            })?;
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
