// SPDX-License-Identifier: GPL-2.0

//! fyah kernel companion module.
//!
//! This is not an agent. It is a privileged primitive layer that the userspace
//! harness drives through `/dev/fyah`: kernel memory and physical memory
//! access, symbol lookup, indirect calls and kprobes. Every request is a
//! single `ioctl` on one fixed-size [`Request`] structure, so the userspace
//! side only needs to agree on this file's layout.
//!
//! The module deliberately does not try to be safe. Writing kernel memory,
//! writing physical memory and calling arbitrary addresses can crash or
//! corrupt the machine; that is the point of the tool. What the module does
//! enforce is that only `CAP_SYS_ADMIN` can open the device, and that the
//! userspace harness only enables each capability per agent in `fyah.toml`.

use core::sync::atomic::{AtomicU64, Ordering};

use kernel::{
    alloc::{flags::GFP_KERNEL, KBox, KVec},
    bindings,
    error::{code::*, Error, Result},
    fs::File,
    ioctl,
    miscdevice::{MiscDevice, MiscDeviceOptions, MiscDeviceRegistration},
    prelude::*,
    sync::global_lock,
    uaccess::{UserPtr, UserSlice},
};

/// Version of the request ABI. Bumped whenever [`Request`] changes.
const ABI_VERSION: u64 = 1;

/// Largest single memory transfer accepted by one ioctl.
const MAX_IO: u64 = 1 << 20;

/// Largest allocation the module will hand out.
const MAX_ALLOC: u64 = 256 << 10;

/// `ioctl` number: the magic byte `0xf7` and request 0, read/write.
const FYAH_IOC: u32 = ioctl::_IOWR::<Request>(0xf7, 0);

/// `memremap` flags, from `include/linux/io.h`.
const MEMREMAP_WB: u64 = 1 << 0;
const MEMREMAP_WC: u64 = 1 << 2;

// Request operations. Keep in sync with `src/tools/kernel.rs`.
const OP_VERSION: u32 = 0;
const OP_LOOKUP: u32 = 1;
const OP_READ_VIRT: u32 = 2;
const OP_WRITE_VIRT: u32 = 3;
const OP_READ_PHYS: u32 = 4;
const OP_WRITE_PHYS: u32 = 5;
const OP_CALL: u32 = 6;
const OP_ALLOC: u32 = 7;
const OP_FREE: u32 = 8;
const OP_PROBE_ATTACH: u32 = 9;
const OP_PROBE_DETACH: u32 = 10;
const OP_PROBE_STATS: u32 = 11;
const OP_PROBE_LIST: u32 = 12;

/// One request, copied in and out by a single `ioctl`.
///
/// `flags` selects the physical mapping kind. `len` is the byte count, except
/// for [`OP_CALL`] where it is the argument count. `buf` is a userspace
/// pointer to the payload. `value` and `extra` carry results back.
#[repr(C)]
#[derive(Clone, Copy)]
struct Request {
    op: u32,
    flags: u32,
    addr: u64,
    len: u64,
    buf: u64,
    args: [u64; 6],
    name: [u8; 64],
    id: u32,
    status: i32,
    value: u64,
    extra: [u64; 4],
}

/// One probe list entry, as userspace reads it.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProbeEntry {
    id: u32,
    _pad: u32,
    hits: u64,
    last_ip: u64,
    last_ns: u64,
    name: [u8; 64],
}

/// Minimal `struct hlist_node`.
#[repr(C)]
struct HListNode {
    next: *mut HListNode,
    pprev: *mut *mut HListNode,
}

/// Minimal `struct list_head`.
#[repr(C)]
struct ListHead {
    next: *mut ListHead,
    prev: *mut ListHead,
}

/// Minimal x86-64 `struct pt_regs`: only `ip` is read.
#[repr(C)]
struct PtRegs {
    _regs: [u64; 16],
    ip: u64,
}

/// Prefix-compatible `struct kprobe`.
///
/// `ainsn` is kept as an opaque, oversized, 8-byte-aligned blob. The kernel
/// writes into its own layout inside it, and `flags` lands in the padding, so
/// the module never needs the exact internal `arch_specific_insn` shape. Every
/// field before `ainsn` must keep the kernel's offset.
#[repr(C)]
struct Kprobe {
    hlist: HListNode,
    list: ListHead,
    nmissed: usize,
    addr: *mut u8,
    symbol_name: *const c_char,
    offset: u32,
    _pad0: u32,
    pre_handler: Option<unsafe extern "C" fn(*mut Kprobe, *mut PtRegs) -> c_int>,
    post_handler: Option<unsafe extern "C" fn(*mut Kprobe, *mut PtRegs, c_ulong)>,
    opcode: u8,
    _pad1: [u8; 7],
    ainsn: [u64; 16],
    flags: u32,
    _pad2: u32,
}

