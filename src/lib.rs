use log::{error, info, Level};

use core::ffi::c_void;
use libc::{c_ulong, iovec, pid_t, sysconf, _SC_IOV_MAX};

use memflow::prelude::v1::*;

mod qemu_args;
use qemu_args::{is_qemu, qemu_arg_opt};

#[cfg(feature = "qmp")]
#[macro_use]
extern crate scan_fmt;

mod mem_map;
use mem_map::qemu_mem_mappings;

#[derive(Clone, Copy)]
#[repr(transparent)]
struct IoSendVec(iovec);

unsafe impl Send for IoSendVec {}

#[derive(Clone)]
pub struct QemuProcfs {
    pub pid: pid_t,
    pub mem_map: MemoryMap<(Address, usize)>,
    temp_iov: Box<[IoSendVec]>,
}

impl QemuProcfs {
    pub fn new() -> Result<Self> {
        let prcs = procfs::process::all_processes().map_err(|_| {
            Error(ErrorOrigin::Connector, ErrorKind::UnableToReadDir)
                .log_error("unable to list procfs processes")
        })?;
        let prc = prcs.iter().find(|p| is_qemu(p)).ok_or_else(|| {
            Error(ErrorOrigin::Connector, ErrorKind::NotFound).log_error("qemu process not found")
        })?;
        info!("qemu process found with pid {:?}", prc.stat.pid);

        Self::with_process(prc)
    }

    pub fn with_guest_name(name: &str) -> Result<Self> {
        let prcs = procfs::process::all_processes().map_err(|_| {
            Error(ErrorOrigin::Connector, ErrorKind::UnableToReadDir)
                .log_error("unable to list procfs processes")
        })?;
        let (prc, _) = prcs
            .iter()
            .filter(|p| is_qemu(p))
            .filter_map(|p| {
                if let Ok(c) = p.cmdline() {
                    Some((p, c))
                } else {
                    None
                }
            })
            .find(|(_, c)| qemu_arg_opt(c, "-name", "guest").unwrap_or_default() == name)
            .ok_or_else(|| {
                Error(ErrorOrigin::Connector, ErrorKind::NotFound)
                    .log_error("qemu process not found")
            })?;
        info!(
            "qemu process with name {} found with pid {:?}",
            name, prc.stat.pid
        );

        Self::with_process(prc)
    }

    fn with_process(prc: &procfs::process::Process) -> Result<Self> {
        // find biggest memory mapping in qemu process
        let mut maps = prc
            .maps()
            .map_err(|_| Error(ErrorOrigin::Connector, ErrorKind::UnableToReadDir).log_error("Unable to retrieve Qemu memory maps. Did u run memflow with the correct access rights (SYS_PTRACE or root)?"))?;
        maps.sort_by(|b, a| {
            (a.address.1 - a.address.0)
                .partial_cmp(&(b.address.1 - b.address.0))
                .unwrap()
        });
        let qemu_map = maps.get(0).ok_or_else(|| {
            Error(ErrorOrigin::Connector, ErrorKind::UnableToReadDir)
                .log_error("Qemu memory map could not be read")
        })?;
        info!("qemu memory map found {:?}", qemu_map);

        let cmdline = prc.cmdline().map_err(|_| {
            Error(ErrorOrigin::Connector, ErrorKind::UnableToReadFile)
                .log_error("unable to parse qemu arguments")
        })?;

        Self::with_cmdline_and_mem(prc, &cmdline, qemu_map)
    }

    fn with_cmdline_and_mem(
        prc: &procfs::process::Process,
        cmdline: &[String],
        qemu_map: &procfs::process::MemoryMap,
    ) -> Result<Self> {
        let mem_map = qemu_mem_mappings(&cmdline, qemu_map)?;
        info!("qemu machine mem_map: {:?}", mem_map);

        let iov_max = unsafe { sysconf(_SC_IOV_MAX) } as usize;

        Ok(Self {
            pid: prc.stat.pid,
            mem_map,
            temp_iov: vec![
                IoSendVec {
                    0: iovec {
                        iov_base: std::ptr::null_mut::<c_void>(),
                        iov_len: 0
                    }
                };
                iov_max * 2
            ]
            .into_boxed_slice(),
        })
    }

