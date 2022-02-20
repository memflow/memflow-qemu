use log::info;

use crate::qemu_args::qemu_arg_opt;

use memflow::prelude::v1::{
    mem, umem, Address, CTup2, Error, ErrorKind, ErrorOrigin, MemoryMap, Result,
};

#[cfg(all(target_os = "linux", feature = "qmp"))]
use {
    qapi::{qmp, Qmp},
    std::io::{Read, Write},
    std::net::TcpStream,
    std::os::unix::net::UnixStream,
};

#[derive(Debug, Clone)]
struct Mapping {
    pub range_start: umem,
    pub range_end: umem,
    pub remap_start: umem,
}

impl Mapping {
    pub const fn new(range_start: umem, range_end: umem, remap_start: umem) -> Self {
        Self {
            range_start,
            range_end,
            remap_start,
        }
    }
}

pub fn qemu_mem_mappings(
    cmdline: &str,
    qemu_map: &CTup2<Address, umem>,
) -> Result<MemoryMap<(Address, umem)>> {
    let mut mem_map = MemoryMap::new();

    let mappings = if let Ok(mappings) = qmp_get_mtree(cmdline.split_whitespace()) {
        mappings
    } else {
        // find machine architecture and type
        let machine = if !cmdline.is_empty()
            && cmdline
                .split_whitespace()
                .next()
                .unwrap()
                .contains("aarch64")
        {
            "aarch64".into()
        } else {
            qemu_arg_opt(cmdline.split_whitespace(), "-machine", "type")
                .unwrap_or_else(|| "pc".into())
        };
        info!("qemu process started with machine: {}", machine);
        qemu_get_mtree_fallback(&machine, qemu_map)
    };

    // add all mappings
    for mapping in mappings.iter() {
        mem_map.push_range(
            mapping.range_start.into(),
            mapping.range_end.into(),
            qemu_map.0 + mapping.remap_start,
        );
    }

    Ok(mem_map)
}

#[cfg(all(target_os = "linux", feature = "qmp"))]
fn qmp_get_mtree<'a>(cmdline: impl IntoIterator<Item = &'a str>) -> Result<Vec<Mapping>> {
    // -qmp unix:/tmp/qmp-win10-reversing.sock,server,nowait
    let socket_addr = qemu_arg_opt(cmdline, "-qmp", "")
        .ok_or(Error(ErrorOrigin::Connector, ErrorKind::Configuration))?;
    if socket_addr.starts_with("unix:") {
        let socket_path = socket_addr
            .strip_prefix("unix:")
            .ok_or(Error(ErrorOrigin::Connector, ErrorKind::Configuration))?;

        info!("connecting to qmp unix socket at: {}", socket_path);
        let stream = UnixStream::connect(socket_path).map_err(|err| {
            Error(ErrorOrigin::Connector, ErrorKind::Configuration).log_error(err)
        })?;

        qmp_get_mtree_stream(&stream)
    } else if socket_addr.starts_with("tcp:") {
        let socket_url = socket_addr
            .strip_prefix("tcp:")
            .ok_or(Error(ErrorOrigin::Connector, ErrorKind::Configuration))?;

        info!("connecting to qmp tcp socket at: {}", socket_url);

        let stream = TcpStream::connect(socket_url).map_err(|err| {
            Error(ErrorOrigin::Connector, ErrorKind::Configuration).log_error(err)
        })?;

        qmp_get_mtree_stream(&stream)
    } else {
        Err(Error(ErrorOrigin::Connector, ErrorKind::Configuration))
    }
}

#[cfg(all(target_os = "linux", feature = "qmp"))]
fn qmp_get_mtree_stream<S: Read + Write + Clone>(stream: S) -> Result<Vec<Mapping>> {
    let mut qmp = Qmp::from_stream(stream);
    qmp.handshake()
        .map_err(|err| Error(ErrorOrigin::Connector, ErrorKind::Configuration).log_error(err))?;

    let mtreestr = qmp
        .execute(&qmp::human_monitor_command {
            command_line: "info mtree -f".to_owned(),
            cpu_index: None,
        })
        .map_err(|err| Error(ErrorOrigin::Connector, ErrorKind::Configuration).log_error(err))?;

    Ok(qmp_parse_mtree(&mtreestr))
}

