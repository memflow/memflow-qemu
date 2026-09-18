use log::{debug, error, info};

use memflow::cglue;
use memflow::connector::cpu_state::*;
use memflow::mem::memory_view::RemapView;
use memflow::mem::phys_mem::*;
use memflow::os::root::Os;
use memflow::prelude::v1::*;

mod qemu_args;
use qemu_args::{is_qemu, qemu_arg_opt};

#[cfg(target_os = "linux")]
mod proc_maps;

#[cfg(all(target_os = "linux", feature = "qmp"))]
#[macro_use]
extern crate scan_fmt;

mod mem_map;
use mem_map::{qemu_mem_mappings, try_qmp_mtree};

cglue_impl_group!(QemuProcfs<P: MemoryView + Clone>, ConnectorInstance, {
    ConnectorCpuState
});
cglue_impl_group!(QemuProcfs<P: MemoryView + Clone>, IntoCpuState);

#[derive(Clone)]
pub struct QemuProcfs<P: MemoryView> {
    view: RemapView<P>,
}

impl<P: MemoryView + Process> QemuProcfs<P> {
    pub fn new<O: Os<IntoProcessType = P>>(
        mut os: O,
        map_override: Option<CTup2<Address, umem>>,
        qemu_binary_name: Option<String>,
    ) -> Result<Self> {
        let mut proc = None;

        let callback = &mut |info: ProcessInfo| {
            let matches = qemu_binary_name
                .as_ref()
                .map(|nm| &*info.name == nm)
                .unwrap_or_else(|| is_qemu(&info));
            if proc.is_none() && matches {
                proc = Some(info);
            }

            proc.is_none()
        };

        os.process_info_list_callback(callback.into())?;

        debug!("reading qemu process: {:#?}", proc);

        Self::with_process(
            os,
            proc.ok_or_else(|| {
                Error(ErrorOrigin::Connector, ErrorKind::TargetNotFound)
                    .log_error("No QEMU process could be found. Is QEMU running?")
            })?,
            map_override,
        )
    }

    pub fn with_guest_name<O: Os<IntoProcessType = P>>(
        mut os: O,
        name: &str,
        map_override: Option<CTup2<Address, umem>>,
        qemu_binary_name: Option<String>,
    ) -> Result<Self> {
        let mut proc = None;

        let callback = &mut |info: ProcessInfo| {
            let matches = qemu_binary_name
                .as_ref()
                .map(|nm| &*info.name == nm)
                .unwrap_or_else(|| is_qemu(&info));
            if proc.is_none()
                && matches
                && qemu_arg_opt(info.command_line.split_whitespace(), "-name", "guest").as_deref()
                    == Some(name)
            {
                proc = Some(info);
            }

            proc.is_none()
        };

        os.process_info_list_callback(callback.into())?;

        debug!("reading qemu process: {:#?}", proc);

        Self::with_process(
            os,
            proc.ok_or_else(||
                Error(ErrorOrigin::Connector, ErrorKind::TargetNotFound)
                    .log_error("A QEMU process for the specified guest name could not be found. Is the QEMU process running?")
            )?,
            map_override,
        )
    }

    pub fn with_pid<O: Os<IntoProcessType = P>>(
        mut os: O,
        pid: Pid,
        map_override: Option<CTup2<Address, umem>>,
    ) -> Result<Self> {
        let proc = os.process_info_by_pid(pid)?;

        Self::with_process(os, proc, map_override)
    }

    fn with_process<O: Os<IntoProcessType = P>>(
        os: O,
        info: ProcessInfo,
        map_override: Option<CTup2<Address, umem>>,
    ) -> Result<Self> {
        info!(
            "qemu process with name {} found with pid {:?}",
            info.name, info.pid
        );

        let cmdline: String = info.command_line.to_string();
        let pid = info.pid;

        let mut prc = os.into_process_by_info(info)?;

        // fetch the QMP mtree once up front, because it gives us both the guest -> host remap table and the exact pc.ram byte count.
        // knowing the exact size is what lets us pin the host mapping to pc.ram even when a vfio-pci BAR mirror is larger.
        let mtree = try_qmp_mtree(cmdline.split_whitespace()).ok();
        let expected_pc_ram_size = mtree.as_ref().map(|m| mem_map::pc_ram_size(m));

        if let Some(size) = expected_pc_ram_size {
            info!(
                "qmp reports pc.ram size = {:#x} ({} MiB)",
                size,
                size / (1024 * 1024)
            );
        }

        let qemu_map = if let Some(m) = map_override {
            m
        } else {
            find_pc_ram_mapping(&mut prc, pid, expected_pc_ram_size)?
        };

        info!("qemu memory map found {:?}", qemu_map);

        Self::with_cmdline_and_mem(prc, &cmdline, qemu_map, mtree)
    }

