pub fn is_qemu(process: &procfs::process::Process) -> bool {
    process
        .cmdline()
        .ok()
        .and_then(|cmdline| {
            cmdline.get(0).and_then(|cmd| {
                std::path::Path::new(cmd)
                    .file_name()
                    .and_then(|exe| exe.to_str())
                    .map(|v| v.contains("qemu-system-"))
            })
        })
        .unwrap_or(false)
}

pub fn qemu_arg_opt(args: &[String], argname: &str, argopt: &str) -> Option<String> {
    for (idx, arg) in args.iter().enumerate() {
        if arg == argname {
            let name = args[idx + 1].split(',');
            for (i, kv) in name.clone().enumerate() {
                let kvsplt = kv.split('=').collect::<Vec<_>>();
                if kvsplt.len() == 2 {
                    if kvsplt[0] == argopt {
                        return Some(kvsplt[1].to_string());
                    }
                } else if i == 0 {
                    return Some(kv.to_string());
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        assert_eq!(
            qemu_arg_opt(
                &["-name".to_string(), "win10-test".to_string()],
                "-name",
                "guest"
            ),
            Some("win10-test".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &[
                    "-test".to_string(),
                    "-name".to_string(),
                    "win10-test".to_string()
                ],
                "-name",
                "guest"
            ),
            Some("win10-test".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-name".to_string(), "win10-test,arg=opt".to_string()],
                "-name",
                "guest"
            ),
            Some("win10-test".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-name".to_string(), "guest=win10-test,arg=opt".to_string()],
                "-name",
                "guest"
            ),
            Some("win10-test".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-name".to_string(), "arg=opt,guest=win10-test".to_string()],
                "-name",
                "guest"
            ),
            Some("win10-test".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-name".to_string(), "arg=opt".to_string()],
                "-name",
                "guest"
            ),
            None
        );
    }

    #[test]
    fn test_machine() {
        assert_eq!(
            qemu_arg_opt(
                &["-machine".to_string(), "q35".to_string()],
                "-machine",
                "type"
            ),
            Some("q35".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &[
                    "-test".to_string(),
                    "-machine".to_string(),
                    "q35".to_string()
                ],
                "-machine",
                "type"
            ),
            Some("q35".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-machine".to_string(), "q35,arg=opt".to_string()],
                "-machine",
                "type"
            ),
            Some("q35".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-machine".to_string(), "type=pc,arg=opt".to_string()],
                "-machine",
                "type"
            ),
            Some("pc".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &[
                    "-machine".to_string(),
                    "arg=opt,type=pc-i1440fx".to_string()
                ],
                "-machine",
                "type"
            ),
            Some("pc-i1440fx".into())
        );
        assert_eq!(
            qemu_arg_opt(
                &["-machine".to_string(), "arg=opt".to_string()],
                "-machine",
                "type"
            ),
            None
        );
    }
}