#[cfg(not(all(target_os = "linux", feature = "qmp")))]
fn qmp_get_mtree<'a>(_cmdline: impl IntoIterator<Item = &'a str>) -> Result<Vec<Mapping>> {
    Err(Error(
        ErrorOrigin::Connector,
        ErrorKind::UnsupportedOptionalFeature,
    ))
}

#[cfg(all(target_os = "linux", feature = "qmp"))]
fn qmp_parse_mtree(mtreestr: &str) -> Vec<Mapping> {
    let mut mappings = Vec::new();
    for line in mtreestr
        .lines()
        .filter(|l| l.contains("pc.ram"))
        .map(|l| l.trim())
    {
        let range = scan_fmt_some!(line, "{x}-{x} {*[^:]}: pc.ram {*[@]}{x} KVM", [hex umem], [hex umem], [hex umem]);
        if range.0.is_some() && range.1.is_some() {
            // on some systems the second list of memory mappings (mem-container-smram)
            // does not exactly line up with the first mappings (system).
            // hence we clear the list here again in case we encounter a new set of mappings.
            if range.2.is_none() {
                mappings.clear();
            }

            // add the mapping here, in case the third entry is None
            // we just add the first start mapping here.
            // this should only ever happen for the first entry which starts/remaps at/to 0.
            mappings.push(Mapping::new(
                range.0.unwrap(),
                range.1.unwrap() + 1,
                range.2.unwrap_or_else(|| range.0.unwrap()),
            ))
        }
    }
    mappings
}

fn qemu_get_mtree_fallback(
    machine: &str,
    &CTup2(_, map_size): &CTup2<Address, umem>,
) -> Vec<Mapping> {
    info!("qemu memory map size: {:x}", map_size);

    if machine.contains("q35") {
        if map_size >= mem::mb(2816) {
            info!("using fallback memory mappings for q35 with more than 2816mb of ram");
            qemu_get_mtree_fallback_q35(map_size)
        } else {
            info!("using fallback memory mappings for q35 with less than 2816mb of ram");
            qemu_get_mtree_fallback_q35_smallmem(map_size)
        }
    } else if machine.contains("aarch64") {
        info!("using fallback memory mappings for aarch64");
        qemu_get_mtree_fallback_aarch64(map_size)
    } else {
        info!("using fallback memory mappings for pc-i1440fx");
        qemu_get_mtree_fallback_pc(map_size)
    }
}

/// Returns hard-coded mem-mappings for q35 qemu machine types with more than 2816 mb of ram.
fn qemu_get_mtree_fallback_q35(map_size: umem) -> Vec<Mapping> {
    /*
    0000000000000000-000000000009ffff (prio 0, ram): pc.ram KVM
    00000000000c0000-00000000000c3fff (prio 0, rom): pc.ram @00000000000c0000 KVM
    0000000000100000-000000007fffffff (prio 0, ram): pc.ram @0000000000100000 KVM
    0000000100000000-000000047fffffff (prio 0, ram): pc.ram @0000000080000000 KVM
    */
    vec![
        Mapping::new(mem::mb(1), mem::gb(2), mem::mb(1)),
        Mapping::new(mem::gb(4), map_size + mem::gb(2), mem::gb(2)),
    ]
}

/// Returns hard-coded mem-mappings for q35 qemu machine types with less than 2816 mb of ram.
fn qemu_get_mtree_fallback_q35_smallmem(map_size: umem) -> Vec<Mapping> {
    // Same as above but without the second mapping
    vec![Mapping::new(mem::mb(1), map_size, mem::mb(1))]
}

/// Returns hard-coded mem-mappings for aarch64 qemu machine types.
fn qemu_get_mtree_fallback_aarch64(map_size: umem) -> Vec<Mapping> {
    // It is not known for sure whether this is correct for all ARM machines, but
    // it seems like all memory on qemu ARM is shifted by 1GB and is linear from there.
    vec![Mapping::new(mem::gb(1), map_size + mem::gb(1), 0u64)]
}

