# `cargo feature-aspect`

A Cargo plugin that creates and updates feature aspects across a Cargo workspace.

## Installation

Simply install using `cargo` with `cargo install cargo-feature-aspect`.

## Details

A feature aspect is a feature that should generally exist for all crates in a workspace
that depend on some shared crate.  This shared crate's feature will be propagated to all
of its dependees.

For example, if a hypothetical `logging` crate has a `enable-tracing` feature, all crates
that depend on `logging` might want to have their own `enable-tracing` feature, that enables
additional tracing stuff in the local crate, and also enables the `enable-tracing` feature on
all dependency crates.  In other words, we want to have the crates that depend directly on
`logging` to have a feature like so:

```
[package]
name = "foo"

[dependencies]
logging = "..."

[features]
enable-tracing = ["logging/enable-tracing"]
```

...and second order dependees would get a matching feature that propagates this feature down
the chain:

```
[package]
name = "bar"

[dependencies]
foo = "..."

[features]
enable-tracing = ["foo/enable-tracing"]
```

This command creates and updates such a feature aspect across the crate graph.

Example usages:

```shell
# Any crate that indirectly depends on the `logging` crate should have a feature
# `enable-tracing` that is propagated through all dependency crates.
cargo feature-aspect --name enable-tracing --leaf-feature logging/enable-tracing

# Same as above, but `--name` is inferred from `--leaf-feature`.
cargo feature-aspect --leaf-feature logging/enable-tracing

# Any such crate should also enable the `logging` optional dependency.
cargo feature-aspect --leaf-feature logging/enable-tracing --add-feature-param dep:logging

# Do not re-order the `enable-tracing` feature param array when it is not already in
# alphabetical order.
cargo feature-aspect --leaf-feature logging/enable-tracing --no-sort

# Dry-run to see what changes would be made
cargo feature-aspect --leaf-feature logging/enable-tracing --dry-run

# Verify that the feature aspect is up-to-date (useful for CI)
cargo feature-aspect --leaf-feature logging/enable-tracing --verify
```

# Attribution

Some code in this crate was copied from `cargo-edit` which is
Copyright (c) 2015 Without Boats, Pascal Hertleif.
