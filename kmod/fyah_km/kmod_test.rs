// Standalone smoke test for `/dev/fyah`, run inside the NixOS VM check.
//
// It deliberately does not share code with the userspace harness: it speaks
// the module's ABI directly, so it also proves the two sides agree on the
// request layout. Built with plain `rustc`, no crates.

use std::{fs::OpenOptions, os::fd::AsRawFd as _};

const ABI_VERSION: u64 = 1;

const OP_VERSION: u32 = 0;
const OP_LOOKUP: u32 = 1;
const OP_READ_VIRT: u32 = 2;
const OP_WRITE_VIRT: u32 = 3;
const OP_READ_PHYS: u32 = 4;
const OP_CALL: u32 = 6;
const OP_ALLOC: u32 = 7;
const OP_FREE: u32 = 8;
const OP_PROBE_ATTACH: u32 = 9;
const OP_PROBE_DETACH: u32 = 10;
const OP_PROBE_STATS: u32 = 11;

const FYAH_IOC: u64 = iowr::<Request>(0xf7, 0);

#[repr(C)]
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

impl Request {
    const fn new(op: u32) -> Self {
        Self {
            op,
            flags: 0,
            addr: 0,
            len: 0,
            buf: 0,
            args: [0; 6],
            name: [0; 64],
            id: 0,
            status: 0,
            value: 0,
            extra: [0; 4],
        }
    }
}

const fn iowr<T>(ty: u32, nr: u32) -> u64 {
    let size = core::mem::size_of::<T>();
    let size = if size > 0x3fff { 0x3fff } else { size };
    let dir = 2u32 | 1u32;
    ((dir << 30) | ((ty & 0xff) << 8) | (nr & 0xff) | ((size as u32) << 16)) as u64
}

unsafe extern "C" {
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn getpid() -> i32;
}

fn issue(fd: i32, request: &mut Request) {
    // SAFETY: the fd is open and `request` is a live `Request`.
    let result = unsafe { ioctl(fd, FYAH_IOC, request as *mut Request) };
    assert!(
        result >= 0,
        "ioctl failed: {}",
        std::io::Error::last_os_error()
    );
}

fn expect_ok(request: &Request, what: &str) {
    assert_eq!(
        request.status,
        0,
        "{what} failed: {}",
        std::io::Error::from_raw_os_error(request.status.saturating_neg())
    );
}

fn expect_err(request: &Request, what: &str) {
    assert!(
        request.status < 0,
        "{what} unexpectedly succeeded: value {:#x}",
        request.value
    );
}

fn name_field(name: &str) -> [u8; 64] {
    let bytes = name.as_bytes();
    assert!(bytes.len() < 64, "symbol name too long");
    let mut field = [0u8; 64];
    field[..bytes.len()].copy_from_slice(bytes);
    field
}

fn main() {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/fyah")
        .expect("open /dev/fyah");
    let fd = file.as_raw_fd();

    let mut request = Request::new(OP_VERSION);
    issue(fd, &mut request);
    expect_ok(&request, "version");
    assert_eq!(request.value, ABI_VERSION, "ABI mismatch");

    // alloc / write / read / free roundtrip.
    let mut request = Request::new(OP_ALLOC);
    request.len = 16;
    issue(fd, &mut request);
    expect_ok(&request, "alloc");
    let buffer = request.value;
    assert_ne!(buffer, 0, "alloc returned NULL");

    let payload = b"hello kernel";
    let mut request = Request::new(OP_WRITE_VIRT);
    request.addr = buffer;
    request.len = payload.len() as u64;
    request.buf = payload.as_ptr() as u64;
    issue(fd, &mut request);
    expect_ok(&request, "write");

    let mut readback = [0u8; 16];
    let mut request = Request::new(OP_READ_VIRT);
    request.addr = buffer;
    request.len = payload.len() as u64;
    request.buf = readback.as_mut_ptr() as u64;
    issue(fd, &mut request);
    expect_ok(&request, "read");
    assert_eq!(&readback[..payload.len()], payload, "readback mismatch");

    let mut request = Request::new(OP_FREE);
    request.addr = buffer;
    issue(fd, &mut request);
    expect_ok(&request, "free");

    // A bad virtual address must fail with EFAULT, not oops.
    let mut request = Request::new(OP_READ_VIRT);
    request.addr = 0xdead_0000_0000_0000;
    request.len = 8;
    request.buf = readback.as_mut_ptr() as u64;
    issue(fd, &mut request);
    expect_err(&request, "bad read");

    // Physical memory: low RAM is always present.
    let mut request = Request::new(OP_READ_PHYS);
    request.addr = 0x1000;
    request.len = 16;
    request.buf = readback.as_mut_ptr() as u64;
    issue(fd, &mut request);
    expect_ok(&request, "read_phys");

    // Function lookup, then call it.
    let mut request = Request::new(OP_LOOKUP);
    request.name = name_field("ktime_get");
    issue(fd, &mut request);
    expect_ok(&request, "lookup");
    let ktime_get = request.value;
    assert_ne!(ktime_get, 0, "ktime_get not found");

    let mut request = Request::new(OP_CALL);
    request.addr = ktime_get;
    request.len = 0;
    issue(fd, &mut request);
    expect_ok(&request, "call");
    assert!(request.value > 0, "ktime_get returned zero");

    // A kprobe on a syscall entry counts the calls we make.
    let mut request = Request::new(OP_PROBE_ATTACH);
    request.name = name_field("__x64_sys_getpid");
    issue(fd, &mut request);
    expect_ok(&request, "probe_attach");
    let probe = u32::try_from(request.value).expect("probe id fits u32");
    assert_ne!(probe, 0, "probe id is zero");

    // SAFETY: plain syscall wrapper.
    let _ = unsafe { getpid() };

    let mut request = Request::new(OP_PROBE_STATS);
    request.id = probe;
    issue(fd, &mut request);
    expect_ok(&request, "probe_stats");
    assert!(request.value >= 1, "probe did not record the syscall");

    let mut request = Request::new(OP_PROBE_DETACH);
    request.id = probe;
    issue(fd, &mut request);
    expect_ok(&request, "probe_detach");

    println!("kmod-test: all checks passed");
}
