//! The `kernel` builtin: a client for the `fyah_km` module behind
//! `/dev/fyah`.
//!
//! Every call is one `ioctl` carrying a fixed-size [`Request`], so the module
//! and this file must agree on that layout. The module is not an agent: it is
//! a primitive layer, and this tool is where the per-agent gates are applied.

use std::{borrow::Cow, fmt::Write as _, fs::OpenOptions, os::fd::AsRawFd as _, path::Path};

use futures::{
    FutureExt as _,
    future::{self, Either},
};
use serde::{Deserialize, Deserializer, de};

use crate::{
    config::AgentKernel,
    tools::{deadline::blocking, errors::ExecError, output::ToolOutput},
    ui::OutboundChannel,
};

/// Version of the request ABI, mirrored from `kmod/fyah_km/fyah_km.rs`.
const ABI_VERSION: u64 = 1;

/// Largest transfer the module accepts in one request.
const MAX_IO: u64 = 1 << 20;

/// Largest allocation the module hands out.
const MAX_ALLOC: u64 = 256 << 10;

/// `ioctl` number: magic `0xf7`, request 0, read/write, size of [`Request`].
const FYAH_IOC: libc::c_ulong = iowr::<Request>(0xf7, 0);

// Operations, mirrored from the module.
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

/// Build an `_IOWR` ioctl number. Linux `_IOC` layout for x86-64.
#[allow(clippy::cast_possible_truncation)]
const fn iowr<T>(ty: u32, nr: u32) -> libc::c_ulong {
    let size = core::mem::size_of::<T>();
    let size = if size > 0x3fff { 0x3fff } else { size };
    let dir = 2u32 | 1u32;
    ((dir << 30) | ((ty & 0xff) << 8) | (nr & 0xff) | ((size as u32) << 16)) as libc::c_ulong
}

/// One request, matching the module's `Request` byte for byte.
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

/// One probe list entry, matching the module's `ProbeEntry`.
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

/// What a call asks the module to do, parsed and owned.
enum Op {
    Version,
    Lookup {
        name: String,
    },
    Read {
        addr: u64,
        len: usize,
    },
    Write {
        addr: u64,
        bytes: Vec<u8>,
    },
    ReadPhys {
        addr: u64,
        len: usize,
        mmio: bool,
    },
    WritePhys {
        addr: u64,
        bytes: Vec<u8>,
        mmio: bool,
    },
    Call {
        target: Target,
        args: Vec<u64>,
    },
    Alloc {
        len: usize,
    },
    Free {
        addr: u64,
    },
    ProbeAttach {
        name: String,
    },
    ProbeDetach {
        id: u32,
    },
    ProbeStats {
        id: u32,
    },
    ProbeList,
    Patch {
        target: Target,
        bytes: Vec<u8>,
    },
}

/// A call target: an address the model gave, or a symbol to resolve.
enum Target {
    Addr(u64),
    Name(String),
}

/// The JSON the model sends.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args<'a> {
    #[serde(borrow)]
    op: Cow<'a, str>,
    #[serde(default)]
    addr: Option<Addr>,
    #[serde(default, borrow)]
    name: Option<Cow<'a, str>>,
    #[serde(default)]
    len: Option<u64>,
    #[serde(default, borrow)]
    hex: Option<Cow<'a, str>>,
    #[serde(default)]
    args: Option<Vec<u64>>,
    #[serde(default)]
    id: Option<u32>,
    #[serde(default)]
    mmio: bool,
}

/// An address given as a number or as a `0x`-prefixed string.
struct Addr(u64);

impl<'de> Deserialize<'de> for Addr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Addr;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an address as a number or a 0x-prefixed string")
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Addr(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                u64::try_from(value).map(Addr).map_err(E::custom)
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                parse_addr(value).map(Addr).map_err(E::custom)
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Parse an address as decimal or as `0x` hex.
fn parse_addr(text: &str) -> Result<u64, String> {
    let text = text.trim();
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| format!("not an address: {text}"))
}