impl Kprobe {
    fn zeroed() -> Self {
        // SAFETY: every field is valid when zeroed; the kernel re-initialises
        // the probe state it cares about in `register_kprobe`.
        unsafe { core::mem::zeroed() }
    }
}

/// A registered probe and its counters.
///
/// `kp` must stay first: the kernel callback only receives `*mut Kprobe`, and
/// the module recovers the slot by casting that pointer back.
#[repr(C)]
struct Probe {
    kp: Kprobe,
    hits: AtomicU64,
    last_ip: AtomicU64,
    last_ns: AtomicU64,
    id: u32,
    _pad: u32,
    name: [u8; 64],
}

impl Probe {
    fn zeroed() -> Self {
        // SAFETY: atomics and pointers are valid when zeroed.
        unsafe { core::mem::zeroed() }
    }
}

// SAFETY: the probe is only reached through the global mutex, except for the
// atomic counters touched from the kprobe callback.
unsafe impl Send for Probe {}

/// Everything the module owns on behalf of userspace.
struct State {
    allocations: KVec<KVec<u8>>,
    probes: KVec<KBox<Probe>>,
    next_probe_id: u32,
}

global_lock! {
    // SAFETY: initialised in the module initializer, before the device exists.
    unsafe(uninit) static STATE: Mutex<State> = State {
        allocations: KVec::new(),
        probes: KVec::new(),
        next_probe_id: 1,
    };
}

unsafe extern "C" {
    fn register_kprobe(p: *mut Kprobe) -> c_int;
    fn unregister_kprobe(p: *mut Kprobe) -> c_int;
    fn ktime_get_mono_fast_ns() -> u64;
}

/// Called at every probe hit. Runs in atomic context, so only atomics.
unsafe extern "C" fn on_probe(p: *mut Kprobe, regs: *mut PtRegs) -> c_int {
    // SAFETY: `kp` is the first field of `Probe`, and the kernel passes back
    // the same pointer it was registered with.
    let probe = unsafe { &*(p as *const Probe) };
    probe.hits.fetch_add(1, Ordering::Relaxed);
    if !regs.is_null() {
        // SAFETY: the kernel hands a valid `pt_regs` for this hit.
        probe
            .last_ip
            .store(unsafe { (*regs).ip }, Ordering::Relaxed);
    }
    // SAFETY: safe from any context and exported GPL.
    probe
        .last_ns
        .store(unsafe { ktime_get_mono_fast_ns() }, Ordering::Relaxed);
    0
}

struct FyahDevice;

#[vtable]
impl MiscDevice for FyahDevice {
    type Ptr = ();

    fn open(_file: &File, _misc: &MiscDeviceRegistration<Self>) -> Result<Self::Ptr> {
        // SAFETY: `capable` is safe to call from process context.
        if !unsafe { bindings::capable(bindings::CAP_SYS_ADMIN as c_int) } {
            return Err(EPERM);
        }
        Ok(())
    }

    fn ioctl(
        _device: (),
        _file: &File,
        cmd: u32,
        arg: usize,
    ) -> Result<isize> {
        if cmd != FYAH_IOC {
            return Err(EINVAL);
        }

        let size = core::mem::size_of::<Request>();
        let mut raw = [0u8; core::mem::size_of::<Request>()];
        UserSlice::new(UserPtr::from_addr(arg), size)
            .reader()
            .read_slice(&mut raw)?;

        // SAFETY: `raw` is `size` bytes and `Request` is `repr(C)`. The read is
        // unaligned because userspace may pass a loosely aligned struct.
        let mut request = unsafe { core::ptr::read_unaligned(raw.as_ptr().cast::<Request>()) };

        let seq = REQUESTS.fetch_add(1, Ordering::Relaxed);
        pr_info!(
            "req {} op {} addr {:#x} len {}\n",
            seq,
            op_name(request.op),
            request.addr,
            request.len
        );
        match dispatch(&mut request) {
            Ok(()) => request.status = 0,
            Err(error) => request.status = error.to_errno(),
        }
        pr_info!("req {} status {}\n", seq, request.status);

        // SAFETY: `request` is a live `Request` and `size` is its exact size.
        let bytes = unsafe {
            core::slice::from_raw_parts(core::ptr::from_ref(&request).cast::<u8>(), size)
        };
        UserSlice::new(UserPtr::from_addr(arg), size)
            .writer()
            .write_slice(bytes)?;
        Ok(0)
    }
}

