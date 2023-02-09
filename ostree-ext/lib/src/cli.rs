//! # Commandline parsing
//!
//! While there is a separate `ostree-ext-cli` crate that
//! can be installed and used directly, the CLI code is
//! also exported as a library too, so that projects
//! such as `rpm-ostree` can directly reuse it.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand};
use ostree::{cap_std, gio, glib};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use tokio::sync::mpsc::Receiver;

use crate::commit::container_commit;
use crate::container::store::{ImportProgress, LayerProgress, PreparedImport};
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
pub fn parse_repo(s: &Utf8Path) -> Result<ostree::Repo> {
    let repofd = cap_std::fs::Dir::open_ambient_dir(s, cap_std::ambient_authority())
        .with_context(|| format!("Opening directory at '{s}'"))?;
    ostree::Repo::open_at_dir(&repofd, ".")
        .with_context(|| format!("Opening ostree repository at '{s}'"))
}

/// Options for importing a tar archive.
#[derive(Debug, Parser)]
pub(crate) struct ImportOpts {
    /// Path to the repository
    #[clap(long, value_parser)]
    repo: Utf8PathBuf,

    /// Path to a tar archive; if unspecified, will be stdin.  Currently the tar archive must not be compressed.
    path: Option<String>,
}

/// Options for exporting a tar archive.
#[derive(Debug, Parser)]
pub(crate) struct ExportOpts {
    /// Path to the repository
    #[clap(long, value_parser)]
    repo: Utf8PathBuf,

    /// The format version.  Must be 0 or 1.
    #[clap(long)]
    format_version: u32,

    /// The ostree ref or commit to export
    rev: String,
}

/// Options for import/export to tar archives.
#[derive(Debug, Subcommand)]
pub(crate) enum TarOpts {
    /// Import a tar archive (currently, must not be compressed)
    Import(ImportOpts),

    /// Write a tar archive to stdout
    Export(ExportOpts),
}

/// Options for container import/export.
#[derive(Debug, Subcommand)]
pub(crate) enum ContainerOpts {
    #[clap(alias = "import")]
    /// Import an ostree commit embedded in a remote container image
    Unencapsulate {
        /// Path to the repository
        #[clap(long, value_parser)]
        repo: Utf8PathBuf,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_imgref)]
        imgref: OstreeImageReference,

        /// Create an ostree ref pointing to the imported commit
        #[clap(long)]
        write_ref: Option<String>,

        /// Don't display progress
        #[clap(long)]
        quiet: bool,
    },

    /// Print information about an exported ostree-container image.
    Info {
        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_imgref)]
        imgref: OstreeImageReference,
    },

    ///  Wrap an ostree commit into a container
    #[clap(alias = "export")]
    Encapsulate {
        /// Path to the repository
        #[clap(long, value_parser)]
        repo: Utf8PathBuf,

        /// The ostree ref or commit to export
        rev: String,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_base_imgref)]
        imgref: ImageReference,

        /// Additional labels for the container
        #[clap(name = "label", long, short)]
        labels: Vec<String>,

        /// Propagate an OSTree commit metadata key to container label
        #[clap(name = "copymeta", long)]
        copy_meta_keys: Vec<String>,

        /// Propagate an optionally-present OSTree commit metadata key to container label
        #[clap(name = "copymeta-opt", long)]
        copy_meta_opt_keys: Vec<String>,

        /// Corresponds to the Dockerfile `CMD` instruction.
        #[clap(long)]
        cmd: Option<Vec<String>>,

        /// Compress at the fastest level (e.g. gzip level 1)
        #[clap(long)]
        compression_fast: bool,
    },

    #[clap(alias = "commit")]
    /// Perform build-time checking and canonicalization.
    /// This is presently an optional command, but may become required in the future.
    Commit,

    /// Commands for working with (possibly layered, non-encapsulated) container images.
    #[clap(subcommand)]
    Image(ContainerImageOpts),

    /// Compare the contents of two OCI compliant images.
    Compare {
        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_imgref)]
        imgref_old: OstreeImageReference,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_imgref)]
        imgref_new: OstreeImageReference,
    },
}

/// Options for container image fetching.
#[derive(Debug, Parser)]
pub(crate) struct ContainerProxyOpts {
    #[clap(long)]
    /// Do not use default authentication files.
    auth_anonymous: bool,