/// Parse the model's arguments into one owned operation.
fn parse_args(arguments: &str) -> Result<Op, ExecError> {
    let args: Args<'_> = serde_json::from_str(arguments).map_err(ExecError::BadArguments)?;
    match args.op.as_ref() {
        "version" => Ok(Op::Version),
        "lookup" => Ok(Op::Lookup {
            name: required_name(args.name)?,
        }),
        "read" => Ok(Op::Read {
            addr: required_addr(args.addr)?,
            len: length(args.len, 64)?,
        }),
        "write" => Ok(Op::Write {
            addr: required_addr(args.addr)?,
            bytes: decode_hex(&required_hex(args.hex)?)?,
        }),
        "read_phys" => Ok(Op::ReadPhys {
            addr: required_addr(args.addr)?,
            len: length(args.len, 64)?,
            mmio: args.mmio,
        }),
        "write_phys" => Ok(Op::WritePhys {
            addr: required_addr(args.addr)?,
            bytes: decode_hex(&required_hex(args.hex)?)?,
            mmio: args.mmio,
        }),
        "call" => {
            let target = target(args.addr, args.name)?;
            let call_args = args.args.unwrap_or_default();
            if call_args.len() > 6 {
                return Err(ExecError::KernelArguments(
                    "call takes at most six arguments",
                ));
            }
            Ok(Op::Call {
                target,
                args: call_args,
            })
        }
        "alloc" => {
            let len = length(args.len, 4096)?;
            if u64::try_from(len).unwrap_or(u64::MAX) > MAX_ALLOC {
                return Err(ExecError::KernelArguments("alloc is capped at 256 KiB"));
            }
            Ok(Op::Alloc { len })
        }
        "free" => Ok(Op::Free {
            addr: required_addr(args.addr)?,
        }),
        "probe_attach" => Ok(Op::ProbeAttach {
            name: required_name(args.name)?,
        }),
        "probe_detach" => Ok(Op::ProbeDetach {
            id: required_id(args.id)?,
        }),
        "probe_stats" => Ok(Op::ProbeStats {
            id: required_id(args.id)?,
        }),
        "probe_list" => Ok(Op::ProbeList),
        "patch" => Ok(Op::Patch {
            target: target(args.addr, args.name)?,
            bytes: decode_hex(&required_hex(args.hex)?)?,
        }),
        other => Err(ExecError::KernelUnknownOp(other.to_owned())),
    }
}

fn required_addr(addr: Option<Addr>) -> Result<u64, ExecError> {
    addr.map(|addr| addr.0)
        .ok_or(ExecError::KernelArguments("addr is required"))
}

fn required_name(name: Option<Cow<'_, str>>) -> Result<String, ExecError> {
    name.map(|name| name.into_owned())
        .ok_or(ExecError::KernelArguments("name is required"))
}

fn required_hex(hex: Option<Cow<'_, str>>) -> Result<String, ExecError> {
    match hex {
        Some(hex) => Ok(hex.into_owned()),
        None => Err(ExecError::KernelArguments("hex is required")),
    }
}

fn required_id(id: Option<u32>) -> Result<u32, ExecError> {
    id.ok_or(ExecError::KernelArguments("id is required"))
}

fn target(addr: Option<Addr>, name: Option<Cow<'_, str>>) -> Result<Target, ExecError> {
    match (addr, name) {
        (Some(addr), _) => Ok(Target::Addr(addr.0)),
        (None, Some(name)) => Ok(Target::Name(name.into_owned())),
        (None, None) => Err(ExecError::KernelArguments("addr or name is required")),
    }
}

fn length(len: Option<u64>, default: u64) -> Result<usize, ExecError> {
    let len = len.unwrap_or(default);
    if len == 0 {
        return Err(ExecError::KernelArguments("len must be positive"));
    }
    usize::try_from(len).map_err(|_| ExecError::KernelArguments("len is too large"))
}

