#!/bin/bash

cargo build --release --all-features
if [[ ! -d ~/.local/lib/memflow ]]; then
    mkdir ~/.local/lib/memflow
fi
cp target/release/libmemflow_qemu_procfs.so ~/.local/lib/memflow
