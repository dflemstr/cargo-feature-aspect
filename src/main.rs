#![warn(clippy::all, clippy::cargo)]
#![doc = include_str!("../README.md")]

use std::path;
use std::{borrow, collections, fs};

#[derive(Debug, clap::Parser)]
#[command(bin_name = "cargo", styles = clap_cargo::style::CLAP_STYLING, subcommand_required = true)]
enum Command {
    FeatureAspect(FeatureAspectArgs),
}

/// Creates and updates aspect features in a workspace.
///
/// An aspect feature is a feature that should generally exist for all crates in a workspace
/// that depend on some shared crate.  This shared crate's feature will be propagated to all
/// of its dependees.
///
/// For example, if a hypothetical `logging` crate has a `enable-tracing` feature, all crates
/// that depend on `logging` might want to have their own `enable-tracing` feature, that enables
/// additional tracing stuff in the local crate, and also enables the `logging` feature on all
/// dependency crates.
///
/// This command creates and updates such an aspect feature across the crate graph.
///
/// Example usages:
///
/// ```shell
/// # Any crate that indirectly depends on the `logging` crate should have a feature
/// # `enable-tracing` that is propagated through all dependency crates.
/// cargo feature-aspect --name enable-tracing --leaf-feature logging/enable-tracing
///
/// # Same as above, but `--name` is inferred from `--leaf-feature`.
/// cargo feature-aspect --leaf-feature logging/enable-tracing
///
/// # Any such crate should also enable the `logging` optional dependency.
/// cargo feature-aspect --leaf-feature logging/enable-tracing --add-feature-param dep:logging
///
/// # Do not re-order the `enable-tracing` feature param array when it is not already in
/// # alphabetical order.
/// cargo feature-aspect --leaf-feature logging/enable-tracing --no-sort
///
/// # Dry-run to see what changes would be made
/// cargo feature-aspect --leaf-feature logging/enable-tracing --dry-run
///
/// # Verify that the aspect feature is up-to-date (useful for CI)
/// cargo feature-aspect --leaf-feature logging/enable-tracing --verify
/// ```
#[derive(Debug, clap::Args)]
struct FeatureAspectArgs {
    /// The name of the resulting aspect feature.
    ///
    /// This will be inferred from the name of the leaf feature if there's only one, but it might
    /// be useful to specify this flag if there are multiple leaf features.
    #[arg(short, long)]
    name: Option<String>,

    /// The leaf features to match/propagate, e.g. `enable-tracing` from the example above, or
    /// `logging/enable-tracing` to only match a specific crate.
    ///
    /// These are the features that identify "root crates" that should have their features spread to
    /// all dependee crates.
    #[arg(short = 'f', long = "leaf-feature")]
    leaf_features: Vec<String>,

    /// Add extra elements to the generated feature.  For example, it might be useful to add a
    /// `dep:logging` to all features that have the `enable-tracing` feature from the example above.
    #[arg(short, long = "add-feature-param")]
    add_feature_params: Vec<String>,

    /// Do not modify `Cargo.toml` files, instead print the changes that would be made.
    #[arg(short, long)]
    dry_run: bool,

    /// Do not modify `Cargo.toml` files, instead fail the command if changes would be made.
    #[arg(short, long)]
    verify: bool,

    /// Do not sort params the feature spec lexicographically.  If specified, new features are added
    /// to the end instead.
    ///
    /// For example, by default `myfeature = ["b/myfeature", "a/myfeature"]` will be changed to have
    /// `a` come before `b`, but this flag disables that behavior.
    #[arg(long)]
    no_sort: bool,

    /// Provide the path to the `Cargo.toml` to start with.
    ///
    /// This arg mainly exists to maintain compatibility with the standard `cargo` flag of the same
    /// name.
    #[arg(long)]
    manifest_path: Option<path::PathBuf>,

    /// Run without accessing the network.
    ///
    /// This arg mainly exists to maintain compatibility with the standard `cargo` flag of the same
    /// name.
    #[arg(long)]
    offline: bool,

    /// Require `Cargo.toml` to be up-to-date.
    ///
    /// This arg mainly exists to maintain compatibility with the standard `cargo` flag of the same
    /// name.
    #[arg(long)]
    locked: bool,
}

struct Context<'a> {
    feature_name: borrow::Cow<'a, str>,
    extra_feature_params: Vec<&'a str>,
    dry_run: bool,
    verify: bool,
    sort: bool,
    has_changes: bool,
    unqualified_leaf_features: Vec<&'a str>,
    qualified_leaf_features: Vec<(&'a str, &'a str)>,
    in_scope_packages: collections::HashSet<&'a str>,
}

fn main() -> anyhow::Result<()> {
    let command: Command = clap::Parser::parse();

    match command {
        Command::FeatureAspect(args) => run_feature_aspect(&args),
    }
}

