/*!
This example shows how to use the qemu_procfs connector to read physical_memory
from a target machine. It also evaluates the number of read cycles per second
and prints them to stdout.
*/
use std::time::Instant;

use log::{info, Level};

use memflow::prelude::v1::*;

fn main() {
    simple_logger::SimpleLogger::new()
        .with_level(Level::Debug.to_level_filter())
        .init()
        .unwrap();

    let mut connector = memflow_qemu_procfs::create_connector(&Args::default(), Level::Debug)
        .expect("unable to create qemu_procfs connector");

    let metadata = connector.metadata();
    info!("Received metadata: {:?}", metadata);

    let mut mem = vec![0; 8];
    connector
        .phys_read_raw_into(Address::from(0x1000).into(), &mut mem)
        .expect("unable to read physical memory");
    info!("Received memory: {:?}", mem);

    let start = Instant::now();
    let mut counter = 0;
    loop {
        let mut buf = vec![0; 0x1000];
        connector
            .phys_read_raw_into(Address::from(0x1000).into(), &mut buf)
            .expect("unable to read physical memory");

        counter += 1;
        if (counter % 10000000) == 0 {
            let elapsed = start.elapsed().as_millis() as f64;
            if elapsed > 0.0 {
                info!("{} reads/sec", (f64::from(counter)) / elapsed * 1000.0);
                info!("{} ms/read", elapsed / (f64::from(counter)));
            }
        }
    }
}
