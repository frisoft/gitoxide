use std::{convert::TryInto, process::Command};

use anyhow::{anyhow, bail};
use bstr::ByteSlice;
use cargo_metadata::{
    camino::{Utf8Component, Utf8Path},
    Package,
};
use git_repository::{easy::object, prelude::ReferenceAccessExt, refs, refs::transaction::PreviousValue};

use super::{tag_name_for, utils::will, Oid, Options};

fn is_top_level_package(manifest_path: &Utf8Path, shared: &git_repository::Easy) -> bool {
    manifest_path
        .strip_prefix(shared.repo.work_tree.as_ref().expect("repo with working tree"))
        .map_or(false, |p| p.components().count() == 1)
}

pub(in crate::command::release_impl) fn has_changed_since_last_release(
    package: &Package,
    ctx: &crate::Context,
    verbose: bool,
) -> anyhow::Result<bool> {
    let version_tag_name = tag_name_for(
        &package.name,
        &package.version.to_string(),
        is_top_level_package(&package.manifest_path, &ctx.git_easy),
    );
    let mut tag_ref = match ctx.git_easy.try_find_reference(&version_tag_name)? {
        None => {
            if verbose {
                log::info!(
                    "Package {} wasn't tagged with {} yet and thus needs a release",
                    package.name,
                    version_tag_name
                );
            }
            return Ok(true);
        }
        Some(r) => r,
    };
    let repo_relative_crate_dir = package
        .manifest_path
        .parent()
        .expect("parent of a file is always present")
        .strip_prefix(&ctx.root)
        .expect("workspace members are releative to the root directory");

    let current_commit = ctx.git_easy.find_reference("HEAD")?.peel_to_id_in_place()?;
    let released_target = tag_ref.peel_to_id_in_place()?;

    if repo_relative_crate_dir.as_os_str().is_empty() {
        Ok(current_commit != released_target)
    } else {
        let components = repo_relative_crate_dir.components().map(|c| match c {
            Utf8Component::Normal(c) => c.as_bytes(),
            _ => unreachable!("only normal components are possible in paths here"),
        });
        let current_dir_id = current_commit
            .object()?
            .peel_to_kind(object::Kind::Tree)?
            .into_tree()
            .lookup_path(components.clone())?
            .expect("path must exist in current commit")
            .oid;
        let released_dir_id = released_target
            .object()?
            .peel_to_kind(object::Kind::Tree)?
            .into_tree()
            .lookup_path(components)?
            .expect("path must exist as it was supposedly released there")
            .oid;

        Ok(released_dir_id != current_dir_id)
    }
}

pub fn assure_clean_working_tree() -> anyhow::Result<()> {
    let tracked_changed = !Command::new("git")
        .arg("diff")
        .arg("HEAD")
        .arg("--exit-code")
        .arg("--name-only")
        .status()?
        .success();
    if tracked_changed {
        bail!("Detected working tree changes. Please commit beforehand as otherwise these would be committed as part of manifest changes, or use --allow-dirty to force it.")
    }

    let untracked = Command::new("git")
        .arg("ls-files")
        .arg("--exclude-standard")
        .arg("--others")
        .output()?
        .stdout;
    if !untracked.trim().is_empty() {
        let err = anyhow!(bstr::BString::from(untracked));
        return Err(err.context("Found untracked files which would possibly be packaged when publishing."));
    }
    Ok(())
}

pub(in crate::command::release_impl) fn commit_changes(
    message: impl AsRef<str>,
    verbose: bool,
    dry_run: bool,
    empty_commit_possible: bool,
    ctx: &crate::Context,
) -> anyhow::Result<Option<Oid<'_>>> {
    // TODO: replace with gitoxide one day
    let mut cmd = Command::new("git");
    cmd.arg("commit").arg("-am").arg(message.as_ref());
    if empty_commit_possible {
        cmd.arg("--allow-empty");
    }
    if verbose {
        log::info!("{} run {:?}", will(dry_run), cmd);
    }
    if dry_run {
        return Ok(None);
    }

    if !cmd.status()?.success() {
        bail!("Failed to commit changed manifests");
    }
    Ok(Some(ctx.git_easy.find_reference("HEAD")?.peel_to_id_in_place()?))
}

pub(in crate::command::release_impl) fn create_version_tag<'repo>(
    publishee: &Package,
    new_version: &str,
    commit_id: Option<Oid<'repo>>,
    ctx: &'repo crate::Context,
    Options {
        verbose,
        dry_run,
        skip_tag,
        ..
    }: Options,
) -> anyhow::Result<Option<refs::FullName>> {
    if skip_tag {
        return Ok(None);
    }
    let tag_name = tag_name_for(
        &publishee.name,
        new_version,
        is_top_level_package(&publishee.manifest_path, &ctx.git_easy),
    );
    if dry_run {
        if verbose {
            log::info!("WOULD create tag {}", tag_name);
        }
        Ok(Some(format!("refs/tags/{}", tag_name).try_into()?))
    } else {
        let tag = ctx
            .git_easy
            .tag(tag_name, commit_id.expect("set in --execute mode"), PreviousValue::Any)?;
        log::info!("Created tag {}", tag.name().as_bstr());
        Ok(Some(tag.inner.name))
    }
}

// TODO: Make this gitoxide
pub fn push_tags_and_head(tag_names: impl IntoIterator<Item = refs::FullName>, options: Options) -> anyhow::Result<()> {
    if options.skip_push {
        return Ok(());
    }

    let mut cmd = Command::new("git");
    cmd.arg("push").arg("origin").arg("HEAD");
    for tag_name in tag_names {
        cmd.arg(tag_name.as_bstr().to_str()?);
    }

    if options.verbose {
        log::info!("{} run {:?}", will(options.dry_run), cmd);
    }
    if options.dry_run || cmd.status()?.success() {
        Ok(())
    } else {
        bail!("'git push' invocation failed. Try to push manually and repeat the smart-release invocation to resume, possibly with --skip-push.");
    }
}