    fn fill_iovec(addr: &Address, data: &[u8], liov: &mut IoSendVec, riov: &mut IoSendVec) {
        let iov_len = data.len();

        liov.0 = iovec {
            iov_base: data.as_ptr() as *mut c_void,
            iov_len,
        };

        riov.0 = iovec {
            iov_base: addr.as_u64() as *mut c_void,
            iov_len,
        };
    }

    fn vm_error() -> Error {
        match unsafe { *libc::__errno_location() } {
            libc::EFAULT => Error(ErrorOrigin::Connector, ErrorKind::UnableToReadMemory).log_error("process_vm_readv failed: EFAULT (remote memory address is invalid)"),
            libc::ENOMEM => Error(ErrorOrigin::Connector, ErrorKind::UnableToReadMemory).log_error("process_vm_readv failed: ENOMEM (unable to allocate memory for internal copies)"),
            libc::EPERM => Error(ErrorOrigin::Connector, ErrorKind::UnableToReadMemory).log_error("process_vm_readv failed: EPERM (insifficient permissions to access the target address space)"),
            libc::ESRCH => Error(ErrorOrigin::Connector, ErrorKind::UnableToReadMemory).log_error("process_vm_readv failed: ESRCH (process not found)"),
            libc::EINVAL => Error(ErrorOrigin::Connector, ErrorKind::UnableToReadMemory).log_error("process_vm_readv failed: EINVAL (invalid value)"),
            _ => Error(ErrorOrigin::Connector, ErrorKind::UnableToReadMemory).log_error("process_vm_readv failed: unknown error")
        }
    }
}

impl PhysicalMemory for QemuProcfs {
    fn phys_read_raw_list(&mut self, data: &mut [PhysicalReadData]) -> Result<()> {
        let mem_map = &self.mem_map;
        let temp_iov = &mut self.temp_iov;

        let mut void = FnExtend::void();
        let mut iter = mem_map.map_iter(
            data.iter_mut()
                .map(|PhysicalReadData(addr, buf)| (*addr, &mut **buf)),
            &mut void,
        );

        let max_iov = temp_iov.len() / 2;
        let (iov_local, iov_remote) = temp_iov.split_at_mut(max_iov);

        let mut elem = iter.next();

        let mut iov_iter = iov_local.iter_mut().zip(iov_remote.iter_mut()).enumerate();
        let mut iov_next = iov_iter.next();

        while let Some(((addr, _), out)) = elem {
            let (cnt, (liov, riov)) = iov_next.unwrap();

            Self::fill_iovec(&addr, out, liov, riov);

            iov_next = iov_iter.next();
            elem = iter.next();

            if elem.is_none() || iov_next.is_none() {
                if unsafe {
                    libc::process_vm_readv(
                        self.pid,
                        iov_local.as_ptr().cast(),
                        (cnt + 1) as c_ulong,
                        iov_remote.as_ptr().cast(),
                        (cnt + 1) as c_ulong,
                        0,
                    )
                } == -1
                {
                    return Err(Self::vm_error());
                }

                iov_iter = iov_local.iter_mut().zip(iov_remote.iter_mut()).enumerate();
                iov_next = iov_iter.next();
            }
        }

        Ok(())
    }

    fn phys_write_raw_list(&mut self, data: &[PhysicalWriteData]) -> Result<()> {
        let mem_map = &self.mem_map;
        let temp_iov = &mut self.temp_iov;

        let mut void = FnExtend::void();
        let mut iter = mem_map.map_iter(data.iter().copied().map(<_>::from), &mut void);
        //let mut iter = mem_map.map_iter(data.iter(), &mut FnExtend::new(|_|{}));

        let max_iov = temp_iov.len() / 2;
        let (iov_local, iov_remote) = temp_iov.split_at_mut(max_iov);

        let mut elem = iter.next();

        let mut iov_iter = iov_local.iter_mut().zip(iov_remote.iter_mut()).enumerate();
        let mut iov_next = iov_iter.next();

        while let Some(((addr, _), out)) = elem {
            let (cnt, (liov, riov)) = iov_next.unwrap();

            Self::fill_iovec(&addr, out, liov, riov);

            iov_next = iov_iter.next();
            elem = iter.next();

            if elem.is_none() || iov_next.is_none() {
                if unsafe {
                    libc::process_vm_writev(
                        self.pid,
                        iov_local.as_ptr().cast(),
                        (cnt + 1) as c_ulong,
                        iov_remote.as_ptr().cast(),
                        (cnt + 1) as c_ulong,
                        0,
                    )
                } == -1
                {
                    return Err(Self::vm_error());
                }

                iov_iter = iov_local.iter_mut().zip(iov_remote.iter_mut()).enumerate();
                iov_next = iov_iter.next();
            }
        }

        Ok(())
    }

