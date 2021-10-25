//! # Commandline parsing
//!
//! While there is a separate `ostree-ext-cli` crate that
//! can be installed and used directly, the CLI code is
//! also exported as a library too, so that projects
//! such as `rpm-ostree` can directly reuse it.

use anyhow::Result;
use ostree::gio;
use std::collections::BTreeMap;
use std::convert::{TryFrom, TryInto};
use std::ffi::OsString;
use structopt::StructOpt;

use crate::container::store::{LayeredImageImporter, PrepareResult};
use crate::container::{Config, ImportOptions, OstreeImageReference};

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
    #[structopt(alias = "import")]
    /// Import an ostree commit embedded in a remote container image
    Unencapsulate {
        /// Path to the repository
        #[structopt(long)]
        repo: String,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        imgref: String,

        /// Create an ostree ref pointing to the imported commit
        #[structopt(long)]
        write_ref: Option<String>,

        /// Don't display progress
        #[structopt(long)]
        quiet: bool,
    },

    /// Print information about an exported ostree-container image.
    Info {
        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        imgref: String,
    },

    ///  Wrap an ostree commit into a container
    #[structopt(alias = "export")]
    Encapsulate {
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

    /// Commands for working with (possibly layered, non-encapsulated) container images.
    Image(ContainerImageOpts),
}

/// Options for import/export to tar archives.
#[derive(Debug, StructOpt)]
enum ContainerImageOpts {
    /// List container images
    List {
        /// Path to the repository
        #[structopt(long)]
        repo: String,
    },

    /// Pull (or update) a container image.
    Pull {
        /// Path to the repository
        #[structopt(long)]
        repo: String,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        imgref: String,
    },

    /// Copy a pulled container image from one repo to another.
    Copy {
        /// Path to the source repository
        #[structopt(long)]
        src_repo: String,

        /// Path to the destination repository
        #[structopt(long)]
        dest_repo: String,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        imgref: String,
    },

    /// Perform initial deployment for a container image
    Deploy {
        /// Path to the system root
        #[structopt(long)]
        sysroot: String,

        /// Name for the state directory, also known as "osname".
        #[structopt(long)]
        stateroot: String,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[structopt(long)]
        imgref: String,

        #[structopt(long)]
        /// Add a kernel argument
        karg: Option<Vec<String>>,
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
async fn container_import(
    repo: &str,
    imgref: &str,
    write_ref: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repo, gio::NONE_CANCELLABLE)?;
    let imgref = imgref.try_into()?;
    let (tx_progress, rx_progress) = tokio::sync::watch::channel(Default::default());
    let target = indicatif::ProgressDrawTarget::stdout();
    let style = indicatif::ProgressStyle::default_bar();
    let pb = if !quiet {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_draw_target(target);
        pb.set_style(style.template("{spinner} {prefix} {msg}"));
        pb.enable_steady_tick(200);
        pb.set_message("Downloading...");
        Some(pb)
    } else {
        None
    };
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
                if let Some(pb) = pb.as_ref() {
                    pb.set_message(format!("Processed: {}", indicatif::HumanBytes(n)));
                }
            }
            import = &mut import => {
                if let Some(pb) = pb.as_ref() {
                    pb.finish();
                }
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

/// Write a layered container image into an OSTree commit.
async fn container_store(repo: &str, imgref: &str) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repo, gio::NONE_CANCELLABLE)?;
    let imgref = imgref.try_into()?;
    let mut imp = LayeredImageImporter::new(repo, &imgref).await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {} => {}", imgref, c);
            return Ok(());
        }
        PrepareResult::Ready(r) => r,
    };
    if prep.base_layer.commit.is_none() {
        let size = crate::glib::format_size(prep.base_layer.size());
        println!(
            "Downloading base layer: {} ({})",
            prep.base_layer.digest(),
            size
        );
    } else {
        println!("Using base: {}", prep.base_layer.digest());
    }
    for layer in prep.layers.iter() {
        if layer.commit.is_some() {
            println!("Using layer: {}", layer.digest());
        } else {
            let size = crate::glib::format_size(layer.size());
            println!("Downloading layer: {} ({})", layer.digest(), size);
        }
    }
    let import = imp.import(prep).await?;
    if !import.layer_filtered_content.is_empty() {
        for (layerid, filtered) in import.layer_filtered_content {
            eprintln!("Unsupported paths filtered from {}:", layerid);
            for (prefix, count) in filtered {
                eprintln!("  {}: {}", prefix, count);
            }
        }
    }
    println!(
        "Wrote: {} => {} => {}",
        imgref, import.ostree_ref, import.commit
    );
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
        Opt::Container(o) => match o {
            ContainerOpts::Info { imgref } => container_info(imgref.as_str()).await,
            ContainerOpts::Unencapsulate {
                repo,
                imgref,
                write_ref,
                quiet,
            } => container_import(&repo, &imgref, write_ref.as_deref(), quiet).await,
            ContainerOpts::Encapsulate {
                repo,
                rev,
                imgref,
                labels,
                cmd,
            } => {
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
            ContainerOpts::Image(opts) => match opts {
                ContainerImageOpts::List { repo } => {
                    let repo =
                        &ostree::Repo::open_at(libc::AT_FDCWD, &repo, gio::NONE_CANCELLABLE)?;
                    for image in crate::container::store::list_images(repo)? {
                        println!("{}", image);
                    }
                    Ok(())
                }
                ContainerImageOpts::Pull { repo, imgref } => container_store(&repo, &imgref).await,
                ContainerImageOpts::Copy {
                    src_repo,
                    dest_repo,
                    imgref,
                } => {
                    let src_repo =
                        &ostree::Repo::open_at(libc::AT_FDCWD, &src_repo, gio::NONE_CANCELLABLE)?;
                    let dest_repo =
                        &ostree::Repo::open_at(libc::AT_FDCWD, &dest_repo, gio::NONE_CANCELLABLE)?;
                    let imgref = OstreeImageReference::try_from(imgref.as_str())?;
                    crate::container::store::copy(src_repo, dest_repo, &imgref).await
                }
                ContainerImageOpts::Deploy {
                    sysroot,
                    stateroot,
                    imgref,
                    karg,
                } => {
                    let sysroot = &ostree::Sysroot::new(Some(&gio::File::for_path(&sysroot)));
                    let imgref = OstreeImageReference::try_from(imgref.as_str())?;
                    let kargs = karg.as_deref();
                    let kargs = kargs.map(|v| {
                        let r: Vec<_> = v.iter().map(|s| s.as_str()).collect();
                        r
                    });
                    let options = crate::container::deploy::DeployOpts {
                        kargs: kargs.as_deref(),
                    };
                    crate::container::deploy::deploy(sysroot, &stateroot, &imgref, Some(options))
                        .await
                }
            },
        },
        Opt::ImaSign(ref opts) => ima_sign(opts),
    }
}
