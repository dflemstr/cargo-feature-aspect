#![warn(clippy::all, clippy::cargo)]
#![doc = include_str!("../README.md")]

use std::{borrow, cmp, fs, process};

mod cli;
mod context;
mod metadata;
mod output;
mod topo;

fn main() {
    tracing_subscriber::fmt::init();

    let result = {
        let command: cli::Command = clap::Parser::parse();
        match command {
            cli::Command::FeatureAspect(args) => run_feature_aspect(&args),
        }
    };

    if let Err(e) = result {
        tracing::error!("fatal error: {}", e);
        for reason in e.chain().skip(1) {
            tracing::error!("  {}", reason);
        }
        process::exit(1);
    }
}

fn run_feature_aspect(args: &cli::FeatureAspectArgs) -> anyhow::Result<()> {
    tracing::debug!("resolving workspace metadata");
    let metadata = metadata::resolve_ws(
        args.manifest.manifest_path.as_deref(),
        args.locked,
        args.offline,
    )?;
    tracing::debug!("enumerating workspace members");
    let mut packages = metadata::find_ws_members(metadata);
    tracing::debug!("doing topological sort of workspace members");
    topo::sort_packages(&mut packages)?;

    let mut ctx = context::Context::new(args)?;
    for package in &packages {
        visit_package(package, &mut ctx)?;
    }

    if ctx.verify && ctx.has_changes {
        anyhow::bail!("failing because --verify was passed and changes were detected");
    }

    Ok(())
}

#[tracing::instrument(skip_all, fields(package = package.name))]
fn visit_package<'a>(
    package: &'a cargo_metadata::Package,
    ctx: &mut context::Context<'a>,
) -> anyhow::Result<()> {
    let pkg_name = &package.name;
    let mut is_in_scope = false;
    let mut referenced_leaf_features = Vec::new();

    // Here we do lots of `Vec::contains` but since these are small vecs, it is not worth it
    // to do some fancy hash set stuff, since hashing all the strings will probably take more
    // time than just traversing the vec.

    for feature in package.features.keys() {
        if ctx.unqualified_leaf_features.contains(&feature.as_str())
            || ctx.qualified_leaf_features.contains(&(pkg_name, feature))
        {
            tracing::debug!(feature, "package has leaf feature");
            is_in_scope = true;

            if ctx.feature_name.as_ref() != feature.as_str() {
                // It might be the case that our main feature is named something totally different
                // from the leaf feature, which means that we should add the leaf feature as a
                // dependency for our main feature.
                referenced_leaf_features.push(feature.as_str());
            }
        }
    }

    for dependency in &package.dependencies {
        if ctx.in_scope_packages.contains(dependency.name.as_str()) {
            tracing::debug!(
                dependency = dependency.name,
                "package depends on in-scope dependency"
            );
            is_in_scope = true;
        }
    }

    if is_in_scope {
        tracing::debug!("package considered in scope for feature aspect; ensuring feature exists");
        ctx.in_scope_packages.insert(pkg_name);

        // Unfortunately at this point we cannot trust the `package.features` for diffing, because
        // some of the metadata features might be implicitly generated.  We will instead need to
        // check against the actual manifest file no matter what.
        visit_aspect_feature(package, ctx, &referenced_leaf_features)?;
    }

    Ok(())
}

fn visit_aspect_feature(
    package: &cargo_metadata::Package,
    ctx: &mut context::Context,
    referenced_leaf_features: &[&str],
) -> anyhow::Result<()> {
    let feature = ctx.feature_name.as_ref();
    let changes = describe_changes(ctx, package, referenced_leaf_features, feature);
    let has_changes = handle_feature_changes(ctx, package, feature, changes)?;
    ctx.has_changes |= has_changes;
    Ok(())
}

struct Changes<'a> {
    params_to_add: Vec<borrow::Cow<'a, str>>,
    params_to_remove: Vec<borrow::Cow<'a, str>>,
}

/// Generates the changes we would like to make to the feature aspect for a specific package.
fn describe_changes<'a>(
    ctx: &'a context::Context,
    package: &'a cargo_metadata::Package,
    referenced_leaf_features: &[&'a str],
    feature: &str,
) -> Changes<'a> {
    // Params to possibly add, however a check will be made later to remove duplicates
    let mut params_to_add: Vec<borrow::Cow<str>> = Vec::new();
    // Params to remove, however a check will be made later to see if they actually exist
    let mut params_to_remove: Vec<borrow::Cow<str>> = Vec::new();

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

            params_to_add.push(dep_spec_to_add.into());
            params_to_remove.push(dep_spec_to_remove.into());
        }
    }

    // Ensure extra params are present
    for &param in ctx
        .extra_feature_params
        .as_slice()
        .iter()
        .chain(referenced_leaf_features.iter())
    {
        let should_include = if let Some((prefix, suffix)) = param.split_once(':') {
            if prefix == "dep" {
                // Special-case: we only include dep references if the dep actually exists.
                // This would otherwise be very annoying to express with some sort of CLI flags,
                // so we just handle it by default.
                package.dependencies.iter().any(|d| d.name == suffix)
            } else {
                true
            }
        } else {
            true
        };

        if should_include {
            params_to_add.push(param.into());
        }
    }

    Changes {
        params_to_add,
        params_to_remove,
    }
}

