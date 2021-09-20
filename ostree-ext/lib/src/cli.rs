//! # Commandline parsing
//!
//! While there is a separate `ostree-ext-cli` crate that
//! can be installed and used directly, the CLI code is
//! also exported as a library too, so that projects
//! such as `rpm-ostree` can directly reuse it.

use anyhow::Result;
use ostree::gio;
use std::collections::BTreeMap;
use std::convert::TryInto;
use std::ffi::OsString;
use structopt::StructOpt;

use crate::container::{Config, ImportOptions};

#[derive(Debug, StructOpt)]
struct BuildOpts {
    #[structopt(long)]
    repo: String,

    #[structopt(long = "ref")]
    ostree_ref: String,

    #[structopt(long)]
    oci_dir: String,
}

/// Options for importing a tar archive.
#[derive(Debug, StructOpt)]
struct ImportOpts {
    /// Path to the repository
    #[structopt(long)]
    repo: String,

    /// Path to a tar archive; if unspecified, will be stdin.  Currently the tar archive must not be compressed.
    path: Option<String>,
}

/// Options for exporting a tar archive.
#[derive(Debug, StructOpt)]
struct ExportOpts {
    /// Path to the repository
    #[structopt(long)]
    repo: String,

    /// The ostree ref or commit to export
    rev: String,
}

/// Options for import/export to tar archives.
#[derive(Debug, StructOpt)]
enum TarOpts {
    /// Import a tar archive (currently, must not be compressed)
    Import(ImportOpts),

    /// Write a tar archive to stdout
    Export(ExportOpts),
}

/// Options for container import/export.
#[derive(Debug, StructOpt)]
enum ContainerOpts {
    /// Import an ostree commit embedded in a remote container image
    Import {
        /// Path to the repository
        #[structopt(long)]
        repo: String,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        imgref: String,

        /// Create an ostree ref pointing to the imported commit
        #[structopt(long)]
        write_ref: Option<String>,
    },

    /// Print information about an exported ostree-container image.
    Info {
        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        imgref: String,
    },

    /// Export an ostree commit to an OCI layout
    Export {
        /// Path to the repository
        #[structopt(long)]
        repo: String,

        /// The ostree ref or commit to export
        rev: String,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        imgref: String,

        /// Additional labels for the container
        #[structopt(name = "label", long, short)]
        labels: Vec<String>,

        /// Corresponds to the Dockerfile `CMD` instruction.
        #[structopt(long)]
        cmd: Option<Vec<String>>,
    },
}

/// Options for the Integrity Measurement Architecture (IMA).
#[derive(Debug, StructOpt)]
struct ImaSignOpts {
    /// Path to the repository
    #[structopt(long)]
    repo: String,
    /// The ostree ref or commit to use as a base
    src_rev: String,
    /// The ostree ref to use for writing the signed commit
    target_ref: String,

    /// Digest algorithm
    algorithm: String,
    /// Path to IMA key
    key: String,
}

/// Toplevel options for extended ostree functionality.
#[derive(Debug, StructOpt)]
#[structopt(name = "ostree-ext")]
#[structopt(rename_all = "kebab-case")]
enum Opt {
    /// Import and export to tar
    Tar(TarOpts),
    /// Import and export to a container image
    Container(ContainerOpts),
    /// IMA signatures
    ImaSign(ImaSignOpts),
}

/// Import a tar archive containing an ostree commit.
async fn tar_import(opts: &ImportOpts) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, opts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    let imported = if let Some(path) = opts.path.as_ref() {
        let instream = tokio::fs::File::open(path).await?;
        crate::tar::import_tar(repo, instream, None).await?
    } else {
        let stdin = tokio::io::stdin();
        crate::tar::import_tar(repo, stdin, None).await?
    };
    println!("Imported: {}", imported);
    Ok(())
}

/// Export a tar archive containing an ostree commit.
fn tar_export(opts: &ExportOpts) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, opts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    crate::tar::export_commit(repo, opts.rev.as_str(), std::io::stdout())?;
    Ok(())
}