    #[clap(long)]
    /// Path to Docker-formatted authentication file.
    authfile: Option<PathBuf>,

    #[clap(long)]
    /// Directory with certificates (*.crt, *.cert, *.key) used to connect to registry
    /// Equivalent to `skopeo --cert-dir`
    cert_dir: Option<PathBuf>,

    #[clap(long)]
    /// Skip TLS verification.
    insecure_skip_tls_verification: bool,
}

/// Options for import/export to tar archives.
#[derive(Debug, Subcommand)]
pub(crate) enum ContainerImageOpts {
    /// List container images
    List {
        /// Path to the repository
        #[clap(long, value_parser)]
        repo: Utf8PathBuf,
    },

    /// Pull (or update) a container image.
    Pull {
        /// Path to the repository
        #[clap(value_parser)]
        repo: Utf8PathBuf,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_imgref)]
        imgref: OstreeImageReference,

        #[clap(flatten)]
        proxyopts: ContainerProxyOpts,

        /// Don't display progress
        #[clap(long)]
        quiet: bool,
    },

    /// Output metadata about an already stored container image.
    History {
        /// Path to the repository
        #[clap(long, value_parser)]
        repo: Utf8PathBuf,

        /// Container image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_base_imgref)]
        imgref: ImageReference,
    },

    /// Copy a pulled container image from one repo to another.
    Copy {
        /// Path to the source repository
        #[clap(long, value_parser)]
        src_repo: Utf8PathBuf,

        /// Path to the destination repository
        #[clap(long, value_parser)]
        dest_repo: Utf8PathBuf,

        /// Image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_imgref)]
        imgref: OstreeImageReference,
    },

    /// Replace the detached metadata (e.g. to add a signature)
    ReplaceDetachedMetadata {
        /// Path to the source repository
        #[clap(long)]
        #[clap(value_parser = parse_base_imgref)]
        src: ImageReference,

        /// Target image
        #[clap(long)]
        #[clap(value_parser = parse_base_imgref)]
        dest: ImageReference,

        /// Path to file containing new detached metadata; if not provided,
        /// any existing detached metadata will be deleted.
        contents: Option<Utf8PathBuf>,
    },

    /// Unreference one or more pulled container images and perform a garbage collection.
    Remove {
        /// Path to the repository
        #[clap(long, value_parser)]
        repo: Utf8PathBuf,

        /// Image reference, e.g. quay.io/exampleos/exampleos:latest
        #[clap(value_parser = parse_base_imgref)]
        imgrefs: Vec<ImageReference>,

        /// Do not garbage collect unused layers
        #[clap(long)]
        skip_gc: bool,
    },

    /// Garbage collect unreferenced image layer references.
    PruneLayers {
        /// Path to the repository
        #[clap(long, value_parser)]
        repo: Utf8PathBuf,
    },

    /// Perform initial deployment for a container image
    Deploy {
        /// Path to the system root
        #[clap(long)]
        sysroot: String,

        /// Name for the state directory, also known as "osname".
        #[clap(long)]
        stateroot: String,

        /// Source image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos@sha256:abcd...
        #[clap(long)]
        #[clap(value_parser = parse_imgref)]
        imgref: OstreeImageReference,

        #[clap(flatten)]
        proxyopts: ContainerProxyOpts,

        /// Target image reference, e.g. ostree-remote-image:someremote:registry:quay.io/exampleos/exampleos:latest
        ///
        /// If specified, `--imgref` will be used as a source, but this reference will be emitted into the origin
        /// so that later OS updates pull from it.
        #[clap(long)]
        #[clap(value_parser = parse_imgref)]
        target_imgref: Option<OstreeImageReference>,

        /// If set, only write the layer refs, but not the final container image reference.  This
        /// allows generating a disk image that when booted uses "native ostree", but has layer
        /// references "pre-cached" such that a container image fetch will avoid redownloading
        /// everything.
        #[clap(long)]
        no_imgref: bool,

        #[clap(long)]
        /// Add a kernel argument
        karg: Option<Vec<String>>,

        /// Write the deployed checksum to this file
        #[clap(long)]
        write_commitid_to: Option<Utf8PathBuf>,
    },
}