/// Somewhat awkward function broken out from `visit_aspect_feature`
///
/// Returns true if actual changes compared to the physical manifest file were detected. We can't
/// borrow `ctx` by mutable reference here since it has multiple per-field borrows in
/// `visit_aspect_feature`, so we do the update to `has_changes` there instead.
fn handle_feature_changes(
    ctx: &context::Context,
    package: &cargo_metadata::Package,
    feature: &str,
    changes: Changes,
) -> anyhow::Result<bool> {
    // Awkward sorting functions because `.sort_by_key()` doesn't handle sort keys with
    // lifetimes nicely
    fn feature_param_sort_key(param: &str) -> (bool, &str) {
        if param.starts_with("dep:") {
            (false, param)
        } else {
            (true, param)
        }
    }

    fn feature_param_ordering(a: &str, b: &str) -> cmp::Ordering {
        feature_param_sort_key(a).cmp(&feature_param_sort_key(b))
    }

    // Here we do lots of `Vec::contains` but since these are small vecs, it is not worth it
    // to do some fancy hash set stuff, since hashing all the strings will probably take more
    // time than just traversing the vec.

    let pkg_name = &package.name;
    // TODO: is it clean to just make the file change here? Feels a bit dirty/hidden away...
    let contents = fs::read_to_string(&package.manifest_path)?;
    let mut has_changes = false;

    let Changes {
        mut params_to_add,
        mut params_to_remove,
    } = changes;

    if ctx.dry_run || ctx.verify {
        // We need to parse the actual manifest file to remove implicit features that get
        // auto-generated by cargo at runtime
        let doc: toml::Table = toml::from_str(&contents)?;

        let current_params = doc
            // [features]
            // ...
            .get("features")
            // [features]
            // my-feature = ...
            .and_then(|f| f.get(feature))
            // [features]
            // my-feature = [...]
            .and_then(|f| f.as_array())
            // [features]
            // my-feature = ["..."]
            .map(|values| values.iter().flat_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();

        params_to_add.retain(|p| !current_params.contains(&p.as_ref()));
        params_to_remove.retain(|p| current_params.contains(&p.as_ref()));

        // Describe changes that would be made
        if !params_to_add.is_empty() {
            params_to_add.sort_by(|a, b| feature_param_ordering(a.as_ref(), b.as_ref()));

            for param in &params_to_add {
                tracing::info!(?feature, ?param, "would add param");
                output::shell_status(
                    "Would add",
                    &format!(
                        "{param:?} to package {pkg_name} feature {feature:?}"
                    ),
                )?;
            }

            has_changes = true;
        }

        if !params_to_remove.is_empty() {
            params_to_remove.sort_by(|a, b| feature_param_ordering(a.as_ref(), b.as_ref()));

            for param in &params_to_remove {
                tracing::info!(?feature, ?param, "would remove param");
                output::shell_status(
                    "Would remove",
                    &format!(
                        "{param:?} from package {pkg_name} feature {feature:?}"
                    ),
                )?;
            }

            has_changes = true;
        }
    } else {
        let mut doc: toml_edit::DocumentMut = contents.parse()?;
        // Not in dry-run/verify mode, let's make the changes
        tracing::debug!(manifest_path=?package.manifest_path, "editing manifest file");

        let features = doc.entry("features")
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to edit manifest for package `{}`: the `features` field exists but is not a table!", package.name))?;
        let feature_arr = features.entry(feature.as_ref())
            .or_insert_with(|| toml_edit::Item::Value(toml_edit::Value::Array(toml_edit::Array::new())))
            .as_array_mut()
            .ok_or_else(|| anyhow::anyhow!("failed to edit manifest for package `{}`: `features.{}` exists but is not an array!", package.name, feature))?;

        params_to_add.retain(|param| {
            !feature_arr
                .iter()
                .any(|p| p.as_str() == Some(param.as_ref()))
        });

        // As opposed to the verify case, we store the indices of what to remove, to aid in making
        // edits as non-invasive as possible. If we don't, we might lose information like comments
        // for existing feature params. It's a bit dirty to have side-effects in `retain()`, I hope
        // you'll forgive me.
        let mut param_indices_to_remove = Vec::new();
        params_to_remove.retain(|param| {
            if let Some(idx) = feature_arr
                .iter()
                .position(|p| p.as_str() == Some(param.as_ref()))
            {
                param_indices_to_remove.push(idx);
                true
            } else {
                false
            }
        });

        if !(params_to_add.is_empty() && params_to_remove.is_empty()) {
            // If sorting the existing array is disabled, at least sort the new stuff we're adding.
            params_to_add.sort_by(|a, b| feature_param_ordering(a.as_ref(), b.as_ref()));
            params_to_remove.sort_by(|a, b| feature_param_ordering(a.as_ref(), b.as_ref()));

            for param in &params_to_add {
                output::shell_status(
                    "Adding",
                    &format!(
                        "{param:?} to package {pkg_name} feature {feature:?}"),
                )?;
            }

            for param in params_to_remove {
                output::shell_status(
                    "Removing",
                    &format!(
                        "{param:?} from package {pkg_name} feature {feature:?}"
                    ),
                )?;
            }

            // Now that we have logged what we're about to do, let's edit the actual TOML

            // Reverse sort indices to make it safe to remove them one by one from the array without
            // invalidating later indices
            param_indices_to_remove.sort_by(|a, b| b.cmp(a));

            for &idx in &param_indices_to_remove {
                // Remove in the actual TOML manifest
                feature_arr.remove(idx);
            }

            for param in params_to_add {
                feature_arr.push_formatted(toml_edit::Value::String(toml_edit::Formatted::new(
                    param.into_owned(),
                )));
            }

            if ctx.sort {
                feature_arr.sort_by(|a, b| {
                    feature_param_ordering(a.as_str().unwrap_or(""), b.as_str().unwrap_or(""))
                });
                feature_arr.fmt();
            }

            fs::write(&package.manifest_path, doc.to_string())?;
        }
    }

    Ok(has_changes)
}