    fn with_cmdline_and_mem(
        prc: P,
        cmdline: &str,
        qemu_map: CTup2<Address, umem>,
        prepared: Option<Vec<mem_map::Mapping>>,
    ) -> Result<Self> {
        let mem_map = qemu_mem_mappings(cmdline, &qemu_map, prepared)?;
        info!("qemu machine mem_map: {:?}", mem_map);

        Ok(Self {
            view: prc.into_remap_view(mem_map),
        })
    }
}

fn find_pc_ram_mapping<P: Process>(
    prc: &mut P,
    pid: Pid,
    expected_size: Option<umem>,
) -> Result<CTup2<Address, umem>> {
    #[cfg(target_os = "linux")]
    if let Some(m) = pick_pc_ram_via_procfs(pid, expected_size) {
        return Ok(m);
    }

    let mut biggest_map: Option<CTup2<Address, umem>> = None;
    let callback = &mut |range: MemoryRange| {
        if biggest_map
            .map(|CTup2(_, oldsize)| oldsize < range.1)
            .unwrap_or(true)
        {
            biggest_map = Some(CTup2(range.0, range.1));
        }
        true
    };
    prc.mapped_mem_range(
        smem::mb(-1),
        Address::NULL,
        Address::INVALID,
        callback.into(),
    );

    biggest_map.ok_or_else(|| {
        Error(ErrorOrigin::Connector, ErrorKind::NotFound).log_error(
            "Unable to find the QEMU guest memory map. This usually indicates \
             insufficient permissions to acquire the QEMU memory maps. Are you \
             running with appropriate access rights?",
        )
    })
}

#[cfg(target_os = "linux")]
fn pick_pc_ram_via_procfs(pid: Pid, expected_size: Option<umem>) -> Option<CTup2<Address, umem>> {
    let maps = match proc_maps::read(pid) {
        Ok(m) => m,
        Err(e) => {
            debug!("could not read /proc/{}/maps: {}", pid, e);
            return None;
        }
    };

    let candidates: Vec<_> = maps
        .into_iter()
        .filter(proc_maps::ProcMap::is_ram_candidate)
        .collect();

    if candidates.is_empty() {
        debug!("no ram-candidate mappings in /proc/{}/maps", pid);
        return None;
    }

    if let Some(want) = expected_size {
        if let Some(m) = candidates.iter().find(|m| m.size() == want) {
            info!(
                "pc.ram: matched procfs mapping {:x}-{:x} (size {:#x}) to QMP pc.ram size",
                m.start.to_umem(),
                m.end.to_umem(),
                m.size()
            );
            return Some(CTup2(m.start, m.size()));
        }
        error!(
            "QMP reports pc.ram = {:#x} bytes but no /proc/{}/maps entry matches; \
             falling back to biggest ram-candidate mapping. This may indicate an \
             unusual QEMU memory backend or a memflow-qemu bug - please attach \
             the /proc/<pid>/maps content when filing an issue.",
            want, pid
        );
    }

    let m = candidates.into_iter().max_by_key(|m| m.size())?;
    info!(
        "pc.ram: picked largest ram-candidate procfs mapping {:x}-{:x} (size {:#x})",
        m.start.to_umem(),
        m.end.to_umem(),
        m.size()
    );
    Some(CTup2(m.start, m.size()))
}

impl<P: MemoryView> PhysicalMemory for QemuProcfs<P> {
    fn phys_read_raw_iter(
        &mut self,
        MemOps { inp, out, out_fail }: PhysicalReadMemOps,
    ) -> Result<()> {
        let inp = inp.map(|CTup3(addr, meta_addr, data)| CTup3(addr.into(), meta_addr, data));
        MemOps::with_raw(inp, out, out_fail, |data| self.view.read_raw_iter(data))
    }

    fn phys_write_raw_iter(
        &mut self,
        MemOps { inp, out, out_fail }: PhysicalWriteMemOps,
    ) -> Result<()> {
        let inp = inp.map(|CTup3(addr, meta_addr, data)| CTup3(addr.into(), meta_addr, data));
        MemOps::with_raw(inp, out, out_fail, |data| self.view.write_raw_iter(data))
    }

    fn metadata(&self) -> PhysicalMemoryMetadata {
        let md = self.view.metadata();

        PhysicalMemoryMetadata {
            max_address: md.max_address,
            real_size: md.real_size,
            readonly: md.readonly,
            ideal_batch_size: 4096,
        }
    }
}

