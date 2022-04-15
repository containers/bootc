//! # Commandline parsing
//!
//! While there is a separate `ostree-ext-cli` crate that
//! can be installed and used directly, the CLI code is
//! also exported as a library too, so that projects
//! such as `rpm-ostree` can directly reuse it.

use anyhow::{Context, Result};
use camino::Utf8PathBuf;
use ostree::{cap_std, gio, glib};
use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::ffi::OsString;
use std::path::PathBuf;
use structopt::StructOpt;
use tokio::sync::mpsc::Receiver;

use crate::commit::container_commit;
use crate::container::store::{ImportProgress, PreparedImport};
use crate::container::{self as ostree_container};
use crate::container::{Config, ImageReference, OstreeImageReference};
use ostree_container::store::{ImageImporter, PrepareResult};

/// Parse an [`OstreeImageReference`] from a CLI arguemnt.
pub fn parse_imgref(s: &str) -> Result<OstreeImageReference> {
    OstreeImageReference::try_from(s)
}

/// Parse a base [`ImageReference`] from a CLI arguemnt.
pub fn parse_base_imgref(s: &str) -> Result<ImageReference> {
    ImageReference::try_from(s)
}

/// Parse an [`ostree::Repo`] from a CLI arguemnt.
pub fn parse_repo(s: &str) -> Result<ostree::Repo> {
    let repofd = cap_std::fs::Dir::open_ambient_dir(s, cap_std::ambient_authority())?;
    Ok(ostree::Repo::open_at_dir(&repofd, ".")?)
}

/// Options for importing a tar archive.
#[derive(Debug, StructOpt)]
struct ImportOpts {
    /// Path to the repository
    #[structopt(long)]
    #[structopt(parse(try_from_str = parse_repo))]
    repo: ostree::Repo,

    /// Path to a tar archive; if unspecified, will be stdin.  Currently the tar archive must not be compressed.
    path: Option<String>,
}

/// Options for exporting a tar archive.
#[derive(Debug, StructOpt)]
struct ExportOpts {
    /// Path to the repository
    #[structopt(long)]
    #[structopt(parse(try_from_str = parse_repo))]
    repo: ostree::Repo,

    /// The format version.  Must be 0 or 1.
    #[structopt(long)]
    format_version: u32,

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
        #[structopt(parse(try_from_str = parse_repo))]
        repo: ostree::Repo,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[structopt(parse(try_from_str = parse_imgref))]
        imgref: OstreeImageReference,

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
        #[structopt(parse(try_from_str = parse_imgref))]
        imgref: OstreeImageReference,
    },

    ///  Wrap an ostree commit into a container
    #[structopt(alias = "export")]
    Encapsulate {
        /// Path to the repository
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_repo))]
        repo: ostree::Repo,

        /// The ostree ref or commit to export
        rev: String,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[structopt(parse(try_from_str = parse_base_imgref))]
        imgref: ImageReference,

        /// Additional labels for the container
        #[structopt(name = "label", long, short)]
        labels: Vec<String>,

        /// Propagate an OSTree commit metadata key to container label
        #[structopt(name = "copymeta", long)]
        copy_meta_keys: Vec<String>,

        /// Corresponds to the Dockerfile `CMD` instruction.
        #[structopt(long)]
        cmd: Option<Vec<String>>,
    },

    #[structopt(alias = "commit")]
    /// Perform build-time checking and canonicalization.
    /// This is presently an optional command, but may become required in the future.
    Commit,

    /// Commands for working with (possibly layered, non-encapsulated) container images.
    Image(ContainerImageOpts),
}

/// Options for container image fetching.
#[derive(Debug, StructOpt)]
struct ContainerProxyOpts {
    #[structopt(long)]
    /// Do not use default authentication files.
    auth_anonymous: bool,

    #[structopt(long)]
    /// Path to Docker-formatted authentication file.
    authfile: Option<PathBuf>,

    #[structopt(long)]
    /// Directory with certificates (*.crt, *.cert, *.key) used to connect to registry
    /// Equivalent to `skopeo --cert-dir`
    cert_dir: Option<PathBuf>,

    #[structopt(long)]
    /// Skip TLS verification.
    insecure_skip_tls_verification: bool,
}