/// Decode a hex payload, ignoring whitespace and underscores.
fn decode_hex(text: &str) -> Result<Vec<u8>, ExecError> {
    let mut nibbles = Vec::new();
    for character in text.chars() {
        if character.is_whitespace() || character == '_' {
            continue;
        }
        let digit = character.to_digit(16).ok_or(ExecError::KernelArguments(
            "hex payload has a non-hex character",
        ))?;
        nibbles.push(digit);
    }
    if nibbles.len() % 2 != 0 {
        return Err(ExecError::KernelArguments(
            "hex payload has an odd number of digits",
        ));
    }
    Ok(nibbles
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::try_from((pair[0] << 4) | pair[1]).unwrap_or_default())
        .collect())
}

/// Gate the call, ask the user when it changes kernel state, then run it on
/// the blocking pool: `ioctl` sleeps. A refused or disabled op never opens
/// the device.
pub fn run(
    arguments: &str,
    device: &str,
    max_bytes: u64,
    access: AgentKernel,
    output: &OutboundChannel,
) -> impl Future<Output = Result<ToolOutput, ExecError>> + Send + use<> {
    let checked = parse_args(arguments).and_then(|op| gate(&op, access).map(|()| op));
    match checked {
        Ok(op) => {
            let approval = match question(&op) {
                Some(question) if access.confirm() => Either::Left(output.confirm(question)),
                _ => Either::Right(future::ready(true)),
            };
            let device = device.to_owned();
            Either::Left(approval.then(move |approved| {
                if approved {
                    Either::Left(
                        blocking(move || execute(&op, &device, max_bytes)).map(Result::flatten),
                    )
                } else {
                    Either::Right(future::ready(Err(ExecError::KernelRefused)))
                }
            }))
        }
        Err(error) => Either::Right(future::ready(Err(error))),
    }
}

/// What to ask the user before an op that changes kernel state; `None` for
/// ops that only observe it.
fn question(op: &Op) -> Option<String> {
    match op {
        Op::Version
        | Op::Lookup { .. }
        | Op::Read { .. }
        | Op::ReadPhys { .. }
        | Op::Alloc { .. }
        | Op::Free { .. }
        | Op::ProbeDetach { .. }
        | Op::ProbeStats { .. }
        | Op::ProbeList => None,
        Op::Write { addr, bytes } => Some(format!(
            "kernel: write {} bytes at {addr:#018x}?",
            bytes.len()
        )),
        Op::WritePhys { addr, bytes, .. } => Some(format!(
            "kernel: write {} bytes at physical {addr:#018x}?",
            bytes.len()
        )),
        Op::Patch { target, bytes } => Some(format!(
            "kernel: patch {} bytes of kernel text at {target}?",
            bytes.len()
        )),
        Op::Call { target, args } => Some(format!(
            "kernel: call {target}({})?",
            format_args_list(args)
        )),
        Op::ProbeAttach { name } => Some(format!("kernel: attach a kprobe to {name}?")),
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Addr(addr) => write!(f, "{addr:#018x}"),
            Self::Name(name) => f.write_str(name),
        }
    }
}

/// A call in progress: the open device and the output cap.
struct Session {
    device: Device,
    max_bytes: u64,
}

fn execute(op: &Op, device: &str, max_bytes: u64) -> Result<ToolOutput, ExecError> {
    let mut session = Session {
        device: Device::open(Path::new(device))?,
        max_bytes,
    };
    session.version()?;
    session.run(op)
}

/// Map an operation to the capability it needs.
fn gate(op: &Op, access: AgentKernel) -> Result<(), ExecError> {
    let capability = match op {
        Op::Version | Op::Lookup { .. } | Op::Read { .. } => return Ok(()),
        Op::Write { .. } | Op::Patch { .. } => ("write", access.write()),
        Op::ReadPhys { .. } | Op::WritePhys { .. } => ("phys", access.phys()),
        Op::Call { .. } | Op::Alloc { .. } | Op::Free { .. } => ("call", access.call()),
        Op::ProbeAttach { .. } | Op::ProbeDetach { .. } | Op::ProbeStats { .. } | Op::ProbeList => {
            ("probe", access.probe())
        }
    };
    if capability.1 {
        Ok(())
    } else {
        Err(ExecError::KernelDenied {
            capability: capability.0,
        })
    }
}