fn run_feature_aspect(args: &FeatureAspectArgs) -> anyhow::Result<()> {
    let metadata = resolve_ws(args.manifest_path.as_deref(), args.locked, args.offline)?;
    let mut packages = find_ws_members(metadata);
    reverse_topo_sort(&mut packages)?;

    let mut ctx = create_context(args)?;
    for package in &packages {
        visit_package(package, &mut ctx)?;
    }

    if ctx.verify && ctx.has_changes {
        anyhow::bail!("failing because --verify was passed and changes were detected");
    }

    Ok(())
}

fn create_context(args: &FeatureAspectArgs) -> anyhow::Result<Context> {
    let feature_name = if let Some(name) = &args.name {
        name.into()
    } else if let &[name] = &args.leaf_features.as_slice() {
        // We have exactly one leaf feature, see if it is scoped by package
        if let Some((_, feature)) = name.split_once('/') {
            feature.into()
        } else {
            name.into()
        }
    } else {
        anyhow::bail!("Must specify exactly one --leaf-feature or else specify --name")
    };

    let extra_feature_params = args.add_feature_params.iter().map(String::as_str).collect();
    let dry_run = args.dry_run;
    let verify = args.verify;
    let sort = !args.no_sort;
    let has_changes = false;

    // We expect these to be tiny, so it's overkill to use a hash data structure
    let mut unqualified_leaf_features = Vec::new();
    let mut qualified_leaf_features = Vec::new();

    for leaf_feature in &args.leaf_features {
        if let Some((pkg, feature)) = leaf_feature.split_once('/') {
            if !qualified_leaf_features.contains(&(pkg, feature)) {
                qualified_leaf_features.push((pkg, feature));
            }
        } else {
            let feature = leaf_feature.as_str();
            if !unqualified_leaf_features.contains(&feature) {
                unqualified_leaf_features.push(feature);
            }
        }
    }

    // This might have relatively many elems so might make sense to hash values here
    let in_scope_packages = collections::HashSet::new();

    let context = Context {
        feature_name,
        extra_feature_params,
        dry_run,
        verify,
        sort,
        has_changes,
        unqualified_leaf_features,
        qualified_leaf_features,
        in_scope_packages,
    };

    Ok(context)
}

fn reverse_topo_sort(packages: &mut [cargo_metadata::Package]) -> anyhow::Result<()> {
    const DEP_CYCLE_ERR: &str = "Dependency cycle detected! Resolve this using other cargo commands first (e.g. `cargo build` should fail with a decent error message).";

    let packages_len = packages.len();

    let mut topo = topo_sort::TopoSort::with_capacity(packages.len());
    for package in packages.iter() {
        topo.insert(
            package.name.as_str(),
            package.dependencies.iter().map(|d| d.name.as_str()),
        );
    }

    // Contains a mapping of package name to topological sort index
    let order: collections::HashMap<String, usize> = topo
        .nodes()
        .enumerate()
        // Sort in reverse order with `packages_len - idx` so that dependencies are before dependees
        .map(|(idx, node_result)| Ok(((*node_result?).to_string(), packages_len - idx)))
        .collect::<anyhow::Result<_>>()
        .map_err(|_| anyhow::anyhow!(DEP_CYCLE_ERR))?;

    packages.sort_by_key(|p| order[p.name.as_str()]);

    Ok(())
}

fn visit_package<'a>(
    package: &'a cargo_metadata::Package,
    ctx: &mut Context<'a>,
) -> anyhow::Result<()> {
    let pkg_name = &package.name;
    let mut is_in_scope = false;
    let mut extra_leaf_features = Vec::new();

    for feature in package.features.keys() {
        if ctx.unqualified_leaf_features.contains(&feature.as_str())
            || ctx.qualified_leaf_features.contains(&(pkg_name, feature))
        {
            is_in_scope = true;
            extra_leaf_features.push(feature.as_str());
        }
    }

    for dependency in &package.dependencies {
        if ctx.in_scope_packages.contains(dependency.name.as_str()) {
            is_in_scope = true;
        }
    }

    if is_in_scope {
        ctx.in_scope_packages.insert(pkg_name);

        if let Some(params) = package.features.get(ctx.feature_name.as_ref()) {
            let params = params.iter().map(String::as_str).collect::<Vec<_>>();
            visit_aspect_feature(package, ctx, &params, &extra_leaf_features)?;
        } else {
            visit_aspect_feature(package, ctx, &[], &extra_leaf_features)?;
        }
    }

    Ok(())
}

