/*
0000000000000000-00000000000bffff (prio 0, ram): pc.ram KVM
00000000000c0000-00000000000cafff (prio 0, rom): pc.ram @00000000000c0000 KVM
00000000000cb000-00000000000cdfff (prio 0, ram): pc.ram @00000000000cb000 KVM
00000000000ce000-00000000000e7fff (prio 0, rom): pc.ram @00000000000ce000 KVM
00000000000e8000-00000000000effff (prio 0, ram): pc.ram @00000000000e8000 KVM
00000000000f0000-00000000000fffff (prio 0, rom): pc.ram @00000000000f0000 KVM
0000000000100000-00000000bfffffff (prio 0, ram): pc.ram @0000000000100000 KVM
0000000100000000-000000023fffffff (prio 0, ram): pc.ram @00000000c0000000 KVM

mem_map.push_range(Address::NULL, size::kb(768).into(), map_base.into()); // section: [start - 768kb] -> map to start
mem_map.push_range(
    size::kb(812).into(),
    size::kb(824).into(),
    (map_base + size::kb(812)).into(),
); // section: [768kb - 812kb] -> map to 768kb
*/

use log::info;

use crate::qemu_args::qemu_arg_opt;

use memflow::prelude::v1::{size, Address, Error, ErrorKind, ErrorOrigin, MemoryMap, Result};

#[cfg(feature = "qmp")]
use qapi::{qmp, Qmp};
#[cfg(feature = "qmp")]
use std::io::{Read, Write};
#[cfg(feature = "qmp")]
use std::net::TcpStream;
#[cfg(feature = "qmp")]
use std::os::unix::net::UnixStream;

#[derive(Debug, Clone)]
struct Mapping {
    pub range_start: u64,
    pub range_end: u64,
    pub remap_start: u64,
}

impl Mapping {
    pub const fn new(range_start: u64, range_end: u64, remap_start: u64) -> Self {
        Self {
            range_start,
            range_end,
            remap_start,
        }
    }
}

pub fn qemu_mem_mappings(
    cmdline: &[String],
    qemu_map: &procfs::process::MemoryMap,
) -> Result<MemoryMap<(Address, usize)>> {
    let mut mem_map = MemoryMap::new();

    let mappings = if let Ok(mappings) = qmp_get_mtree(cmdline) {
        mappings
    } else {
        // find machine architecture and type
        let machine = if !cmdline.is_empty() && cmdline[0].contains("aarch64") {
            "aarch64".into()
        } else {
            qemu_arg_opt(&cmdline, "-machine", "type").unwrap_or_else(|| "pc".into())
        };
        info!("qemu process started with machine: {}", machine);
        qemu_get_mtree_fallback(&machine, qemu_map)
    };

    // add all mappings
    for mapping in mappings.iter() {
        mem_map.push_range(
            mapping.range_start.into(),
            mapping.range_end.into(),
            (qemu_map.address.0 + mapping.remap_start).into(),
        );
    }

    Ok(mem_map)
}

#[cfg(feature = "qmp")]
fn qmp_get_mtree(cmdline: &[String]) -> Result<Vec<Mapping>> {
    // -qmp unix:/tmp/qmp-win10-reversing.sock,server,nowait
    let socket_addr = qemu_arg_opt(&cmdline, "-qmp", "")
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

#[cfg(feature = "qmp")]
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

#[cfg(not(feature = "qmp"))]
fn qmp_get_mtree(cmdline: &[String]) -> Result<Vec<Mapping>> {
    Err(Error(
        ErrorOrigin::Connector,
        ErrorKind::UnsupportedOptionalFeature,
    ))
}

#[cfg(feature = "qmp")]
fn qmp_parse_mtree(mtreestr: &str) -> Vec<Mapping> {
    let mut lines = mtreestr
        .lines()
        .filter(|l| l.contains("pc.ram"))
        .map(|l| l.trim())
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines.dedup();

    let mut mappings = Vec::new();
    for line in lines.iter() {
        let range = scan_fmt_some!(line, "{x}-{x} {*[^:]}: pc.ram {*[@]}{x} KVM", [hex u64], [hex u64], [hex u64]);
        if range.0.is_some() && range.1.is_some() {
            mappings.push(Mapping::new(
                range.0.unwrap(),
                range.1.unwrap() + 1,
                range.2.unwrap_or_else(|| range.0.unwrap()),
            ))
        }
    }
    mappings
}

fn qemu_get_mtree_fallback(machine: &str, qemu_map: &procfs::process::MemoryMap) -> Vec<Mapping> {
    let map_size = (qemu_map.address.1 - qemu_map.address.0) as u64;
    info!("qemu memory map size: {:x}", map_size);

    if machine.contains("q35") {
        if map_size >= size::mb(2816) as u64 {
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
fn qemu_get_mtree_fallback_q35(map_size: u64) -> Vec<Mapping> {
    /*
    0000000000000000-000000000009ffff (prio 0, ram): pc.ram KVM
    00000000000c0000-00000000000c3fff (prio 0, rom): pc.ram @00000000000c0000 KVM
    0000000000100000-000000007fffffff (prio 0, ram): pc.ram @0000000000100000 KVM
    0000000100000000-000000047fffffff (prio 0, ram): pc.ram @0000000080000000 KVM
    */
    vec![
        Mapping::new(size::mb(1) as u64, size::gb(2) as u64, size::mb(1) as u64),
        Mapping::new(
            size::gb(4) as u64,
            map_size + size::gb(2) as u64,
            size::gb(2) as u64,
        ),
    ]
}

/// Returns hard-coded mem-mappings for q35 qemu machine types with less than 2816 mb of ram.
fn qemu_get_mtree_fallback_q35_smallmem(map_size: u64) -> Vec<Mapping> {
    // Same as above but without the second mapping
    vec![Mapping::new(
        size::mb(1) as u64,
        map_size,
        size::mb(1) as u64,
    )]
}

/// Returns hard-coded mem-mappings for aarch64 qemu machine types.
fn qemu_get_mtree_fallback_aarch64(map_size: u64) -> Vec<Mapping> {
    // It is not known for sure whether this is correct for all ARM machines, but
    // it seems like all memory on qemu ARM is shifted by 1GB and is linear from there.
    vec![Mapping::new(
        size::gb(1) as u64,
        map_size + size::gb(1) as u64,
        0u64,
    )]
}

/// Returns hard-coded mem-mappings for pc-i1440fx qemu machine types.
fn qemu_get_mtree_fallback_pc(map_size: u64) -> Vec<Mapping> {
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
        Mapping::new(0u64, size::kb(768) as u64, 0u64),
        Mapping::new(
            size::kb(812) as u64,
            size::kb(824) as u64,
            size::kb(812) as u64,
        ),
        Mapping::new(
            size::kb(928) as u64,
            size::kb(960) as u64,
            size::kb(928) as u64,
        ),
        Mapping::new(size::mb(1) as u64, size::gb(3) as u64, size::mb(1) as u64),
        Mapping::new(
            size::gb(4) as u64,
            map_size + size::gb(1) as u64,
            size::gb(3) as u64,
        ),
    ]
}

#[cfg(test)]
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
