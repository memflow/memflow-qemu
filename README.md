# memflow-qemu-procfs

This connector implements an interface for Qemu via the Process Filesystem on Linux.

## Compilation

### Using the library in a rust project

To use the plugin in a rust project just include it in your Cargo.toml

```
memflow-qemu-procfs = "0.1"
```

Make sure to not enable the `plugin` feature when importing multiple
connectors in a rust project without using the memflow plugin inventory.
This might cause duplicated exports being generated in your project.

### Installing the library

The stand-alone library can be installed with the `install.sh` script. It will place it inside `~/.local/lib/memflow` directory. Add `~/.local/lib` directory to `PATH` to use the connector in other memflow projects.

### Building the stand-alone plugin

Alternatively, you may want to compile the plugin manually. The plugin part is feature-gated behind the `plugin` feature.
To compile a dynamic library as a plugin use the following command:

```cargo build --release --all-features```

## License

Licensed under MIT License, see [LICENSE](LICENSE).

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, shall be licensed as above, without any additional terms or conditions.