fn visit_aspect_feature(
    package: &cargo_metadata::Package,
    ctx: &mut Context,
    feature_params: &[&str],
    extra_leaf_features: &[&str],
) -> anyhow::Result<()> {
    let pkg_name = &package.name;
    let feature = &ctx.feature_name;

    // This is our feature, let's edit it.
    let mut params_to_add: Vec<borrow::Cow<str>> = Vec::new();
    let mut param_indices_to_remove = Vec::new();

    // Here we do lots of `Vec::contains` but since these are small vecs, it is not worth it
    // to do some fancy hash set stuff, since hasing all the strings will probably take more
    // time than just traversing the vec.

    // Ensure that we propagate the feature to our dependencies.
    for dep in &package.dependencies {
        if ctx.in_scope_packages.contains(dep.name.as_str()) {
            let non_optional_dep_spec = format!("{}/{}", dep.name, feature);
            let optional_dep_spec = format!("{}?/{}", dep.name, feature);

            // Gracefully handle when a dependency might have changed its "optional status" from
            // previous runs.
            let (dep_spec_to_add, dep_spec_to_remove) = if dep.optional {
                (optional_dep_spec, non_optional_dep_spec)
            } else {
                (non_optional_dep_spec, optional_dep_spec)
            };

            if !feature_params.contains(&dep_spec_to_add.as_str()) {
                params_to_add.push(dep_spec_to_add.into());
            }
            if let Some(idx) = feature_params.iter().position(|p| p == &dep_spec_to_remove) {
                param_indices_to_remove.push(idx);
            }
        }
    }

    // Ensure extra params are present
    for &param in ctx
        .extra_feature_params
        .as_slice()
        .iter()
        .chain(extra_leaf_features.iter())
    {
        if !feature_params.contains(&param) && !params_to_add.iter().any(|s| s == param) {
            params_to_add.push(param.into());
        }
    }

    if ctx.dry_run || ctx.verify {
        // Describe changes that would be made
        if !params_to_add.is_empty() {
            for param in params_to_add {
                eprintln!("package `{pkg_name}` feature `{feature}`: would add `{param}`");
                ctx.has_changes = true;
            }
        }
        if !param_indices_to_remove.is_empty() {
            for idx in param_indices_to_remove {
                let param = &feature_params[idx];
                eprintln!("package `{pkg_name}` feature `{feature}`: would remove `{param}`");
                ctx.has_changes = true;
            }
        }
    } else {
        // Not in dry-run/verify mode, let's make the changes

        // TODO: is it clean to just make the file change here? Feels a bit dirty/hidden away...
        let contents = fs::read_to_string(&package.manifest_path)?;
        let mut doc: toml_edit::DocumentMut = contents.parse()?;

        let features = doc.entry("features").or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new())).as_table_mut().ok_or_else(||anyhow::anyhow!("failed to edit manifest for package `{}`: the `features` field exists but is not a table!", package.name))?;
        let feature_arr = features.entry(feature.as_ref()).or_insert_with(|| toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new())))
            .as_array_mut().ok_or_else(|| anyhow::anyhow!("failed to edit manifest for package `{}`: `features.{}` exists but is not an array!", package.name, feature))?;

        // Reverse sort indices to make it safe to remove them one by one from a Vec
        param_indices_to_remove.sort_by(|a, b| b.cmp(a));

        // If sorting the existing array is disabled, at least sort the new stuff we're adding
        if !ctx.sort {
            params_to_add.sort();
        }

        for idx in param_indices_to_remove {
            feature_arr.remove(idx);
        }

        for param in params_to_add {
            feature_arr.push(param.as_ref());
        }

        if ctx.sort {
            feature_arr.sort_by_key(|v| v.as_str().map(ToOwned::to_owned));
        }
    }

    Ok(())
}

fn resolve_ws(
    manifest_path: Option<&path::Path>,
    locked: bool,
    offline: bool,
) -> anyhow::Result<cargo_metadata::Metadata> {
    let mut cmd = cargo_metadata::MetadataCommand::new();
    if let Some(manifest_path) = manifest_path {
        cmd.manifest_path(manifest_path);
    }
    cmd.features(cargo_metadata::CargoOpt::AllFeatures);
    let mut other = Vec::new();
    if locked {
        other.push("--locked".to_owned());
    }
    if offline {
        other.push("--offline".to_owned());
    }
    cmd.other_options(other);

    let ws = cmd.exec().or_else(|_| {
        cmd.no_deps();
        cmd.exec()
    })?;
    Ok(ws)
}

fn find_ws_members(ws: cargo_metadata::Metadata) -> Vec<cargo_metadata::Package> {
    let workspace_members: collections::HashSet<_> = ws.workspace_members.iter().collect();
    ws.packages
        .into_iter()
        .filter(|p| workspace_members.contains(&p.id))
        .collect()
}

#[test]
fn verify_cli() {
    use clap::CommandFactory as _;
    Command::command().debug_assert();
}
