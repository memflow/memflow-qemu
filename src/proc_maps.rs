use std::fs::File;
use std::io::{BufRead, BufReader};

use memflow::prelude::v1::{umem, Address};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcMap {
    pub start: Address,
    pub end: Address,
    pub perms: [u8; 4],
    pub offset: u64,
    pub dev_major: u32,
    pub dev_minor: u32,
    pub inode: u64,
    pub path: String,
}

impl ProcMap {
    pub fn size(&self) -> umem {
        (self.end - self.start) as umem
    }

    pub fn is_readable(&self) -> bool {
        self.perms[0] == b'r'
    }

    pub fn is_writable(&self) -> bool {
        self.perms[1] == b'w'
    }

    pub fn is_executable(&self) -> bool {
        self.perms[2] == b'x'
    }

    pub fn is_ram_candidate(&self) -> bool {
        if !self.is_readable() || !self.is_writable() || self.is_executable() {
            return false;
        }

        if self.path.starts_with("anon_inode:[vfio-") || self.path.starts_with("/dev/kvmfr") {
            return false;
        }

        if self.path.starts_with('[') && self.path.ends_with(']') {
            return false;
        }

        if self.path.ends_with(".so") || self.path.contains(".so.") {
            return false;
        }

        true
    }
}

pub fn read(pid: u32) -> std::io::Result<Vec<ProcMap>> {
    let f = File::open(format!("/proc/{pid}/maps"))?;
    let mut out = Vec::new();

    for line in BufReader::new(f).lines() {
        if let Some(map) = parse_line(&line?) {
            out.push(map);
        }
    }
    Ok(out)
}

fn parse_line(line: &str) -> Option<ProcMap> {
    let mut it = line.split_ascii_whitespace();
    let range = it.next()?;
    let perms_str = it.next()?;
    let offset_str = it.next()?;
    let dev_str = it.next()?;
    let inode_str = it.next()?;
    let path = {
        let rest: Vec<&str> = it.collect();
        rest.join(" ")
    };

    let (start_str, end_str) = range.split_once('-')?;
    let start = u64::from_str_radix(start_str, 16).ok()?;
    let end = u64::from_str_radix(end_str, 16).ok()?;

    let mut perms = [b'-'; 4];
    for (i, b) in perms_str.bytes().take(4).enumerate() {
        perms[i] = b;
    }

    let offset = u64::from_str_radix(offset_str, 16).ok()?;

    let (major_str, minor_str) = dev_str.split_once(':')?;
    let dev_major = u32::from_str_radix(major_str, 16).ok()?;
    let dev_minor = u32::from_str_radix(minor_str, 16).ok()?;

    let inode = inode_str.parse().ok()?;

    Some(ProcMap {
        start: Address::from(start),
        end: Address::from(end),
        perms,
        offset,
        dev_major,
        dev_minor,
        inode,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn parses_16g_pc_ram_line() {
        // real capture from a Windows 10 Q35 VM with 16 GiB RAM + RTX 5090
        let line = "7f4f2be00000-7f532be00000 rw-p 00000000 00:00 0 ";
        let m = parse_line(line).unwrap();

        assert_eq!(&m.perms, b"rw-p");
        assert_eq!(m.offset, 0);
        assert_eq!(m.inode, 0);
        assert_eq!(m.dev_major, 0);
        assert_eq!(m.dev_minor, 0);
        assert_eq!(m.path, "");
        assert_eq!(m.size(), 16 * GIB);
        assert!(m.is_ram_candidate());
    }

    #[test]
    fn parses_and_rejects_vfio_bar_line() {
        // this is the mmap that used to win the biggest-map heuristic and caused this issue
        let line = "7f46c0000000-7f4ec0000000 rw-s 10000000000 00:11 122                     anon_inode:[vfio-device]";
        let m = parse_line(line).unwrap();

        assert_eq!(&m.perms, b"rw-s");
        assert_eq!(m.size(), 32 * GIB);
        assert_eq!(m.path, "anon_inode:[vfio-device]");
        assert!(!m.is_ram_candidate());
    }

    #[test]
    fn parses_and_rejects_kvmfr_line() {
        let line =
            "7f4f1bfff000-7f4f23fff000 rw-s 00000000 00:07 783                        /dev/kvmfr0";
        let m = parse_line(line).unwrap();

        assert_eq!(&m.perms, b"rw-s");
        assert_eq!(m.path, "/dev/kvmfr0");
        assert!(!m.is_ram_candidate());
    }

    #[test]
    fn parses_and_rejects_heap_stack() {
        let heap =
            "55f4820d5000-55f483e6c000 rw-p 00000000 00:00 0                          [heap]";
        let m = parse_line(heap).unwrap();

        assert_eq!(m.path, "[heap]");
        assert!(!m.is_ram_candidate());

        let stack =
            "7ffcafffe000-7ffcb0000000 rw-p 00000000 00:00 0                          [stack]";
        let m = parse_line(stack).unwrap();

        assert!(!m.is_ram_candidate());
    }

    #[test]
    fn parses_and_rejects_executable_binary() {
        let line = "5622861de000-562286555000 r-xp 0019d000 fd:01 142002304                  /usr/bin/qemu-system-x86_64";
        let m = parse_line(line).unwrap();

        assert!(m.is_executable());
        assert!(!m.is_ram_candidate());
    }

    #[test]
    fn parses_and_rejects_shared_object() {
        let line = "7f9b12345000-7f9b12500000 rw-p 00000000 fd:01 42                         /usr/lib/libglib-2.0.so.0";
        let m = parse_line(line).unwrap();

        assert!(!m.is_ram_candidate());
    }

    #[test]
    fn accepts_memfd_backed_pc_ram() {
        let line = "7f00_0000_0000-7f04_0000_0000 rw-s 00000000 00:01 12345                     /memfd:pc.ram (deleted)"
            .replace('_', "");
        let m = parse_line(&line).unwrap();

        assert_eq!(m.path, "/memfd:pc.ram (deleted)");
        assert!(m.is_ram_candidate());
    }

    #[test]
    fn accepts_hugetlbfs_backed_pc_ram() {
        let line = "7f00_0000_0000-7f04_0000_0000 rw-s 00000000 00:32 56789                     /dev/hugepages/libvirt/qemu/1-win10/ram-node0"
            .replace('_', "");
        let m = parse_line(&line).unwrap();

        assert!(m.is_ram_candidate());
    }

    #[test]
    fn parse_line_survives_short_input() {
        assert!(parse_line("").is_none());
        assert!(parse_line("garbage").is_none());
        assert!(parse_line("1000-2000 rw-p").is_none());
    }
}