/// Options for import/export to tar archives.
#[derive(Debug, StructOpt)]
enum ContainerImageOpts {
    /// List container images
    List {
        /// Path to the repository
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_repo))]
        repo: ostree::Repo,
    },

    /// Pull (or update) a container image.
    Pull {
        /// Path to the repository
        #[structopt(parse(try_from_str = parse_repo))]
        repo: ostree::Repo,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[structopt(parse(try_from_str = parse_imgref))]
        imgref: OstreeImageReference,

        #[structopt(flatten)]
        proxyopts: ContainerProxyOpts,
    },

    /// Output metadata about an already stored container image.
    History {
        /// Path to the repository
        #[structopt(long, parse(try_from_str = parse_repo))]
        repo: ostree::Repo,

        /// Container image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[structopt(parse(try_from_str = parse_base_imgref))]
        imgref: ImageReference,
    },

    /// Copy a pulled container image from one repo to another.
    Copy {
        /// Path to the source repository
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_repo))]
        src_repo: ostree::Repo,

        /// Path to the destination repository
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_repo))]
        dest_repo: ostree::Repo,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[structopt(parse(try_from_str = parse_imgref))]
        imgref: OstreeImageReference,
    },

    /// Replace the detached metadata (e.g. to add a signature)
    ReplaceDetachedMetadata {
        /// Path to the source repository
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_base_imgref))]
        src: ImageReference,

        /// Target image
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_base_imgref))]
        dest: ImageReference,

        /// Path to file containing new detached metadata; if not provided,
        /// any existing detached metadata will be deleted.
        contents: Option<Utf8PathBuf>,
    },

    /// Unreference one or more pulled container images and perform a garbage collection.
    Remove {
        /// Path to the repository
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_repo))]
        repo: ostree::Repo,

        /// Image reference, e.g. quay.io/exampleos/exampleos:latest
        #[structopt(parse(try_from_str = parse_base_imgref))]
        imgrefs: Vec<ImageReference>,

        /// Do not garbage collect unused layers
        #[structopt(long)]
        skip_gc: bool,
    },

    /// Perform initial deployment for a container image
    Deploy {
        /// Path to the system root
        #[structopt(long)]
        sysroot: String,

        /// Name for the state directory, also known as "osname".
        #[structopt(long)]
        stateroot: String,

        /// Source image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos@sha256:abcd...
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_imgref))]
        imgref: OstreeImageReference,

        #[structopt(flatten)]
        proxyopts: ContainerProxyOpts,

        /// Target image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        ///
        /// If specified, `--imgref` will be used as a source, but this reference will be emitted into the origin
        /// so that later OS updates pull from it.
        #[structopt(long)]
        #[structopt(parse(try_from_str = parse_imgref))]
        target_imgref: Option<OstreeImageReference>,

        #[structopt(long)]
        /// Add a kernel argument
        karg: Option<Vec<String>>,

        /// Write the deployed checksum to this file
        #[structopt(long)]
        write_commitid_to: Option<Utf8PathBuf>,
    },
}

/// Options for the Integrity Measurement Architecture (IMA).
#[derive(Debug, StructOpt)]
struct ImaSignOpts {
    /// Path to the repository
    #[structopt(long)]
    #[structopt(parse(try_from_str = parse_repo))]
    repo: ostree::Repo,
    /// The ostree ref or commit to use as a base
    src_rev: String,
    /// The ostree ref to use for writing the signed commit
    target_ref: String,

    /// Digest algorithm
    algorithm: String,
    /// Path to IMA key
    key: Utf8PathBuf,

    #[structopt(long)]
    /// Overwrite any existing signatures
    overwrite: bool,
}

/// Options for internal testing
#[derive(Debug, StructOpt)]
enum TestingOpts {
    /// Detect the current environment
    DetectEnv,
    /// Execute integration tests, assuming mutable environment
    Run,
    /// Execute IMA tests
    RunIMA,
    FilterTar,
}

/// Toplevel options for extended ostree functionality.
#[derive(Debug, StructOpt)]
#[structopt(name = "ostree-ext")]
#[structopt(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
enum Opt {
    /// Import and export to tar
    Tar(TarOpts),
    /// Import and export to a container image
    Container(ContainerOpts),
    /// IMA signatures
    ImaSign(ImaSignOpts),
    /// Internal integration testing helpers.
    #[structopt(setting(structopt::clap::AppSettings::Hidden))]
    #[cfg(feature = "internal-testing-api")]
    InternalOnlyForTesting(TestingOpts),
}