/// Options for the Integrity Measurement Architecture (IMA).
#[derive(Debug, Parser)]
pub(crate) struct ImaSignOpts {
    /// Path to the repository
    #[clap(long, value_parser)]
    repo: Utf8PathBuf,

    /// The ostree ref or commit to use as a base
    src_rev: String,
    /// The ostree ref to use for writing the signed commit
    target_ref: String,

    /// Digest algorithm
    algorithm: String,
    /// Path to IMA key
    key: Utf8PathBuf,

    #[clap(long)]
    /// Overwrite any existing signatures
    overwrite: bool,
}

/// Options for internal testing
#[derive(Debug, Subcommand)]
pub(crate) enum TestingOpts {
    /// Detect the current environment
    DetectEnv,
    /// Generate a test fixture
    CreateFixture,
    /// Execute integration tests, assuming mutable environment
    Run,
    /// Execute IMA tests
    RunIMA,
    FilterTar,
}

/// Options for man page generation
#[derive(Debug, Parser)]
pub(crate) struct ManOpts {
    #[clap(long)]
    /// Output to this directory
    directory: Utf8PathBuf,
}

/// Toplevel options for extended ostree functionality.
#[derive(Debug, Parser)]
#[clap(name = "ostree-ext")]
#[clap(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Opt {
    /// Import and export to tar
    #[clap(subcommand)]
    Tar(TarOpts),
    /// Import and export to a container image
    #[clap(subcommand)]
    Container(ContainerOpts),
    /// IMA signatures
    ImaSign(ImaSignOpts),
    /// Internal integration testing helpers.
    #[clap(hide(true), subcommand)]
    #[cfg(feature = "internal-testing-api")]
    InternalOnlyForTesting(TestingOpts),
    #[clap(hide(true))]
    #[cfg(feature = "docgen")]
    Man(ManOpts),
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
    let repo = parse_repo(&opts.repo)?;
    let imported = if let Some(path) = opts.path.as_ref() {
        let instream = tokio::fs::File::open(path).await?;
        crate::tar::import_tar(&repo, instream, None).await?
    } else {
        let stdin = tokio::io::stdin();
        crate::tar::import_tar(&repo, stdin, None).await?
    };
    println!("Imported: {}", imported);
    Ok(())
}