impl Session {
    fn run(&mut self, op: &Op) -> Result<ToolOutput, ExecError> {
        match op {
            Op::Version => Ok(output(format!("fyah_km ABI version {ABI_VERSION}\n"))),
            Op::Lookup { name } => {
                let addr = self.lookup(name)?;
                Ok(output(format!("{name} = {addr:#018x}\n")))
            }
            Op::Read { addr, len } => {
                let len = self.cap(*len);
                let bytes = self.read_virt(*addr, len)?;
                Ok(output(hex_dump(&bytes, *addr)))
            }
            Op::Write { addr, bytes } => {
                self.write_virt(*addr, bytes)?;
                Ok(output(format!(
                    "wrote {} bytes at {addr:#018x}\n",
                    bytes.len()
                )))
            }
            Op::ReadPhys { addr, len, mmio } => {
                let len = self.cap(*len);
                let bytes = self.read_phys(*addr, len, *mmio)?;
                Ok(output(hex_dump(&bytes, *addr)))
            }
            Op::WritePhys { addr, bytes, mmio } => {
                self.write_phys(*addr, bytes, *mmio)?;
                Ok(output(format!(
                    "wrote {} bytes at physical {addr:#018x}\n",
                    bytes.len()
                )))
            }
            Op::Call { target, args } => {
                let addr = self.resolve(target)?;
                let ret = self.call(addr, args)?;
                Ok(output(format!(
                    "call {addr:#018x}({}) = {ret:#018x} ({ret})\n",
                    format_args_list(args)
                )))
            }
            Op::Alloc { len } => {
                let addr = self.alloc(*len)?;
                Ok(output(format!("allocated {len} bytes at {addr:#018x}\n")))
            }
            Op::Free { addr } => {
                self.free(*addr)?;
                Ok(output(format!("freed {addr:#018x}\n")))
            }
            Op::ProbeAttach { name } => {
                let id = self.probe_attach(name)?;
                Ok(output(format!("probe {id} attached to {name}\n")))
            }
            Op::ProbeDetach { id } => {
                self.probe_detach(*id)?;
                Ok(output(format!("probe {id} detached\n")))
            }
            Op::ProbeStats { id } => {
                let (hits, last_ip, last_ns) = self.probe_stats(*id)?;
                Ok(output(format!(
                    "probe {id}: hits {hits}, last ip {last_ip:#018x}, last ns {last_ns}\n"
                )))
            }
            Op::ProbeList => {
                let entries = self.probe_list()?;
                let mut text = String::new();
                for entry in &entries {
                    let name = symbol_name(&entry.name);
                    let _ = writeln!(
                        text,
                        "{} {} hits {} last ip {:#018x}",
                        entry.id, name, entry.hits, entry.last_ip
                    );
                }
                Ok(output(text))
            }
            Op::Patch { target, bytes } => {
                let addr = self.resolve(target)?;
                self.patch(addr, bytes)?;
                Ok(output(format!(
                    "patched {} bytes at {addr:#018x} through text_poke\n",
                    bytes.len()
                )))
            }
        }
    }

    fn cap(&self, len: usize) -> usize {
        let limit = usize::try_from(self.max_bytes).unwrap_or(usize::MAX);
        len.min(limit)
            .min(usize::try_from(MAX_IO).unwrap_or(usize::MAX))
    }

    fn version(&mut self) -> Result<(), ExecError> {
        let mut request = Request::new(OP_VERSION);
        self.device.request(&mut request, "version")?;
        if request.value != ABI_VERSION {
            return Err(ExecError::KernelArguments(
                "fyah_km ABI mismatch: rebuild the module for this fyah",
            ));
        }
        Ok(())
    }