#[allow(clippy::from_over_into)]
impl Into<ostree_container::store::ImageProxyConfig> for ContainerProxyOpts {
    fn into(self) -> ostree_container::store::ImageProxyConfig {
        ostree_container::store::ImageProxyConfig {
            auth_anonymous: self.auth_anonymous,
            authfile: self.authfile,
            certificate_directory: self.cert_dir,
            insecure_skip_tls_verification: Some(self.insecure_skip_tls_verification),
            ..Default::default()
        }
    }
}

/// Import a tar archive containing an ostree commit.
async fn tar_import(opts: &ImportOpts) -> Result<()> {
    let imported = if let Some(path) = opts.path.as_ref() {
        let instream = tokio::fs::File::open(path).await?;
        crate::tar::import_tar(&opts.repo, instream, None).await?
    } else {
        let stdin = tokio::io::stdin();
        crate::tar::import_tar(&opts.repo, stdin, None).await?
    };
    println!("Imported: {}", imported);
    Ok(())
}

/// Export a tar archive containing an ostree commit.
fn tar_export(opts: &ExportOpts) -> Result<()> {
    #[allow(clippy::needless_update)]
    let subopts = crate::tar::ExportOptions {
        format_version: opts.format_version,
        ..Default::default()
    };
    crate::tar::export_commit(
        &opts.repo,
        opts.rev.as_str(),
        std::io::stdout(),
        Some(subopts),
    )?;
    Ok(())
}

/// Render an import progress notification as a string.
pub fn layer_progress_format(p: &ImportProgress) -> String {
    let (starting, s, layer) = match p {
        ImportProgress::OstreeChunkStarted(v) => (true, "ostree chunk", v),
        ImportProgress::OstreeChunkCompleted(v) => (false, "ostree chunk", v),
        ImportProgress::DerivedLayerStarted(v) => (true, "layer", v),
        ImportProgress::DerivedLayerCompleted(v) => (false, "layer", v),
    };
    // podman outputs 12 characters of digest, let's add 7 for `sha256:`.
    let short_digest = layer.digest().chars().take(12 + 7).collect::<String>();
    if starting {
        let size = glib::format_size(layer.size() as u64);
        format!("Fetching {s} {short_digest} ({size})")
    } else {
        format!("Fetched {s} {short_digest}")
    }
}

async fn handle_layer_progress_print(mut r: Receiver<ImportProgress>) {
    while let Some(v) = r.recv().await {
        println!("{}", layer_progress_format(&v));
    }
}

fn print_layer_status(prep: &PreparedImport) {
    let (stored, to_fetch, to_fetch_size) =
        prep.all_layers()
            .fold((0u32, 0u32, 0u64), |(stored, to_fetch, sz), v| {
                if v.commit.is_some() {
                    (stored + 1, to_fetch, sz)
                } else {
                    (stored, to_fetch + 1, sz + v.size())
                }
            });
    if to_fetch > 0 {
        let size = crate::glib::format_size(to_fetch_size);
        println!("layers stored: {stored} needed: {to_fetch} ({size})");
    }
}

/// Import a container image with an encapsulated ostree commit.
async fn container_import(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    write_ref: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let target = indicatif::ProgressDrawTarget::stdout();
    let style = indicatif::ProgressStyle::default_bar();
    let pb = (!quiet).then(|| {
        let pb = indicatif::ProgressBar::new_spinner();
        pb.set_draw_target(target);
        pb.set_style(style.template("{spinner} {prefix} {msg}"));
        pb.enable_steady_tick(200);
        pb.set_message("Downloading...");
        pb
    });
    let importer = ImageImporter::new(repo, imgref, Default::default()).await?;
    let import_result = importer.unencapsulate().await;
    if let Some(pb) = pb.as_ref() {
        pb.finish();
    }
    // It must have been set
    let import = import_result.unwrap();
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
    repo: &ostree::Repo,
    rev: &str,
    imgref: &ImageReference,
    labels: BTreeMap<String, String>,
    copy_meta_keys: Vec<String>,
    cmd: Option<Vec<String>>,
) -> Result<()> {
    let config = Config {
        labels: Some(labels),
        cmd,
    };
    let opts = crate::container::ExportOpts {
        copy_meta_keys,
        ..Default::default()
    };
    let pushed =
        crate::container::encapsulate(repo, rev, &config, Some(opts), None, imgref).await?;
    println!("{}", pushed);
    Ok(())
}