/// Sequence number of every request, for matching the two dmesg lines.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

fn op_name(op: u32) -> &'static str {
    match op {
        OP_VERSION => "version",
        OP_LOOKUP => "lookup",
        OP_READ_VIRT => "read",
        OP_WRITE_VIRT => "write",
        OP_READ_PHYS => "read_phys",
        OP_WRITE_PHYS => "write_phys",
        OP_CALL => "call",
        OP_ALLOC => "alloc",
        OP_FREE => "free",
        OP_PROBE_ATTACH => "probe_attach",
        OP_PROBE_DETACH => "probe_detach",
        OP_PROBE_STATS => "probe_stats",
        OP_PROBE_LIST => "probe_list",
        _ => "unknown",
    }
}

fn dispatch(request: &mut Request) -> Result<()> {
    match request.op {
        OP_VERSION => {
            request.value = ABI_VERSION;
            Ok(())
        }
        OP_LOOKUP => lookup(request),
        OP_READ_VIRT => read_virt(request),
        OP_WRITE_VIRT => write_virt(request),
        OP_READ_PHYS => read_phys(request),
        OP_WRITE_PHYS => write_phys(request),
        OP_CALL => call(request),
        OP_ALLOC => alloc(request),
        OP_FREE => free(request),
        OP_PROBE_ATTACH => probe_attach(request),
        OP_PROBE_DETACH => probe_detach(request),
        OP_PROBE_STATS => probe_stats(request),
        OP_PROBE_LIST => probe_list(request),
        _ => Err(EINVAL),
    }
}

fn capped(len: u64, max: u64) -> Result<usize> {
    if len == 0 || len > max {
        return Err(EINVAL);
    }
    Ok(len as usize)
}

fn name_is_terminated(name: &[u8; 64]) -> bool {
    name.contains(&0)
}

/// Resolve a kernel function through a throwaway kprobe.
///
/// `kallsyms_lookup_name` is not exported, and userspace reads
/// `/proc/kallsyms` first. This is the fallback when that file is restricted.
fn lookup(request: &mut Request) -> Result<()> {
    if !name_is_terminated(&request.name) {
        return Err(EINVAL);
    }
    let mut kp = Kprobe::zeroed();
    kp.symbol_name = request.name.as_ptr().cast();
    // SAFETY: `kp` is zeroed and valid for the call. A probe with no handler
    // only resolves and registers the address.
    let ret = unsafe { register_kprobe(&mut kp) };
    if ret < 0 {
        return Err(Error::from_errno(ret));
    }
    request.value = kp.addr as u64;
    // SAFETY: the probe was registered just above.
    unsafe { unregister_kprobe(&mut kp) };
    Ok(())
}

fn read_virt(request: &mut Request) -> Result<()> {
    let len = capped(request.len, MAX_IO)?;
    let mut buf = KVec::new();
    buf.resize(len, 0u8, GFP_KERNEL).map_err(|_| ENOMEM)?;
    // SAFETY: the destination is `len` bytes. `nofault` returns non-zero
    // instead of faulting on a bad kernel address.
    let ret = unsafe {
        bindings::copy_from_kernel_nofault(
            buf.as_mut_ptr().cast(),
            request.addr as *const c_void,
            len,
        )
    };
    if ret != 0 {
        return Err(EFAULT);
    }
    UserSlice::new(UserPtr::from_addr(request.buf as usize), len)
        .writer()
        .write_slice(&buf)?;
    Ok(())
}

fn write_virt(request: &mut Request) -> Result<()> {
    let len = capped(request.len, MAX_IO)?;
    let mut buf = KVec::new();
    buf.resize(len, 0u8, GFP_KERNEL).map_err(|_| ENOMEM)?;
    UserSlice::new(UserPtr::from_addr(request.buf as usize), len)
        .reader()
        .read_slice(&mut buf)?;
    // SAFETY: the caller asked for an unchecked write to a kernel address.
    // There is no exported nofault write, so a bad address faults here.
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), request.addr as *mut u8, len);
    }
    Ok(())
}