    fn lookup(&mut self, name: &str) -> Result<u64, ExecError> {
        if let Some(addr) = kallsyms(name) {
            return Ok(addr);
        }
        let mut request = Request::new(OP_LOOKUP);
        request.name = name_field(name)?;
        self.device.request(&mut request, "lookup")?;
        if request.value == 0 {
            return Err(ExecError::KernelArguments("symbol not found"));
        }
        Ok(request.value)
    }

    fn resolve(&mut self, target: &Target) -> Result<u64, ExecError> {
        match target {
            Target::Addr(addr) => Ok(*addr),
            Target::Name(name) => self.lookup(name),
        }
    }

    fn read_virt(&mut self, addr: u64, len: usize) -> Result<Vec<u8>, ExecError> {
        let mut bytes = vec![0u8; len];
        let mut request = Request::new(OP_READ_VIRT);
        request.addr = addr;
        request.len = len as u64;
        request.buf = bytes.as_mut_ptr() as u64;
        self.device.request(&mut request, "read")?;
        Ok(bytes)
    }

    fn write_virt(&mut self, addr: u64, bytes: &[u8]) -> Result<(), ExecError> {
        let mut request = Request::new(OP_WRITE_VIRT);
        request.addr = addr;
        request.len = bytes.len() as u64;
        request.buf = bytes.as_ptr() as u64;
        self.device.request(&mut request, "write")
    }

    fn read_phys(&mut self, addr: u64, len: usize, mmio: bool) -> Result<Vec<u8>, ExecError> {
        let mut bytes = vec![0u8; len];
        let mut request = Request::new(OP_READ_PHYS);
        request.addr = addr;
        request.len = len as u64;
        request.buf = bytes.as_mut_ptr() as u64;
        request.flags = u32::from(mmio);
        self.device.request(&mut request, "read_phys")?;
        Ok(bytes)
    }

    fn write_phys(&mut self, addr: u64, bytes: &[u8], mmio: bool) -> Result<(), ExecError> {
        let mut request = Request::new(OP_WRITE_PHYS);
        request.addr = addr;
        request.len = bytes.len() as u64;
        request.buf = bytes.as_ptr() as u64;
        request.flags = u32::from(mmio);
        self.device.request(&mut request, "write_phys")
    }

    fn call(&mut self, addr: u64, args: &[u64]) -> Result<u64, ExecError> {
        let mut request = Request::new(OP_CALL);
        request.addr = addr;
        request.len = args.len() as u64;
        request.args[..args.len()].copy_from_slice(args);
        self.device.request(&mut request, "call")?;
        Ok(request.value)
    }

    fn alloc(&mut self, len: usize) -> Result<u64, ExecError> {
        let mut request = Request::new(OP_ALLOC);
        request.len = len as u64;
        self.device.request(&mut request, "alloc")?;
        Ok(request.value)
    }

    fn free(&mut self, addr: u64) -> Result<(), ExecError> {
        let mut request = Request::new(OP_FREE);
        request.addr = addr;
        self.device.request(&mut request, "free")
    }

    fn probe_attach(&mut self, name: &str) -> Result<u32, ExecError> {
        let mut request = Request::new(OP_PROBE_ATTACH);
        request.name = name_field(name)?;
        self.device.request(&mut request, "probe_attach")?;
        u32::try_from(request.value)
            .map_err(|_| ExecError::KernelArguments("probe id out of range"))
    }

    fn probe_detach(&mut self, id: u32) -> Result<(), ExecError> {
        let mut request = Request::new(OP_PROBE_DETACH);
        request.id = id;
        self.device.request(&mut request, "probe_detach")
    }

    fn probe_stats(&mut self, id: u32) -> Result<(u64, u64, u64), ExecError> {
        let mut request = Request::new(OP_PROBE_STATS);
        request.id = id;
        self.device.request(&mut request, "probe_stats")?;
        Ok((request.value, request.extra[0], request.extra[1]))
    }

