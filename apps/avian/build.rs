use std::{env, path::PathBuf, process::Command};

fn git(root: &std::path::Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn valid_version(version: &str) -> bool {
    let (core, prerelease) = version
        .split_once('-')
        .map_or((version, None), |(core, suffix)| (core, Some(suffix)));
    let components = core.split('.').collect::<Vec<_>>();
    if components.len() != 3
        || components.iter().any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
        })
    {
        return false;
    }
    prerelease.is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix.split('.').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && !(part.len() > 1
                        && part.starts_with('0')
                        && part.bytes().all(|byte| byte.is_ascii_digit()))
            })
    })
}

fn main() {
    println!("cargo:rerun-if-env-changed=AVIAN_RELEASE");
    let root =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("package directory")).join("../..");
    // Git metadata changes independently of Rust sources. Track both symbolic and packed refs.
    if let Some(directory) = git(&root, &["rev-parse", "--absolute-git-dir"]) {
        for filename in ["HEAD", "index", "packed-refs", "refs/heads", "refs/tags"] {
            println!("cargo:rerun-if-changed={directory}/{filename}");
        }
    }
    // Also notice edits in other workspace components that affect build identity.
    for directory in [
        "src",
        "../../crates",
        "../../tools",
        "../../qualification",
        "../../README.md",
        "../../LICENSE.md",
        "../../THIRD_PARTY_NOTICES.md",
        "../../.gitlab-ci.yml",
        "../../Cargo.toml",
        "../../Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={directory}");
    }
    let package_version = env::var("CARGO_PKG_VERSION").expect("package version");
    let commit = git(&root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(
        &root,
        &["status", "--porcelain", "--untracked-files=normal"],
    )
    .is_none_or(|status| !status.is_empty());
    let short_commit = commit.get(..12).unwrap_or(&commit);
    let release = env::var("AVIAN_RELEASE").unwrap_or_default();
    let build_version = if release.is_empty() {
        format!(
            "{package_version}-dev.{short_commit}{}",
            if dirty { ".dirty" } else { "" }
        )
    } else {
        let version = release
            .strip_prefix("avian-v")
            .expect("AVIAN_RELEASE must start with avian-v");
        assert!(
            valid_version(version),
            "AVIAN_RELEASE must contain a valid semantic version"
        );
        assert_eq!(
            version.split('-').next(),
            package_version.split('-').next(),
            "AVIAN_RELEASE must match the package major.minor.patch version"
        );
        format!(
            "{version} ({release}, {short_commit}{})",
            if dirty { ", dirty" } else { "" }
        )
    };
    println!("cargo:rustc-env=AVIAN_BUILD_VERSION={build_version}");
    println!("cargo:rustc-env=AVIAN_RELEASE_TAG={release}");
    println!("cargo:rustc-env=AVIAN_BUILD_SHA={commit}");
    println!("cargo:rustc-env=AVIAN_BUILD_DIRTY={dirty}");
}

#[cfg(test)]
#[path = "../../tests/version.rs"]
mod tests;