/// Load metadata for a container image with an encapsulated ostree commit.
async fn container_info(imgref: &OstreeImageReference) -> Result<()> {
    let (_, digest) = crate::container::fetch_manifest(imgref).await?;
    println!("{} digest: {}", imgref, digest);
    Ok(())
}

/// Write a layered container image into an OSTree commit.
async fn container_store(
    repo: &ostree::Repo,
    imgref: &OstreeImageReference,
    proxyopts: ContainerProxyOpts,
) -> Result<()> {
    let mut imp = ImageImporter::new(repo, imgref, proxyopts.into()).await?;
    let layer_progress = imp.request_progress();
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {} => {}", imgref, c.merge_commit);
            return Ok(());
        }
        PrepareResult::Ready(r) => r,
    };
    print_layer_status(&prep);
    let progress_printer =
        tokio::task::spawn(async move { handle_layer_progress_print(layer_progress).await });
    let import = imp.import(prep).await;
    let _ = progress_printer.await;
    let import = import?;
    let commit = &repo.load_commit(&import.merge_commit)?.0;
    let commit_meta = &glib::VariantDict::new(Some(&commit.child_value(0)));
    let filtered = commit_meta.lookup::<ostree_container::store::MetaFilteredData>(
        ostree_container::store::META_FILTERED,
    )?;
    if let Some(filtered) = filtered {
        for (layerid, filtered) in filtered {
            eprintln!("Unsupported paths filtered from {}:", layerid);
            for (prefix, count) in filtered {
                eprintln!("  {}: {}", prefix, count);
            }
        }
    }
    println!("Wrote: {} => {}", imgref, import.merge_commit);
    Ok(())
}

fn print_column(s: &str, clen: usize, remaining: &mut usize) {
    let l = s.len().min(*remaining);
    print!("{}", &s[0..l]);
    if clen > 0 {
        // We always want two trailing spaces
        let pad = clen.saturating_sub(l) + 2;
        for _ in 0..pad {
            print!(" ");
        }
        *remaining = remaining.checked_sub(l + pad).unwrap();
    }
}

/// Output the container image history
async fn container_history(repo: &ostree::Repo, imgref: &ImageReference) -> Result<()> {
    let img = crate::container::store::query_image_ref(repo, imgref)?
        .ok_or_else(|| anyhow::anyhow!("No such image: {}", imgref))?;
    let columns = [("ID", 20), ("SIZE", 10), ("CREATED BY", 0usize)];
    let width = term_size::dimensions().map(|x| x.0).unwrap_or(80);
    if let Some(config) = img.configuration.as_ref() {
        {
            let mut remaining = width;
            for (name, width) in columns.iter() {
                print_column(name, *width as usize, &mut remaining);
            }
            println!();
        }

        let mut history = config.history().iter();
        let layers = img.manifest.layers().iter();
        for layer in layers {
            let histent = history.next();
            let created_by = histent
                .and_then(|s| s.created_by().as_deref())
                .unwrap_or("");

            let mut remaining = width;

            let digest = layer.digest().as_str();
            // Verify it's OK to slice, this should all be ASCII
            assert!(digest.chars().all(|c| c.is_ascii()));
            let digest_max = columns[0].1;
            let digest = &digest[0..digest_max];
            print_column(digest, digest_max, &mut remaining);
            let size = glib::format_size(layer.size() as u64);
            print_column(size.as_str(), columns[1].1, &mut remaining);
            print_column(created_by, columns[2].1, &mut remaining);
            println!();
        }
        Ok(())
    } else {
        anyhow::bail!("v0 image does not have fetched configuration");
    }
}

/// Add IMA signatures to an ostree commit, generating a new commit.
fn ima_sign(cmdopts: &ImaSignOpts) -> Result<()> {
    let cancellable = gio::NONE_CANCELLABLE;
    let signopts = crate::ima::ImaOpts {
        algorithm: cmdopts.algorithm.clone(),
        key: cmdopts.key.clone(),
        overwrite: cmdopts.overwrite,
    };
    let tx = cmdopts.repo.auto_transaction(cancellable)?;
    let signed_commit = crate::ima::ima_sign(&cmdopts.repo, cmdopts.src_rev.as_str(), &signopts)?;
    cmdopts.repo.transaction_set_ref(
        None,
        cmdopts.target_ref.as_str(),
        Some(signed_commit.as_str()),
    );
    let _stats = tx.commit(cancellable)?;
    println!("{} => {}", cmdopts.target_ref, signed_commit);
    Ok(())
}