/// Returns hard-coded mem-mappings for pc-i1440fx qemu machine types.
fn qemu_get_mtree_fallback_pc(map_size: umem) -> Vec<Mapping> {
    /*
    0000000000000000-00000000000bffff (prio 0, ram): pc.ram KVM
    00000000000c0000-00000000000cafff (prio 0, rom): pc.ram @00000000000c0000 KVM
    00000000000cb000-00000000000cdfff (prio 0, ram): pc.ram @00000000000cb000 KVM
    00000000000ce000-00000000000e7fff (prio 0, rom): pc.ram @00000000000ce000 KVM
    00000000000e8000-00000000000effff (prio 0, ram): pc.ram @00000000000e8000 KVM
    00000000000f0000-00000000000fffff (prio 0, rom): pc.ram @00000000000f0000 KVM
    0000000000100000-00000000bfffffff (prio 0, ram): pc.ram @0000000000100000 KVM
    0000000100000000-000000023fffffff (prio 0, ram): pc.ram @00000000c0000000 KVM
    */
    vec![
        Mapping::new(0u64, mem::kb(768), 0u64),
        Mapping::new(mem::kb(812), mem::kb(824), mem::kb(812)),
        Mapping::new(mem::kb(928), mem::kb(960), mem::kb(928)),
        Mapping::new(mem::mb(1), mem::gb(3), mem::mb(1)),
        Mapping::new(mem::gb(4), map_size + mem::gb(1), mem::gb(3)),
    ]
}

#[cfg(test)]
#[cfg(all(target_os = "linux", feature = "qmp"))]
mod tests {
    use super::qmp_parse_mtree;

    #[test]
    fn test_parse_mtree() {
        let mtreestr = r#"
            0000000000000000-000000000009ffff (prio 0, ram): pc.ram KVM
            00000000000a0000-00000000000affff (prio 1, ram): vga.vram KVM
            00000000000b0000-00000000000bffff (prio 1, i/o): vga-lowmem @0000000000010000
            00000000000c0000-00000000000c3fff (prio 0, rom): pc.ram @00000000000c0000 KVM
            00000000000c4000-00000000000dffff (prio 1, rom): pc.rom @0000000000004000 KVM
            00000000000e0000-00000000000fffff (prio 1, rom): isa-bios KVM
            0000000000100000-000000007fffffff (prio 0, ram): pc.ram @0000000000100000 KVM
            0000000100000000-000000047fffffff (prio 0, ram): pc.ram @0000000080000000 KVM
            0000000000000000-000000000009ffff (prio 0, ram): pc.ram KVM
            00000000000a0000-00000000000affff (prio 1, ram): vga.vram KVM
            00000000000b0000-00000000000bffff (prio 1, i/o): vga-lowmem @0000000000010000
            00000000000c0000-00000000000c3fff (prio 0, rom): pc.ram @00000000000c0000 KVM
            00000000000c4000-00000000000dffff (prio 1, rom): pc.rom @0000000000004000 KVM
            00000000000e0000-00000000000fffff (prio 1, rom): isa-bios KVM
            0000000000100000-000000007fffffff (prio 0, ram): pc.ram @0000000000100000 KVM
            0000000100000000-000000047fffffff (prio 0, ram): pc.ram @0000000080000000 KVM"#;

        let mappings = qmp_parse_mtree(mtreestr);

        assert_eq!(mappings.len(), 4);

        assert_eq!(mappings[0].range_start, 0);
        assert_eq!(mappings[0].range_end, 0xa0000);
        assert_eq!(mappings[0].remap_start, 0);

        assert_eq!(mappings[1].range_start, 0xc0000);
        assert_eq!(mappings[1].range_end, 0xc4000);
        assert_eq!(mappings[1].remap_start, 0xc0000);

        assert_eq!(mappings[2].range_start, 0x100000);
        assert_eq!(mappings[2].range_end, 0x80000000);
        assert_eq!(mappings[2].remap_start, 0x100000);

        assert_eq!(mappings[3].range_start, 0x100000000);
        assert_eq!(mappings[3].range_end, 0x480000000);
        assert_eq!(mappings[3].remap_start, 0x80000000);
    }
}
