use log::{error, info};

use memflow::cglue;
use memflow::connector::cpu_state::*;
use memflow::mem::memory_view::RemapView;
use memflow::mem::phys_mem::*;
use memflow::mem::virt_translate::MemoryRange;
use memflow::os::root::OsInner;
use memflow::prelude::v1::*;

mod qemu_args;
use qemu_args::{is_qemu, qemu_arg_opt};

#[cfg(feature = "qmp")]
#[macro_use]
extern crate scan_fmt;

mod mem_map;
use mem_map::qemu_mem_mappings;

cglue_impl_group!(QemuProcfs/*<P: PhysicalMemory + Clone>*/, ConnectorInstance<'a>, { ConnectorCpuStateInner<'a> });
cglue_impl_group!(
    QemuProcfs, /*<P: PhysicalMemory + Clone>*/
    IntoCpuState
);

#[derive(Clone)]
pub struct QemuProcfs {
    view: RemapView<IntoProcessInstanceArcBox<'static>>,
}

impl QemuProcfs {
    pub fn new(mut os: OsInstanceArcBox<'static>) -> Result<Self> {
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
        )
    }

    pub fn with_guest_name(mut os: OsInstanceArcBox<'static>, name: &str) -> Result<Self> {
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
        )
    }

    fn with_process(os: OsInstanceArcBox<'static>, info: ProcessInfo) -> Result<Self> {
        info!(
            "qemu process with name {} found with pid {:?}",
            info.name, info.pid
        );

        let cmdline: String = info.command_line.to_string();

        let mut prc = os.into_process_by_info(info)?;

        let tr_prc = as_mut!(prc impl VirtualTranslate).ok_or(Error(
            ErrorOrigin::Connector,
            ErrorKind::UnsupportedOptionalFeature,
        ))?;

        let mut biggest_map = None;

        let callback = &mut |range: MemoryRange| {
            if biggest_map
                .map(|m: MemoryRange| m.size < range.size)
                .unwrap_or(true)
            {
                biggest_map = Some(range);
            }

            true
        };

        tr_prc.virt_page_map_range(
            smem::mb(-1),
            Address::NULL,
            Address::INVALID,
            callback.into(),
        );

        let qemu_map = biggest_map.ok_or(Error(ErrorOrigin::Connector, ErrorKind::NotFound))?;

        info!("qemu memory map found {:?}", qemu_map);

        Self::with_cmdline_and_mem(prc, &cmdline, qemu_map)
    }

    fn with_cmdline_and_mem(
        prc: IntoProcessInstanceArcBox<'static>,
        cmdline: &str,
        qemu_map: MemoryRange,
    ) -> Result<Self> {
        let mem_map = qemu_mem_mappings(cmdline, &qemu_map)?;
        info!("qemu machine mem_map: {:?}", mem_map);

        Ok(Self {
            view: prc.into_remap_view(mem_map),
        })
    }
}

impl PhysicalMemory for QemuProcfs {
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

impl<'a> ConnectorCpuStateInner<'a> for QemuProcfs {
    type CpuStateType = Fwd<&'a mut QemuProcfs>;
    type IntoCpuStateType = QemuProcfs;

    fn cpu_state(&'a mut self) -> Result<Self::CpuStateType> {
        Ok(self.forward_mut())
    }

    fn into_cpu_state(self) -> Result<Self::IntoCpuStateType> {
        Ok(self)
    }
}

impl CpuState for QemuProcfs {
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
}

/// Creates a new Qemu Procfs instance.
#[connector_bare(name = "qemu", help_fn = "help", target_list_fn = "target_list")]
pub fn create_connector(
    args: &Args,
    os: Option<OsInstanceArcBox<'static>>,
    lib: CArc<std::ffi::c_void>,
) -> Result<ConnectorInstanceArcBox<'static>> {
    let validator = validator();

    let os = if let Some(os) = os {
        os
    } else {
        memflow_native::build_os(
            &Default::default(),
            None,
            Option::<std::sync::Arc<_>>::None.into(),
        )?
    };

    let qemu = match validator.validate(args) {
        Ok(_) => {
            if let Some(name) = args.get("name").or_else(|| args.get_default()) {
                QemuProcfs::with_guest_name(os, name)
            } else {
                QemuProcfs::new(os)
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

    Ok(group_obj!((qemu, lib) as ConnectorInstance))
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