#[cfg(feature = "internal-testing-api")]
fn testing(opts: &TestingOpts) -> Result<()> {
    match opts {
        TestingOpts::DetectEnv => {
            let s = crate::integrationtest::detectenv();
            println!("{}", s);
            Ok(())
        }
        TestingOpts::Run => crate::integrationtest::run_tests(),
        TestingOpts::RunIMA => crate::integrationtest::test_ima(),
        TestingOpts::FilterTar => {
            crate::tar::filter_tar(std::io::stdin(), std::io::stdout()).map(|_| {})
        }
    }
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
            ContainerOpts::Info { imgref } => container_info(&imgref).await,
            ContainerOpts::Commit {} => container_commit().await,
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
                copy_meta_keys,
                cmd,
            } => {
                let labels: Result<BTreeMap<_, _>> = labels
                    .into_iter()
                    .map(|l| {
                        let (k, v) = l
                            .split_once('=')
                            .ok_or_else(|| anyhow::anyhow!("Missing '=' in label {}", l))?;
                        Ok((k.to_string(), v.to_string()))
                    })
                    .collect();
                container_export(&repo, &rev, &imgref, labels?, copy_meta_keys, cmd).await
            }
            ContainerOpts::Image(opts) => match opts {
                ContainerImageOpts::List { repo } => {
                    for image in crate::container::store::list_images(&repo)? {
                        println!("{}", image);
                    }
                    Ok(())
                }
                ContainerImageOpts::Pull {
                    repo,
                    imgref,
                    proxyopts,
                } => container_store(&repo, &imgref, proxyopts).await,
                ContainerImageOpts::History { repo, imgref } => {
                    container_history(&repo, &imgref).await
                }
                ContainerImageOpts::Remove {
                    repo,
                    imgrefs,
                    skip_gc,
                } => {
                    let nimgs = imgrefs.len();
                    crate::container::store::remove_images(&repo, imgrefs.iter())?;
                    if !skip_gc {
                        let nlayers = crate::container::store::gc_image_layers(&repo)?;
                        println!("Removed images: {nimgs} layers: {nlayers}");
                    } else {
                        println!("Removed images: {nimgs}");
                    }
                    Ok(())
                }
                ContainerImageOpts::Copy {
                    src_repo,
                    dest_repo,
                    imgref,
                } => crate::container::store::copy(&src_repo, &dest_repo, &imgref).await,
                ContainerImageOpts::ReplaceDetachedMetadata {
                    src,
                    dest,
                    contents,
                } => {
                    let contents = contents.map(std::fs::read).transpose()?;
                    let digest = crate::container::update_detached_metadata(
                        &src,
                        &dest,
                        contents.as_deref(),
                    )
                    .await?;
                    println!("Pushed: {}", digest);
                    Ok(())
                }
                ContainerImageOpts::Deploy {
                    sysroot,
                    stateroot,
                    imgref,
                    target_imgref,
                    karg,
                    proxyopts,
                    write_commitid_to,
                } => {
                    let sysroot = &ostree::Sysroot::new(Some(&gio::File::for_path(&sysroot)));
                    sysroot.load(gio::NONE_CANCELLABLE)?;
                    let kargs = karg.as_deref();
                    let kargs = kargs.map(|v| {
                        let r: Vec<_> = v.iter().map(|s| s.as_str()).collect();
                        r
                    });
                    let options = crate::container::deploy::DeployOpts {
                        kargs: kargs.as_deref(),
                        target_imgref: target_imgref.as_ref(),
                        proxy_cfg: Some(proxyopts.into()),
                    };
                    let state = crate::container::deploy::deploy(
                        sysroot,
                        &stateroot,
                        &imgref,
                        Some(options),
                    )
                    .await?;
                    if let Some(p) = write_commitid_to {
                        std::fs::write(&p, state.merge_commit.as_bytes())
                            .with_context(|| format!("Failed to write commitid to {}", p))?;
                    }
                    Ok(())
                }
            },
        },
        Opt::ImaSign(ref opts) => ima_sign(opts),
        #[cfg(feature = "internal-testing-api")]
        Opt::InternalOnlyForTesting(ref opts) => testing(opts),
    }
}