impl<P: MemoryView + 'static> ConnectorCpuState for QemuProcfs<P> {
    type CpuStateType<'a> = Fwd<&'a mut QemuProcfs<P>>;
    type IntoCpuStateType = QemuProcfs<P>;

    fn cpu_state(&mut self) -> Result<Self::CpuStateType<'_>> {
        Ok(self.forward_mut())
    }

    fn into_cpu_state(self) -> Result<Self::IntoCpuStateType> {
        Ok(self)
    }
}

impl<P: MemoryView> CpuState for QemuProcfs<P> {
    fn pause(&mut self) {}

    fn resume(&mut self) {}
}

fn validator() -> ArgsValidator {
    ArgsValidator::new()
        .arg(ArgDescriptor::new("map_base").description("override of VM memory base"))
        .arg(ArgDescriptor::new("map_size").description("override of VM memory size"))
        .arg(
            ArgDescriptor::new("qemu_binary_name").description("override default QEMU binary name"),
        )
}

/// Creates a new Qemu Procfs instance.
#[connector(
    name = "qemu",
    help_fn = "help",
    target_list_fn = "target_list",
    accept_input = true,
    return_wrapped = true
)]
fn create_plugin(
    args: &ConnectorArgs,
    os: Option<OsInstanceArcBox<'static>>,
    lib: LibArc,
) -> Result<ConnectorInstanceArcBox<'static>> {
    let os = os.map(Result::Ok).unwrap_or_else(|| {
        memflow_native::create_os(
            &Default::default(),
            Option::<std::sync::Arc<_>>::None.into(),
        )
    })?;

    let qemu = create_connector_with_os(args, os)?;
    Ok(memflow::plugins::connector::create_instance(
        qemu, lib, args, false,
    ))
}

pub fn create_connector(
    args: &ConnectorArgs,
) -> Result<QemuProcfs<IntoProcessInstanceArcBox<'static>>> {
    create_connector_with_os(
        args,
        memflow_native::create_os(
            &Default::default(),
            Option::<std::sync::Arc<_>>::None.into(),
        )?,
    )
}

pub fn create_connector_with_os<O: Os>(
    args: &ConnectorArgs,
    os: O,
) -> Result<QemuProcfs<O::IntoProcessType>> {
    let validator = validator();

    let name = args.target.as_deref();

    let args = &args.extra_args;

    let qemu = match validator.validate(args) {
        Ok(_) => {
            let map_override = args
                .get("map_base")
                .and_then(|base| umem::from_str_radix(base, 16).ok())
                .zip(
                    args.get("map_size")
                        .and_then(|size| umem::from_str_radix(size, 16).ok()),
                )
                .map(|(start, size)| CTup2(Address::from(start), size));

            let qemu_binary_name_override = args.get("qemu_binary_name").map(|s| s.to_string());

            if let Some(name) = name.or_else(|| args.get("name")) {
                if let Ok(pid) = Pid::from_str_radix(name, 10) {
                    QemuProcfs::with_pid(os, pid, map_override)
                } else {
                    QemuProcfs::with_guest_name(os, name, map_override, qemu_binary_name_override)
                }
            } else {
                QemuProcfs::new(os, map_override, qemu_binary_name_override)
            }
        }
        Err(err) => {
            error!(
                "unable to validate provided arguments, valid arguments are:\n{}",
                validator
            );
            Err(err)
        }
    }?;

    Ok(qemu)
}

/// Retrieve the help text for the Qemu Procfs Connector.
pub fn help() -> String {
    let validator = validator();
    format!(
        "\
The `qemu` connector implements a memflow plugin interface
for QEMU on top of the Process Filesystem on Linux.

This connector requires access to the qemu process via the linux procfs.
This means any process which loads this connector requires
to have at least ptrace permissions set.

The `target` argument specifies the target qemu virtual machine.
The qemu virtual machine name can be specified when starting qemu with the -name flag.

Alternatively, if `target` is a number, qemu process by PID will be accessed.

Available arguments are:
{validator}"
    )
}

/// Retrieve a list of all currently available Qemu targets.
pub fn target_list() -> Result<Vec<TargetInfo>> {
    let mut os = memflow_native::create_os(
        &Default::default(),
        Option::<std::sync::Arc<_>>::None.into(),
    )?;

    let mut out = vec![];

    let callback = &mut |info: ProcessInfo| {
        if is_qemu(&info) {
            if let Some(n) = qemu_arg_opt(info.command_line.split_whitespace(), "-name", "guest") {
                out.push(TargetInfo {
                    name: ReprCString::from(n),
                });
            }
        }

        true
    };

    os.process_info_list_callback(callback.into())?;

    Ok(out)
}