fn phys_map(request: &Request, len: usize) -> Result<*mut u8> {
    let flags = if request.flags & 1 != 0 {
        MEMREMAP_WC
    } else {
        MEMREMAP_WB
    };
    // SAFETY: `memremap` validates the range and returns NULL on failure.
    let ptr = unsafe { bindings::memremap(request.addr, len, flags as usize) };
    if ptr.is_null() {
        return Err(EINVAL);
    }
    Ok(ptr.cast())
}

fn read_phys(request: &mut Request) -> Result<()> {
    let len = capped(request.len, MAX_IO)?;
    let ptr = phys_map(request, len)?;
    // SAFETY: `ptr` maps `len` bytes for reads until `memunmap`.
    let data = unsafe { core::slice::from_raw_parts(ptr, len) };
    let result = UserSlice::new(UserPtr::from_addr(request.buf as usize), len)
        .writer()
        .write_slice(data);
    // SAFETY: `ptr` came from `memremap` and is unmapped once.
    unsafe { bindings::memunmap(ptr.cast()) };
    result
}

fn write_phys(request: &mut Request) -> Result<()> {
    let len = capped(request.len, MAX_IO)?;
    let mut buf = KVec::new();
    buf.resize(len, 0u8, GFP_KERNEL).map_err(|_| ENOMEM)?;
    UserSlice::new(UserPtr::from_addr(request.buf as usize), len)
        .reader()
        .read_slice(&mut buf)?;
    let ptr = phys_map(request, len)?;
    // SAFETY: `ptr` maps `len` writable bytes until `memunmap`.
    unsafe {
        core::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, len);
    }
    // SAFETY: `ptr` came from `memremap` and is unmapped once.
    unsafe { bindings::memunmap(ptr.cast()) };
    Ok(())
}

#[allow(clippy::missing_transmute_annotations)]
fn call(request: &mut Request) -> Result<()> {
    let argc = request.len as usize;
    if argc > 6 {
        return Err(EINVAL);
    }
    let address = request.addr as usize;
    let a = request.args;
    // SAFETY: the caller supplied an address and integer arguments. Under IBT
    // a target without ENDBR faults; that is the caller's risk.
    let ret = unsafe {
        match argc {
            0 => core::mem::transmute::<usize, unsafe extern "C" fn() -> u64>(address)(),
            1 => core::mem::transmute::<usize, unsafe extern "C" fn(u64) -> u64>(address)(a[0]),
            2 => core::mem::transmute::<usize, unsafe extern "C" fn(u64, u64) -> u64>(address)(
                a[0], a[1],
            ),
            3 => core::mem::transmute::<usize, unsafe extern "C" fn(u64, u64, u64) -> u64>(
                address,
            )(a[0], a[1], a[2]),
            4 => core::mem::transmute::<usize, unsafe extern "C" fn(u64, u64, u64, u64) -> u64>(
                address,
            )(a[0], a[1], a[2], a[3]),
            5 => core::mem::transmute::<usize, unsafe extern "C" fn(u64, u64, u64, u64, u64) -> u64>(
                address,
            )(a[0], a[1], a[2], a[3], a[4]),
            _ => core::mem::transmute::<usize, unsafe extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64>(
                address,
            )(a[0], a[1], a[2], a[3], a[4], a[5]),
        }
    };
    request.value = ret;
    Ok(())
}

fn alloc(request: &mut Request) -> Result<()> {
    let len = capped(request.len, MAX_ALLOC)?;
    let mut buf = KVec::new();
    buf.resize(len, 0u8, GFP_KERNEL).map_err(|_| ENOMEM)?;
    let address = buf.as_mut_ptr() as u64;
    STATE
        .lock()
        .allocations
        .push(buf, GFP_KERNEL)
        .map_err(|_| ENOMEM)?;
    request.value = address;
    Ok(())
}

fn free(request: &mut Request) -> Result<()> {
    let address = request.addr;
    let mut state = STATE.lock();
    let index = state
        .allocations
        .iter()
        .position(|buf| buf.as_ptr() as u64 == address)
        .ok_or(ENOENT)?;
    state
        .allocations
        .remove(index)
        .map_err(|_| EINVAL)?;
    Ok(())
}