    fn probe_list(&mut self) -> Result<Vec<ProbeEntry>, ExecError> {
        let mut entries = vec![
            ProbeEntry {
                id: 0,
                _pad: 0,
                hits: 0,
                last_ip: 0,
                last_ns: 0,
                name: [0; 64],
            };
            256
        ];
        let mut request = Request::new(OP_PROBE_LIST);
        request.len = entries.len() as u64;
        request.buf = entries.as_mut_ptr() as u64;
        self.device.request(&mut request, "probe_list")?;
        entries.truncate(usize::try_from(request.value).unwrap_or(0));
        Ok(entries)
    }

    /// Write `bytes` into a module-owned buffer and hand it to `text_poke`.
    fn patch(&mut self, addr: u64, bytes: &[u8]) -> Result<(), ExecError> {
        let buffer = self.alloc(bytes.len())?;
        let written = self.write_virt(buffer, bytes);
        let result = written.and_then(|()| {
            let text_poke = self.lookup("text_poke")?;
            self.call(text_poke, &[addr, buffer, bytes.len() as u64])
                .map(|_| ())
        });
        let freed = self.free(buffer);
        result?;
        freed
    }
}

/// The device, and one `ioctl` per request.
struct Device {
    file: std::fs::File,
}

impl Device {
    fn open(path: &Path) -> Result<Self, ExecError> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map(|file| Self { file })
            .map_err(|source| ExecError::KernelDevice {
                path: path.to_path_buf(),
                source,
            })
    }

    fn request(&mut self, request: &mut Request, op: &'static str) -> Result<(), ExecError> {
        // SAFETY: the fd is open and `request` is a live `Request`; the ioctl
        // copies it in and out.
        let result =
            unsafe { libc::ioctl(self.file.as_raw_fd(), FYAH_IOC, std::ptr::from_mut(request)) };
        if result < 0 {
            return Err(ExecError::KernelIo {
                op,
                source: std::io::Error::last_os_error(),
            });
        }
        if request.status != 0 {
            return Err(ExecError::KernelStatus {
                op,
                errno: request.status,
            });
        }
        Ok(())
    }
}

/// Read a symbol from `/proc/kallsyms`. Root sees real addresses even with
/// `kptr_restrict = 1`; zeroed addresses mean the file is not usable here.
fn kallsyms(name: &str) -> Option<u64> {
    let table = std::fs::read_to_string("/proc/kallsyms").ok()?;
    table.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let address = fields.next()?;
        let _kind = fields.next()?;
        let symbol = fields.next()?;
        if symbol != name {
            return None;
        }
        let address = u64::from_str_radix(address, 16).ok()?;
        (address != 0).then_some(address)
    })
}

fn name_field(name: &str) -> Result<[u8; 64], ExecError> {
    let bytes = name.as_bytes();
    if bytes.len() >= 64 || bytes.contains(&0) {
        return Err(ExecError::KernelArguments(
            "symbol name is too long or contains a NUL",
        ));
    }
    let mut field = [0u8; 64];
    field[..bytes.len()].copy_from_slice(bytes);
    Ok(field)
}

fn symbol_name(field: &[u8; 64]) -> &str {
    let end = field.iter().position(|byte| *byte == 0).unwrap_or(64);
    std::str::from_utf8(&field[..end]).unwrap_or("<invalid>")
}

fn format_args_list(args: &[u64]) -> String {
    let mut text = String::new();
    for (index, arg) in args.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        let _ = write!(text, "{arg:#x}");
    }
    text
}

fn hex_dump(bytes: &[u8], base: u64) -> String {
    let mut text = String::new();
    for (index, chunk) in bytes.chunks(16).enumerate() {
        let offset = u64::try_from(index).unwrap_or(u64::MAX).saturating_mul(16);
        let _ = write!(text, "{:016x}: ", base.wrapping_add(offset));
        for byte in chunk {
            let _ = write!(text, "{byte:02x} ");
        }
        for _ in chunk.len()..16 {
            text.push_str("   ");
        }
        text.push(' ');
        for byte in chunk {
            text.push(if byte.is_ascii_graphic() || *byte == b' ' {
                char::from(*byte)
            } else {
                '.'
            });
        }
        text.push('\n');
    }
    text
}