    fn metadata(&self) -> PhysicalMemoryMetadata {
        PhysicalMemoryMetadata {
            size: self
                .mem_map
                .as_ref()
                .iter()
                .last()
                .map(|map| map.base().as_usize() + map.output().1)
                .unwrap(),
            readonly: false,
        }
    }

    fn set_mem_map(&mut self, mem_map: MemoryMap<(Address, usize)>) {
        self.mem_map.merge(mem_map)
    }
}

impl<'a> ConnectorCpuStateInner<'a> for QemuProcfs {
    type CpuStateType = &'a mut QemuProcfs;
    type IntoCpuStateType = QemuProcfs;

    fn cpu_state(&'a mut self) -> memflow::error::Result<Self::CpuStateType> {
        Ok(self)
    }

    fn into_cpu_state(self) -> memflow::error::Result<Self::IntoCpuStateType> {
        Ok(self)
    }
}

impl CpuState for QemuProcfs {
    // TODO:
}

impl CpuState for &mut QemuProcfs {
    // TODO:
}

fn validator() -> ArgsValidator {
    ArgsValidator::new()
        .arg(ArgDescriptor::new("default").description(
            "the name of the qemu virtual machine (specified with -name when starting qemu)",
        ))
        .arg(ArgDescriptor::new("name").description(
            "the name of the qemu virtual machine (specified with -name when starting qemu)",
        ))
}

/// Creates a new Qemu Procfs instance.
pub fn create_connector(args: &Args, log_level: Level) -> Result<QemuProcfs> {
    simple_logger::SimpleLogger::new()
        .with_level(log_level.to_level_filter())
        .init()
        .ok();

    let validator = validator();
    match validator.validate(&args) {
        Ok(_) => {
            if let Some(name) = args.get("name").or_else(|| args.get_default()) {
                QemuProcfs::with_guest_name(name)
            } else {
                QemuProcfs::new()
            }
        }
        Err(err) => {
            error!(
                "unable to validate provided arguments, valid arguments are:\n{}",
                validator
            );
            Err(err)
        }
    }
}

/// Creates a new Qemu Procfs Connector instance.
#[connector(name = "qemu_procfs", help_fn = "help", target_list_fn = "target_list")]
pub fn create_connector_instance(args: &Args, log_level: Level) -> Result<ConnectorInstance> {
    let connector = create_connector(args, log_level)?;
    let instance = ConnectorInstance::builder(connector)
        .enable_cpu_state()
        .build();
    Ok(instance)
}

/// Retrieve the help text for the Qemu Procfs Connector.
pub fn help() -> String {
    let validator = validator();
    format!(
        "\
The `qemu_procfs` connector implements a memflow plugin interface
for Qemu on top of the Process Filesystem on Linux.

This connector requires access to the qemu process via the linux procfs.
This means any process which loads this connector requires
to have at least ptrace permissions set.

Available arguments are:
{}",
        validator.to_string()
    )
}

/// Retrieve a list of all currently available Qemu targets.
pub fn target_list() -> Result<Vec<TargetInfo>> {
    Ok(procfs::process::all_processes()
        .map_err(|_| {
            Error(ErrorOrigin::Connector, ErrorKind::UnableToReadDir)
                .log_error("unable to list procfs processes")
        })?
        .iter()
        .filter(|p| is_qemu(p))
        .filter_map(|p| p.cmdline().ok())
        .filter_map(|c| qemu_arg_opt(&c, "-name", "guest"))
        .map(|n| TargetInfo {
            name: ReprCString::from(n),
        })
        .collect::<Vec<_>>())
}
