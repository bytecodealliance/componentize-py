use {
    anyhow::{Context, Result, bail, ensure},
    futures::TryStreamExt as _,
    std::{collections::HashSet, path::Path},
    wasm_pkg_client::{Client, Config, PackageRef, Version},
    wit_component::DecodedWasm,
    wit_parser::{PackageId, ParsedUsePath, Resolve, WorldId, parse_use_path},
};

#[derive(Debug)]
struct RemoteWorld<'a> {
    requested: &'a str,
    package: PackageRef,
    version: Version,
    world: String,
}

pub(crate) fn is_qualified_world_reference(world: &str) -> bool {
    matches!(parse_use_path(world), Ok(ParsedUsePath::Package(_, _)))
        || world
            .split_once('/')
            .is_some_and(|(package, _)| package.contains(':'))
}

fn parse_remote_world(world: &str) -> Result<Option<RemoteWorld<'_>>> {
    let parsed = parse_use_path(world)
        .with_context(|| format!("failed to parse world specifier `{world}`"))?;
    let ParsedUsePath::Package(package, world_name) = parsed else {
        return Ok(None);
    };
    let Some(version) = package.version else {
        bail!("remote world reference `{world}` must include an exact version");
    };
    let package_ref = format!("{}:{}", package.namespace, package.name);

    Ok(Some(RemoteWorld {
        requested: world,
        package: package_ref
            .parse()
            .with_context(|| format!("invalid package in remote world reference `{world}`"))?,
        version,
        world: world_name,
    }))
}

fn has_top_level_package(
    resolve: &Resolve,
    packages: &[(&Path, PackageId)],
    remote: &RemoteWorld<'_>,
) -> bool {
    packages.iter().any(|(_, package)| {
        let package = &resolve.packages[*package];
        package.name.namespace == remote.package.namespace().to_string()
            && package.name.name == remote.package.name().to_string()
            && package.name.version.as_ref() == Some(&remote.version)
    })
}

async fn load_config(path: Option<&Path>, default_registries: bool) -> Result<Config> {
    let mut config = if default_registries {
        Config::global_defaults()
            .await
            .context("failed to load default registry configuration")?
    } else {
        Config::empty()
    };
    if let Some(path) = path {
        config.merge(Config::from_file(path).await.with_context(|| {
            format!("failed to load registry configuration `{}`", path.display())
        })?);
    }
    Ok(config)
}

async fn fetch_package(client: &Client, remote: &RemoteWorld<'_>) -> Result<Resolve> {
    let registry = client.config().resolve_registry(&remote.package).cloned();
    let source = registry
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "<unconfigured>".to_owned());
    let context = || {
        format!(
            "failed to resolve remote world `{}` from package `{}@{}` using registry `{source}`",
            remote.requested, remote.package, remote.version
        )
    };

    let release = client
        .get_release(&remote.package, &remote.version)
        .await
        .with_context(context)?;
    let mut stream = client
        .stream_content(&remote.package, &release)
        .await
        .with_context(context)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.try_next().await.with_context(context)? {
        bytes.extend_from_slice(&chunk);
    }

    let decoded = wit_component::decode(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "{}: registry content is not a binary WIT package: {error:#}",
            context()
        )
    })?;
    let DecodedWasm::WitPackage(resolve, package) = decoded else {
        bail!(
            "{}: registry content is not a binary WIT package",
            context()
        );
    };
    let actual = &resolve.packages[package].name;
    ensure!(
        actual.namespace == remote.package.namespace().to_string()
            && actual.name == remote.package.name().to_string()
            && actual.version.as_ref() == Some(&remote.version),
        "{}: registry content declares package `{actual}`",
        context()
    );
    Ok(resolve)
}

pub(crate) async fn resolve_requested_worlds(
    resolve: &mut Resolve,
    packages: &[(&Path, PackageId)],
    worlds: &[&str],
    registry_config: Option<&Path>,
    default_registries: bool,
) -> Result<Vec<WorldId>> {
    enum RequestedWorld<'a> {
        Local(WorldId),
        Remote(RemoteWorld<'a>),
    }

    let mut requested = Vec::with_capacity(worlds.len());
    for &world in worlds {
        match super::select_world(resolve, Some(world), packages) {
            Ok(world) => requested.push(RequestedWorld::Local(world)),
            Err(local_error) => match parse_remote_world(world)? {
                Some(remote) => {
                    if has_top_level_package(resolve, packages, &remote) {
                        bail!(
                            "local package `{}@{}` does not contain world `{}` requested by `{}`",
                            remote.package,
                            remote.version,
                            remote.world,
                            remote.requested
                        );
                    }
                    requested.push(RequestedWorld::Remote(remote));
                }
                None => return Err(local_error),
            },
        }
    }

    let mut client = None;
    let mut resolved = Vec::with_capacity(requested.len());
    let mut fetched_main_packages = HashSet::new();

    for requested in requested {
        let remote = match requested {
            RequestedWorld::Local(world) => {
                resolved.push(world);
                continue;
            }
            RequestedWorld::Remote(remote) => remote,
        };
        if let Ok(world) = super::select_world(resolve, Some(remote.requested), packages) {
            resolved.push(world);
            continue;
        }
        let package_key = (remote.package.clone(), remote.version.clone());
        if fetched_main_packages.contains(&package_key) {
            bail!(
                "registry package `{}@{}` does not contain world `{}` requested by `{}`",
                remote.package,
                remote.version,
                remote.world,
                remote.requested
            );
        }

        if client.is_none() {
            client = Some(Client::new(
                load_config(registry_config, default_registries).await?,
            ));
        }
        let client = client.as_ref().unwrap();
        let downloaded = fetch_package(client, &remote).await?;
        resolve.merge(downloaded).with_context(|| {
            format!(
                "failed to merge registry package `{}@{}` for `{}`",
                remote.package, remote.version, remote.requested
            )
        })?;
        fetched_main_packages.insert(package_key);
        resolved.push(
            super::select_world(resolve, Some(remote.requested), packages).with_context(|| {
                format!(
                    "registry package `{}@{}` does not contain world `{}` requested by `{}`",
                    remote.package, remote.version, remote.world, remote.requested
                )
            })?,
        );
    }
    Ok(resolved)
}