fn output(text: String) -> ToolOutput {
    ToolOutput::from_bytes(0, text.into_bytes(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(toml: &str) -> AgentKernel {
        toml::from_str(toml).expect("valid kernel settings")
    }

    #[test]
    fn addresses_accept_numbers_and_hex() {
        assert_eq!(parse_addr("0x10").expect("hex"), 16);
        assert_eq!(parse_addr("42").expect("decimal"), 42);
        assert!(parse_addr("nope").is_err());
    }

    #[test]
    fn hex_ignores_separators() {
        assert_eq!(
            decode_hex("48 8b_05").expect("valid hex"),
            vec![0x48, 0x8b, 0x05]
        );
        assert!(decode_hex("abc").is_err());
        assert!(decode_hex("zz").is_err());
    }

    #[test]
    fn parse_rejects_unknown_ops() {
        assert!(matches!(
            parse_args(r#"{"op":"nope"}"#),
            Err(ExecError::KernelUnknownOp(_))
        ));
    }

    #[test]
    fn write_is_denied_by_default() {
        let op = parse_args(r#"{"op":"write","addr":"0x1000","hex":"90"}"#).expect("parsed");
        assert!(matches!(
            gate(&op, AgentKernel::default()),
            Err(ExecError::KernelDenied {
                capability: "write"
            })
        ));
    }

    #[test]
    fn write_is_allowed_when_configured() {
        let op = parse_args(r#"{"op":"write","addr":4096,"hex":"90"}"#).expect("parsed");
        assert!(gate(&op, access("write = true")).is_ok());
    }

    #[test]
    fn only_state_changes_ask_the_user() {
        let read = parse_args(r#"{"op":"read","addr":0}"#).expect("parsed");
        assert!(question(&read).is_none());
        let call = parse_args(r#"{"op":"call","name":"ktime_get","args":[1]}"#).expect("parsed");
        assert_eq!(
            question(&call).as_deref(),
            Some("kernel: call ktime_get(0x1)?")
        );
    }

    #[test]
    fn confirm_is_on_by_default() {
        assert!(AgentKernel::default().confirm());
        assert!(access("write = true").confirm());
        assert!(!access("confirm = false").confirm());
    }

    /// No terminal to answer means no: the call fails before the device,
    /// which here does not even exist.
    #[tokio::test]
    async fn unanswered_write_is_refused_without_opening_the_device() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let output = OutboundChannel::from(tx);
        let result = run(
            r#"{"op":"write","addr":4096,"hex":"90"}"#,
            "/nonexistent/fyah",
            64,
            access("write = true"),
            &output,
        )
        .await;
        assert!(matches!(result, Err(ExecError::KernelRefused)));
    }

    #[tokio::test]
    async fn confirm_false_goes_straight_to_the_device() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let output = OutboundChannel::from(tx);
        let result = run(
            r#"{"op":"write","addr":4096,"hex":"90"}"#,
            "/nonexistent/fyah",
            64,
            access("write = true\nconfirm = false"),
            &output,
        )
        .await;
        assert!(matches!(result, Err(ExecError::KernelDevice { .. })));
    }

    #[test]
    fn read_is_always_allowed() {
        let op = parse_args(r#"{"op":"read","addr":0,"len":1}"#).expect("parsed");
        assert!(gate(&op, AgentKernel::default()).is_ok());
    }

    #[test]
    fn call_rejects_more_than_six_arguments() {
        assert!(matches!(
            parse_args(r#"{"op":"call","addr":0,"args":[1,2,3,4,5,6,7]}"#),
            Err(ExecError::KernelArguments(_))
        ));
    }

    #[test]
    fn long_symbol_names_are_rejected() {
        let name = "a".repeat(64);
        assert!(name_field(&name).is_err());
    }

    #[test]
    fn hex_dump_shows_addresses_and_bytes() {
        let dump = hex_dump(&[0x48, 0x90], 0x1000);
        assert!(dump.starts_with("0000000000001000: 48 90 "));
        assert!(dump.contains("H."));
    }
}