/// Import a container image with an encapsulated ostree commit.
async fn container_import(repo: &str, imgref: &str, write_ref: Option<&str>) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repo, gio::NONE_CANCELLABLE)?;
    let imgref = imgref.try_into()?;
    let (tx_progress, rx_progress) = tokio::sync::watch::channel(Default::default());
    let target = indicatif::ProgressDrawTarget::stdout();
    let style = indicatif::ProgressStyle::default_bar();
    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_draw_target(target);
    pb.set_style(style.template("{spinner} {prefix} {msg}"));
    pb.enable_steady_tick(200);
    pb.set_message("Downloading...");
    let opts = ImportOptions {
        progress: Some(tx_progress),
    };
    let import = crate::container::import(repo, &imgref, Some(opts));
    tokio::pin!(import);
    tokio::pin!(rx_progress);
    let import = loop {
        tokio::select! {
            _ = rx_progress.changed() => {
                let n = rx_progress.borrow().processed_bytes;
                pb.set_message(format!("Processed: {}", indicatif::HumanBytes(n)));
            }
            import = &mut import => {
                pb.finish();
                break import?;
            }
        }
    };

    if let Some(write_ref) = write_ref {
        repo.set_ref_immediate(
            None,
            write_ref,
            Some(import.ostree_commit.as_str()),
            gio::NONE_CANCELLABLE,
        )?;
        println!(
            "Imported: {} => {}",
            write_ref,
            import.ostree_commit.as_str()
        );
    } else {
        println!("Imported: {}", import.ostree_commit);
    }

    Ok(())
}

/// Export a container image with an encapsulated ostree commit.
async fn container_export(
    repo: &str,
    rev: &str,
    imgref: &str,
    labels: BTreeMap<String, String>,
    cmd: Option<Vec<String>>,
) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repo, gio::NONE_CANCELLABLE)?;
    let config = Config {
        labels: Some(labels),
        cmd,
    };
    let imgref = imgref.try_into()?;
    let pushed = crate::container::export(repo, rev, &config, &imgref).await?;
    println!("{}", pushed);
    Ok(())
}

/// Load metadata for a container image with an encapsulated ostree commit.
async fn container_info(imgref: &str) -> Result<()> {
    let imgref = imgref.try_into()?;
    let (_, digest) = crate::container::fetch_manifest(&imgref).await?;
    println!("{} digest: {}", imgref, digest);
    Ok(())
}

/// Add IMA signatures to an ostree commit, generating a new commit.
fn ima_sign(cmdopts: &ImaSignOpts) -> Result<()> {
    let repo =
        &ostree::Repo::open_at(libc::AT_FDCWD, cmdopts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    let signopts = crate::ima::ImaOpts {
        algorithm: cmdopts.algorithm.clone(),
        key: cmdopts.key.clone(),
    };
    let signed_commit = crate::ima::ima_sign(repo, cmdopts.src_rev.as_str(), &signopts)?;
    repo.set_ref_immediate(
        None,
        cmdopts.target_ref.as_str(),
        Some(signed_commit.as_str()),
        gio::NONE_CANCELLABLE,
    )?;
    println!("{} => {}", cmdopts.target_ref, signed_commit);
    Ok(())
}

/// Parse the provided arguments and execute.
/// Calls [`structopt::clap::Error::exit`] on failure, printing the error message and aborting the program.
pub async fn run_from_iter<I>(args: I) -> Result<()>
where
    I: IntoIterator,
    I::Item: Into<OsString> + Clone,
{
    let opt = Opt::from_iter(args);
    match opt {
        Opt::Tar(TarOpts::Import(ref opt)) => tar_import(opt).await,
        Opt::Tar(TarOpts::Export(ref opt)) => tar_export(opt),
        Opt::Container(ContainerOpts::Info { imgref }) => container_info(imgref.as_str()).await,
        Opt::Container(ContainerOpts::Import {
            repo,
            imgref,
            write_ref,
        }) => container_import(&repo, &imgref, write_ref.as_deref()).await,
        Opt::Container(ContainerOpts::Export {
            repo,
            rev,
            imgref,
            labels,
            cmd,
        }) => {
            let labels: Result<BTreeMap<_, _>> = labels
                .into_iter()
                .map(|l| {
                    let mut parts = l.splitn(2, '=');
                    let k = parts.next().unwrap();
                    let v = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Missing '=' in label {}", l))?;
                    Ok((k.to_string(), v.to_string()))
                })
                .collect();
            container_export(&repo, &rev, &imgref, labels?, cmd).await
        }
        Opt::ImaSign(ref opts) => ima_sign(opts),
    }
}
