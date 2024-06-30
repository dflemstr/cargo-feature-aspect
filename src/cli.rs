#[derive(Debug, clap::Parser)]
#[command(bin_name = "cargo", styles = clap_cargo::style::CLAP_STYLING, subcommand_required = true)]
pub enum Command {
    FeatureAspect(FeatureAspectArgs),
}

/// Creates and updates feature aspects in a workspace.
///
/// A feature aspect is a feature that should generally exist for all crates in a workspace
/// that depend on some shared crate.  This shared crate's feature will be propagated to all
/// of its dependees.
///
/// For example, if a hypothetical `logging` crate has a `enable-tracing` feature, all crates
/// that depend on `logging` might want to have their own `enable-tracing` feature, that enables
/// additional tracing stuff in the local crate, and also enables the `enable-tracing` feature on
/// all dependency crates.
///
/// This command creates and updates such a feature aspect across the crate graph.
///
/// See the documentation in the repository for more usage examples:
/// https://github.com/dflemstr/cargo-feature-aspect
#[derive(Debug, clap::Args)]
pub struct FeatureAspectArgs {
    /// The name of the resulting feature aspect.
    ///
    /// This will be inferred from the name of the leaf feature if there's only one, but it might
    /// be useful to specify this flag if there are multiple leaf features.
    #[arg(short, long)]
    pub name: Option<String>,

    /// The leaf features to match/propagate, e.g. `enable-tracing` from the example above, or
    /// `logging/enable-tracing` to only match a specific crate.
    ///
    /// These are the features that identify "root crates" that should have their features spread to
    /// all dependee crates.
    #[arg(short = 'f', long = "leaf-feature")]
    pub leaf_features: Vec<String>,

    /// Add extra elements to the generated feature.  For example, it might be useful to add a
    /// `dep:logging` to all features that have the `enable-tracing` feature from the example above.
    #[arg(short, long = "add-feature-param")]
    pub add_feature_params: Vec<String>,

    /// Do not modify `Cargo.toml` files, instead print the changes that would be made.
    #[arg(short, long)]
    pub dry_run: bool,

    /// Do not modify `Cargo.toml` files, instead fail the command if changes would be made.
    #[arg(short, long)]
    pub verify: bool,

    /// Do not sort params the feature spec lexicographically.  If specified, new features are added
    /// to the end instead.
    ///
    /// For example, by default `myfeature = ["b/myfeature", "a/myfeature"]` will be changed to have
    /// `a` come before `b`, but this flag disables that behavior.
    #[arg(long)]
    pub no_sort: bool,

    #[command(flatten)]
    pub manifest: clap_cargo::Manifest,

    /// Run without accessing the network.
    #[arg(long)]
    pub offline: bool,

    /// Require `Cargo.toml` to be up-to-date.
    #[arg(long)]
    pub locked: bool,
}

#[test]
fn verify_cli() {
    use clap::CommandFactory as _;
    Command::command().debug_assert();
}
