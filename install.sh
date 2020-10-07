#!/bin/bash

cargo build --release --all-features

if [ ! -z "$1" ] && [ $1 = "--system" ]; then
    if [[ ! -d /usr/lib/memflow ]]; then
        sudo mkdir /usr/lib/memflow
    fi
    sudo cp target/release/libmemflow_qemu_procfs.so /usr/lib/memflow
else
    if [[ ! -d ~/.local/lib/memflow ]]; then
        mkdir -p ~/.local/lib/memflow
    fi
    cp target/release/libmemflow_qemu_procfs.so ~/.local/lib/memflow
fi
