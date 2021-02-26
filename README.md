# memflow-qemu-procfs

This connector implements an interface for Qemu via the Process Filesystem on Linux.

## Compilation

### Installing the library

The `./install.sh` script will just compile and install the plugin.
The connector will be installed to `~/.local/lib/memflow` by default.
Additionally the `--system` flag can be specified which will install the connector in `/usr/lib/memflow` as well.

### Building the stand-alone connector for dynamic loading

The stand-alone connector of this library is feature-gated behind the `inventory` feature.
To compile a dynamic library for use with the connector inventory use the following command:

```
cargo build --release --all-features
```

### Using the crate in a rust project

To use the connector in a rust project just include it in your Cargo.toml

```
memflow-qemu-procfs = "0.1"
```

Make sure to not enable the `inventory` feature when importing multiple
connectors in a rust project without using the memflow connector inventory.
This might cause duplicated exports being generated in your project.

## Arguments

The following arguments can be used when loading the connector:

- `name` - the name of the virtual machine (default argument, optional)

## Permissions

The `qemu_procfs` connector requires access to the qemu process via the linux procfs. This means any process which loads this connector requires to have at least ptrace permissions set.

To set ptrace permissions on a binary simply use:
```bash
sudo setcap 'CAP_SYS_PTRACE=ep' [filename]
```

Alternatively you can just run the binary via `sudo`.

## Memory Mappings

The connector supports dynamic acquisition of the qemu memory mappings by utilizing the [qemu qmp protocol](https://qemu.readthedocs.io/en/latest/interop/qemu-qmp-ref.html).

To enable qmp on a virtual machine simply add this to the qemu command line:
```
-qmp unix:/tmp/qmp-my-vm.sock,server,nowait
```

Alternatively a tcp server can be exposed:
```
-qmp tcp:localhost:12345,server,nowait
```

Or via libvirt:
```xml
<domain xmlns:qemu="http://libvirt.org/schemas/domain/qemu/1.0" type="kvm">

...

  </devices>
  <qemu:commandline>
    <qemu:arg value="-qmp"/>
    <qemu:arg value="unix:/tmp/qmp-my-vm.sock,server,nowait"/>
  </qemu:commandline>
</domain>
...

Please refer to the qemu qmp manual for more information about how to configure this feature.

In case qmp is not active or could not be fetched, the connector falls back to hard-coded mapping tables for specific qemu machine types.

## Running Examples

Analog to the examples found in the main memflow repository examples can be run via:

```bash
RUST_SETPTRACE=1 cargo run --example read_phys --release
RUST_SETPTRACE=1 cargo run --example ps_win32 --release
RUST_SETPTRACE=1 cargo run --example ps_inventory --release
```

For more information about `RUST_SETPTRACE` and how to run examples see the [running-examples](https://github.com/memflow/memflow#running-examples) section in the main memflow repository. 

## License

Licensed under MIT License, see [LICENSE](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, shall be licensed as above, without any additional terms or conditions.
