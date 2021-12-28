use log::{error, info};

use memflow::cglue;
use memflow::connector::cpu_state::*;
use memflow::mem::memory_view::RemapView;
use memflow::mem::phys_mem::*;
use memflow::os::root::OsInner;
use memflow::prelude::v1::*;

mod qemu_args;
use qemu_args::{is_qemu, qemu_arg_opt};

#[cfg(feature = "qmp")]
#[macro_use]
extern crate scan_fmt;

mod mem_map;
use mem_map::qemu_mem_mappings;

cglue_impl_group!(QemuProcfs<P: MemoryView + Clone>, ConnectorInstance<'a>, { ConnectorCpuStateInner<'a> });
cglue_impl_group!(QemuProcfs<P: MemoryView + Clone>, IntoCpuState);

#[derive(Clone)]
pub struct QemuProcfs<P: MemoryView> {
    view: RemapView<P>,
}

impl<P: MemoryView + Process> QemuProcfs<P> {
    pub fn new<O: OsInner<'static, IntoProcessType = P>>(
        mut os: O,
        map_override: Option<MemoryRange>,
    ) -> Result<Self> {
        let mut proc = None;

        let callback = &mut |info: ProcessInfo| {
            if proc.is_none() && is_qemu(&info) {
                proc = Some(info);
            }

            !proc.is_some()
        };

        os.process_info_list_callback(callback.into())?;

        Self::with_process(
            os,
            proc.ok_or(Error(ErrorOrigin::Connector, ErrorKind::NotFound))?,
            map_override,
        )
    }

    pub fn with_guest_name<O: OsInner<'static, IntoProcessType = P>>(
        mut os: O,
        name: &str,
        map_override: Option<MemoryRange>,
    ) -> Result<Self> {
        let mut proc = None;

        let callback = &mut |info: ProcessInfo| {
            if proc.is_none()
                && is_qemu(&info)
                && qemu_arg_opt(info.command_line.split_whitespace(), "-name", "guest").as_deref()
                    == Some(name)
            {
                proc = Some(info);
            }

            !proc.is_some()
        };

        os.process_info_list_callback(callback.into())?;

        Self::with_process(
            os,
            proc.ok_or(Error(ErrorOrigin::Connector, ErrorKind::NotFound))?,
            map_override,
        )
    }

    fn with_process<O: OsInner<'static, IntoProcessType = P>>(
        os: O,
        info: ProcessInfo,
        map_override: Option<MemoryRange>,
    ) -> Result<Self> {
        info!(
            "qemu process with name {} found with pid {:?}",
            info.name, info.pid
        );

        let cmdline: String = info.command_line.to_string();

        let mut prc = os.into_process_by_info(info)?;

        let mut biggest_map = map_override;

        let callback = &mut |range: MemoryRange| {
            if biggest_map
                .map(|MemData(_, oldsize): MemoryRange| oldsize < range.1)
                .unwrap_or(true)
            {
                biggest_map = Some(range);
            }

            true
        };

        if map_override.is_none() {
            prc.mapped_mem_range(
                smem::mb(-1),
                Address::NULL,
                Address::INVALID,
                callback.into(),
            );
        }

        let qemu_map = biggest_map.ok_or(Error(ErrorOrigin::Connector, ErrorKind::NotFound))?;

        info!("qemu memory map found {:?}", qemu_map);

        Self::with_cmdline_and_mem(prc, &cmdline, qemu_map)
    }

    fn with_cmdline_and_mem(prc: P, cmdline: &str, qemu_map: MemoryRange) -> Result<Self> {
        let mem_map = qemu_mem_mappings(cmdline, &qemu_map)?;
        info!("qemu machine mem_map: {:?}", mem_map);

        Ok(Self {
            view: prc.into_remap_view(mem_map),
        })
    }
}

impl<P: MemoryView> PhysicalMemory for QemuProcfs<P> {
    fn phys_read_raw_iter<'a>(
        &mut self,
        data: CIterator<PhysicalReadData<'a>>,
        out_fail: &mut ReadFailCallback<'_, 'a>,
    ) -> Result<()> {
        let mut iter = data.map(|MemData(addr, data)| MemData(addr.into(), data));
        let fail = &mut |MemData(a, b): ReadData<'a>| out_fail.call(MemData(a, b));
        self.view
            .read_raw_iter((&mut iter).into(), &mut (fail.into()))
    }

    fn phys_write_raw_iter<'a>(
        &mut self,
        data: CIterator<PhysicalWriteData<'a>>,
        out_fail: &mut WriteFailCallback<'_, 'a>,
    ) -> Result<()> {
        let mut iter = data.map(|MemData(addr, data)| MemData(addr.into(), data));
        self.view.write_raw_iter((&mut iter).into(), out_fail)
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

impl<'a, P: MemoryView + 'static> ConnectorCpuStateInner<'a> for QemuProcfs<P> {
    type CpuStateType = Fwd<&'a mut QemuProcfs<P>>;
    type IntoCpuStateType = QemuProcfs<P>;

    fn cpu_state(&'a mut self) -> Result<Self::CpuStateType> {
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
        .arg(ArgDescriptor::new("default").description(
            "the name of the qemu virtual machine (specified with -name when starting qemu)",
        ))
        .arg(ArgDescriptor::new("name").description(
            "the name of the qemu virtual machine (specified with -name when starting qemu)",
        ))
        .arg(ArgDescriptor::new("map_base").description("override of VM memory base"))
        .arg(ArgDescriptor::new("map_size").description("override of VM memory size"))
}

/// Creates a new Qemu Procfs instance.
#[connector_bare(name = "qemu", help_fn = "help", target_list_fn = "target_list")]
fn create_plugin(
    args: &ConnectorArgs,
    os: Option<OsInstanceArcBox<'static>>,
    lib: LibArc,
) -> Result<ConnectorInstanceArcBox<'static>> {
    let os = os.map(Result::Ok).unwrap_or_else(|| {
        memflow_native::build_os(
            &Default::default(),
            None,
            Option::<std::sync::Arc<_>>::None.into(),
        )
    })?;

    let qemu = create_connector_full(args, os)?;

    Ok(group_obj!((qemu, lib) as ConnectorInstance))
}

pub fn create_connector(
    args: &ConnectorArgs,
) -> Result<QemuProcfs<IntoProcessInstanceArcBox<'static>>> {
    create_connector_full(
        args,
        memflow_native::build_os(
            &Default::default(),
            None,
            Option::<std::sync::Arc<_>>::None.into(),
        )?,
    )
}

pub fn create_connector_full<O: OsInner<'static>>(
    args: &ConnectorArgs,
    os: O,
) -> Result<QemuProcfs<O::IntoProcessType>> {
    let validator = validator();

    let name = args.target.as_ref().map(|s| &**s);

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
                .map(|(start, size)| MemData(Address::from(start), size));

            if let Some(name) = name.or_else(|| args.get("name")) {
                QemuProcfs::with_guest_name(os, name, map_override)
            } else {
                QemuProcfs::new(os, map_override)
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
    let mut os = memflow_native::build_os(
        &Default::default(),
        None,
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