fn probe_attach(request: &mut Request) -> Result<()> {
    if !name_is_terminated(&request.name) {
        return Err(EINVAL);
    }
    let mut probe = KBox::new(Probe::zeroed(), GFP_KERNEL).map_err(|_| ENOMEM)?;
    probe.name = request.name;
    let name_ptr = probe.name.as_ptr();
    probe.kp.symbol_name = name_ptr.cast();
    probe.kp.pre_handler = Some(on_probe);
    // SAFETY: the probe lives in a `KBox` and is not moved while registered.
    let ret = unsafe { register_kprobe(&mut probe.kp) };
    if ret < 0 {
        return Err(Error::from_errno(ret));
    }
    let mut state = STATE.lock();
    let id = state.next_probe_id;
    state.next_probe_id += 1;
    probe.id = id;
    state.probes.push(probe, GFP_KERNEL).map_err(|_| ENOMEM)?;
    request.value = id as u64;
    Ok(())
}

fn probe_detach(request: &mut Request) -> Result<()> {
    let id = request.id;
    let mut state = STATE.lock();
    let index = state
        .probes
        .iter()
        .position(|probe| probe.id == id)
        .ok_or(ENOENT)?;
    let mut probe = state.probes.remove(index).map_err(|_| EINVAL)?;
    drop(state);
    // SAFETY: the probe was registered by `probe_attach`.
    unsafe { unregister_kprobe(&mut probe.kp) };
    Ok(())
}

fn probe_stats(request: &mut Request) -> Result<()> {
    let id = request.id;
    let state = STATE.lock();
    let probe = state
        .probes
        .iter()
        .find(|probe| probe.id == id)
        .ok_or(ENOENT)?;
    request.value = probe.hits.load(Ordering::Relaxed);
    request.extra[0] = probe.last_ip.load(Ordering::Relaxed);
    request.extra[1] = probe.last_ns.load(Ordering::Relaxed);
    Ok(())
}

fn probe_list(request: &mut Request) -> Result<()> {
    let count = capped(request.len, 4096)?;
    let state = STATE.lock();
    let mut entries = KVec::new();
    for probe in state.probes.iter().take(count) {
        entries
            .push(
                ProbeEntry {
                    id: probe.id,
                    _pad: 0,
                    hits: probe.hits.load(Ordering::Relaxed),
                    last_ip: probe.last_ip.load(Ordering::Relaxed),
                    last_ns: probe.last_ns.load(Ordering::Relaxed),
                    name: probe.name,
                },
                GFP_KERNEL,
            )
            .map_err(|_| ENOMEM)?;
    }
    let bytes = entries.len() * core::mem::size_of::<ProbeEntry>();
    // SAFETY: `entries` is a live contiguous array of `ProbeEntry`.
    let data =
        unsafe { core::slice::from_raw_parts(entries.as_ptr().cast::<u8>(), bytes) };
    UserSlice::new(UserPtr::from_addr(request.buf as usize), bytes)
        .writer()
        .write_slice(data)?;
    request.value = entries.len() as u64;
    Ok(())
}

struct FyahModule {
    _device: Pin<KBox<MiscDeviceRegistration<FyahDevice>>>,
}

impl kernel::Module for FyahModule {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        // SAFETY: the module initializer runs exactly once per load, and the
        // statics are fresh module memory on every load.
        unsafe { STATE.init() };

        let options = MiscDeviceOptions { name: c"fyah" };
        let device =
            KBox::pin_init(MiscDeviceRegistration::<FyahDevice>::register(options), GFP_KERNEL)?;

        pr_info!("loaded, /dev/fyah ready\n");
        Ok(Self { _device: device })
    }
}

impl Drop for FyahModule {
    fn drop(&mut self) {
        let mut state = STATE.lock();
        while let Some(mut probe) = state.probes.pop() {
            // SAFETY: every probe in the list was registered by
            // `probe_attach` and has not been detached.
            unsafe { unregister_kprobe(&mut probe.kp) };
        }
        // The static is never dropped, so buffers handed out by `alloc` and
        // never freed are released here.
        while state.allocations.pop().is_some() {}
        pr_info!("unloaded\n");
    }
}

module! {
    type: FyahModule,
    name: "fyah_km",
    authors: ["fyah"],
    description: "Privileged primitive layer for the fyah harness",
    license: "GPL",
}
