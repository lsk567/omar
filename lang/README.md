# OMAR language prototype

Build the Lean 4 compiler:

```sh
cd lang
lake build
```

Compile a program:

```sh
lake exe omarc ../tests/topology/HR.omar /tmp/HR.bytecode.json
```

Verify the bytecode with the Rust VM without spawning agents:

```sh
cargo run -- topology apply /tmp/HR.bytecode.json --dry-run
```

Remove `--dry-run` to spawn the declared agents and persist the installed
topology under the selected EA's state directory. The initial prototype only
constructs a topology; event delivery and topology mutation are not implemented.