/// Export a tar archive containing an ostree commit.
fn tar_export(opts: &ExportOpts) -> Result<()> {
    if !crate::tar::FORMAT_VERSIONS.contains(&opts.format_version) {
        anyhow::bail!("Invalid format version: {}", opts.format_version);
    }
    let repo = parse_repo(&opts.repo)?;
    #[allow(clippy::needless_update)]
    let subopts = crate::tar::ExportOptions {
        format_version: opts.format_version,
        ..Default::default()
    };
    crate::tar::export_commit(&repo, opts.rev.as_str(), std::io::stdout(), Some(subopts))?;
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

/// Write container fetch progress to standard output.
pub async fn handle_layer_progress_print(
    mut layers: Receiver<ImportProgress>,
    mut layer_bytes: tokio::sync::watch::Receiver<Option<LayerProgress>>,
) {
    let style = indicatif::ProgressStyle::default_bar();
    let pb = indicatif::ProgressBar::new(100);
    pb.set_style(
        style
            .template("{prefix} {bytes} [{bar:20}] ({eta}) {msg}")
            .unwrap(),
    );
    loop {
        tokio::select! {
            // Always handle layer changes first.
            biased;
            layer = layers.recv() => {
                if let Some(l) = layer {
                    if l.is_starting() {
                        pb.set_position(0);
                    } else {
                        pb.finish();
                    }
                    pb.set_message(layer_progress_format(&l));
                } else {
                    // If the receiver is disconnected, then we're done
                    break
                };
            },
            r = layer_bytes.changed() => {
                if r.is_err() {
                    // If the receiver is disconnected, then we're done
                    break
                }
                let bytes = layer_bytes.borrow();
                if let Some(bytes) = &*bytes {
                    pb.set_length(bytes.total);
                    pb.set_position(bytes.fetched);
                }
            }

        }
    }
}

/// Write the status of layers to download.
pub fn print_layer_status(prep: &PreparedImport) {
    if let Some(status) = prep.format_layer_status() {
        println!("{status}");
    }
}

/// Write a deprecation notice, and sleep for 3 seconds.
pub async fn print_deprecated_warning(msg: &str) {
    eprintln!("warning: {msg}");
    tokio::time::sleep(std::time::Duration::from_secs(3)).await
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
        pb.set_style(style.template("{spinner} {prefix} {msg}").unwrap());
        pb.enable_steady_tick(std::time::Duration::from_millis(200));
        pb.set_message("Downloading...");
        pb
    });
    let importer = ImageImporter::new(repo, imgref, Default::default()).await?;
    let import = importer.unencapsulate().await;
    // Ensure we finish the progress bar before potentially propagating an error
    if let Some(pb) = pb.as_ref() {
        pb.finish();
    }
    let import = import?;
    if let Some(warning) = import.deprecated_warning.as_deref() {
        print_deprecated_warning(warning).await;
    }
    if let Some(write_ref) = write_ref {
        repo.set_ref_immediate(
            None,
            write_ref,
            Some(import.ostree_commit.as_str()),
            gio::Cancellable::NONE,
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
#[allow(clippy::too_many_arguments)]
async fn container_export(
    repo: &ostree::Repo,
    rev: &str,
    imgref: &ImageReference,
    labels: BTreeMap<String, String>,
    copy_meta_keys: Vec<String>,
    copy_meta_opt_keys: Vec<String>,
    cmd: Option<Vec<String>>,
    compression_fast: bool,
) -> Result<()> {
    let config = Config {
        labels: Some(labels),
        cmd,
    };
    let opts = crate::container::ExportOpts {
        copy_meta_keys,
        copy_meta_opt_keys,
        skip_compression: compression_fast, // TODO rename this in the struct at the next semver break
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
    quiet: bool,
) -> Result<()> {
    let mut imp = ImageImporter::new(repo, imgref, proxyopts.into()).await?;
    let prep = match imp.prepare().await? {
        PrepareResult::AlreadyPresent(c) => {
            println!("No changes in {} => {}", imgref, c.merge_commit);
            return Ok(());
        }
        PrepareResult::Ready(r) => r,
    };
    if let Some(warning) = prep.deprecated_warning() {
        print_deprecated_warning(warning).await;
    }
    print_layer_status(&prep);
    let printer = (!quiet).then(|| {
        let layer_progress = imp.request_progress();
        let layer_byte_progress = imp.request_layer_progress();
        tokio::task::spawn(async move {
            handle_layer_progress_print(layer_progress, layer_byte_progress).await
        })
    });
    let import = imp.import(prep).await;
    if let Some(printer) = printer {
        let _ = printer.await;
    }
    let import = import?;
    if let Some(msg) =
        ostree_container::store::image_filtered_content_warning(repo, &imgref.imgref)?
    {
        eprintln!("{msg}")
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
    let cancellable = gio::Cancellable::NONE;
    let signopts = crate::ima::ImaOpts {
        algorithm: cmdopts.algorithm.clone(),
        key: cmdopts.key.clone(),
        overwrite: cmdopts.overwrite,
    };
    let repo = parse_repo(&cmdopts.repo)?;
    let tx = repo.auto_transaction(cancellable)?;
    let signed_commit = crate::ima::ima_sign(&repo, cmdopts.src_rev.as_str(), &signopts)?;
    repo.transaction_set_ref(
        None,
        cmdopts.target_ref.as_str(),
        Some(signed_commit.as_str()),
    );
    let _stats = tx.commit(cancellable)?;
    println!("{} => {}", cmdopts.target_ref, signed_commit);
    Ok(())
}

#[cfg(feature = "internal-testing-api")]
async fn testing(opts: &TestingOpts) -> Result<()> {
    match opts {
        TestingOpts::DetectEnv => {
            println!("{}", crate::integrationtest::detectenv()?);
            Ok(())
        }
        TestingOpts::CreateFixture => crate::integrationtest::create_fixture().await,
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
    let opt = Opt::parse_from(args);
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
            } => {
                let repo = parse_repo(&repo)?;
                container_import(&repo, &imgref, write_ref.as_deref(), quiet).await
            }
            ContainerOpts::Encapsulate {
                repo,
                rev,
                imgref,
                labels,
                copy_meta_keys,
                copy_meta_opt_keys,
                cmd,
                compression_fast,
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
                let repo = parse_repo(&repo)?;
                container_export(
                    &repo,
                    &rev,
                    &imgref,
                    labels?,
                    copy_meta_keys,
                    copy_meta_opt_keys,
                    cmd,
                    compression_fast,
                )
                .await
            }
            ContainerOpts::Image(opts) => match opts {
                ContainerImageOpts::List { repo } => {
                    let repo = parse_repo(&repo)?;
                    for image in crate::container::store::list_images(&repo)? {
                        println!("{}", image);
                    }
                    Ok(())
                }
                ContainerImageOpts::Pull {
                    repo,
                    imgref,
                    proxyopts,
                    quiet,
                } => {
                    let repo = parse_repo(&repo)?;
                    container_store(&repo, &imgref, proxyopts, quiet).await
                }
                ContainerImageOpts::History { repo, imgref } => {
                    let repo = parse_repo(&repo)?;
                    container_history(&repo, &imgref).await
                }
                ContainerImageOpts::Remove {
                    repo,
                    imgrefs,
                    skip_gc,
                } => {
                    let nimgs = imgrefs.len();
                    let repo = parse_repo(&repo)?;
                    crate::container::store::remove_images(&repo, imgrefs.iter())?;
                    if !skip_gc {
                        let nlayers = crate::container::store::gc_image_layers(&repo)?;
                        println!("Removed images: {nimgs} layers: {nlayers}");
                    } else {
                        println!("Removed images: {nimgs}");
                    }
                    Ok(())
                }
                ContainerImageOpts::PruneLayers { repo } => {
                    let repo = parse_repo(&repo)?;
                    let nlayers = crate::container::store::gc_image_layers(&repo)?;
                    println!("Removed layers: {nlayers}");
                    Ok(())
                }
                ContainerImageOpts::Copy {
                    src_repo,
                    dest_repo,
                    imgref,
                } => {
                    let src_repo = parse_repo(&src_repo)?;
                    let dest_repo = parse_repo(&dest_repo)?;
                    let imgref = &imgref.imgref;
                    crate::container::store::copy_as(&src_repo, imgref, &dest_repo, imgref).await
                }
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
                    no_imgref,
                    karg,
                    proxyopts,
                    write_commitid_to,
                } => {
                    let sysroot = &ostree::Sysroot::new(Some(&gio::File::for_path(&sysroot)));
                    sysroot.load(gio::Cancellable::NONE)?;
                    let repo = &sysroot.repo().unwrap();
                    let kargs = karg.as_deref();
                    let kargs = kargs.map(|v| {
                        let r: Vec<_> = v.iter().map(|s| s.as_str()).collect();
                        r
                    });
                    #[allow(clippy::needless_update)]
                    let options = crate::container::deploy::DeployOpts {
                        kargs: kargs.as_deref(),
                        target_imgref: target_imgref.as_ref(),
                        proxy_cfg: Some(proxyopts.into()),
                        no_imgref,
                        ..Default::default()
                    };
                    let state = crate::container::deploy::deploy(
                        sysroot,
                        &stateroot,
                        &imgref,
                        Some(options),
                    )
                    .await?;
                    let wrote_imgref = target_imgref.as_ref().unwrap_or(&imgref);
                    if let Some(msg) = ostree_container::store::image_filtered_content_warning(
                        repo,
                        &wrote_imgref.imgref,
                    )? {
                        eprintln!("{msg}")
                    }
                    if let Some(p) = write_commitid_to {
                        std::fs::write(&p, state.merge_commit.as_bytes())
                            .with_context(|| format!("Failed to write commitid to {}", p))?;
                    }
                    Ok(())
                }
            },
            ContainerOpts::Compare {
                imgref_old,
                imgref_new,
            } => {
                let (manifest_old, _) = crate::container::fetch_manifest(&imgref_old).await?;
                let (manifest_new, _) = crate::container::fetch_manifest(&imgref_new).await?;
                let manifest_diff = crate::container::manifest_diff(&manifest_old, &manifest_new);
                manifest_diff.print();
                Ok(())
            }
        },
        Opt::ImaSign(ref opts) => ima_sign(opts),
        #[cfg(feature = "internal-testing-api")]
        Opt::InternalOnlyForTesting(ref opts) => testing(opts).await,
        #[cfg(feature = "docgen")]
        Opt::Man(manopts) => crate::docgen::generate_manpages(&manopts.directory),
    }
}
