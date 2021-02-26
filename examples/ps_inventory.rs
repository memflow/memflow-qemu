/*!
This example shows how to use the qemu_procfs connector in conjunction
with a specific OS layer. This example uses the `Inventory` feature of memflow
to create the connector itself and the os instance.

The example is an adaption of the memflow core process list example:
https://github.com/memflow/memflow/blob/next/memflow/examples/process_list.rs

# Remarks:
To run this example you must have the `qemu_procfs` connector and `win32` plugin installed on your system.
Make sure they can be found in one of the following locations:

~/.local/lib/memflow/
/usr/lib/memflow/

or in any other path found in the official memflow documentation.
*/
use std::env::args;

use log::{info, Level};

use memflow::prelude::v1::*;

fn main() {
    simple_logger::SimpleLogger::new()
        .with_level(Level::Debug.to_level_filter())
        .init()
        .unwrap();

    let connector_args = if let Some(arg) = args().nth(1) {
        Args::parse(arg.as_ref()).expect("unable to parse command line arguments")
    } else {
        Args::default()
    };

    let inventory = Inventory::scan();
    let connector = inventory
        .create_connector("qemu_procfs", None, &connector_args)
        .expect("unable to create qemu_procfs connector");
    let mut os = inventory
        .create_os("win32", Some(connector), &Args::default())
        .expect("unable to create win32 instance with qemu_procfs connector");

    let process_list = os.process_info_list().expect("unable to read process list");

    info!(
        "{:>5} {:>10} {:>10} {:<}",
        "PID", "SYS ARCH", "PROC ARCH", "NAME"
    );

    for p in process_list {
        info!(
            "{:>5} {:^10} {:^10} {}",
            p.pid, p.sys_arch, p.proc_arch, p.name
        );
    }
}
