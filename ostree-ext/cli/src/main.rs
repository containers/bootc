use anyhow::Result;
use std::convert::TryInto;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
struct BuildOpts {
    #[structopt(long)]
    repo: String,

    #[structopt(long = "ref")]
    ostree_ref: String,

    #[structopt(long)]
    oci_dir: String,
}

#[derive(Debug, StructOpt)]
struct ImportOpts {
    /// Path to the repository
    #[structopt(long)]
    repo: String,

    /// Path to a tar archive; if unspecified, will be stdin.  Currently the tar archive must not be compressed.
    path: Option<String>,
}

#[derive(Debug, StructOpt)]
struct ExportOpts {
    /// Path to the repository
    #[structopt(long)]
    repo: String,

    /// The ostree ref or commit to export
    rev: String,
}

#[derive(Debug, StructOpt)]
enum TarOpts {
    /// Import a tar archive (currently, must not be compressed)
    Import(ImportOpts),

    /// Write a tar archive to stdout
    Export(ExportOpts),
}

#[derive(Debug, StructOpt)]
enum ContainerOpts {
    /// Import an ostree commit embedded in a remote container image
    Import {
        /// Path to the repository
        #[structopt(long)]
        repo: String,

        /// Image reference, e.g. registry:quay.io/exampleos/exampleos:latest
        imgref: String,
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
    },
}

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

#[derive(Debug, StructOpt)]
#[structopt(name = "ostree-ext")]
#[structopt(rename_all = "kebab-case")]
enum Opt {
    /// Import and export to tar
    Tar(TarOpts),
    /// Import and export to a container image
    Container(ContainerOpts),
    ImaSign(ImaSignOpts),
}

async fn tar_import(opts: &ImportOpts) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, opts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    let imported = if let Some(path) = opts.path.as_ref() {
        let instream = tokio::fs::File::open(path).await?;
        ostree_ext::tar::import_tar(repo, instream).await?
    } else {
        let stdin = tokio::io::stdin();
        ostree_ext::tar::import_tar(repo, stdin).await?
    };
    println!("Imported: {}", imported);
    Ok(())
}

fn tar_export(opts: &ExportOpts) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, opts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    ostree_ext::tar::export_commit(repo, opts.rev.as_str(), std::io::stdout())?;
    Ok(())
}

async fn container_import(repo: &str, imgref: &str) -> Result<()> {
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
    let import = ostree_ext::container::import(repo, &imgref, Some(tx_progress));
    tokio::pin!(import);
    tokio::pin!(rx_progress);
    loop {
        tokio::select! {
            _ = rx_progress.changed() => {
                let n = rx_progress.borrow().processed_bytes;
                pb.set_message(&format!("Processed: {}", indicatif::HumanBytes(n)));
            }
            import = &mut import => {
                pb.finish();
                println!("Imported: {}", import?.ostree_commit);
                return Ok(())
            }
        }
    }
}

async fn container_export(repo: &str, rev: &str, imgref: &str) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, repo, gio::NONE_CANCELLABLE)?;
    let imgref = imgref.try_into()?;
    let pushed = ostree_ext::container::export(repo, rev, &imgref).await?;
    println!("{}", pushed);
    Ok(())
}

async fn container_info(imgref: &str) -> Result<()> {
    let imgref = imgref.try_into()?;
    let info = ostree_ext::container::fetch_manifest_info(&imgref).await?;
    println!("{} @{}", imgref, info.manifest_digest);
    Ok(())
}

fn ima_sign(cmdopts: &ImaSignOpts) -> Result<()> {
    let repo =
        &ostree::Repo::open_at(libc::AT_FDCWD, cmdopts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    let signopts = ostree_ext::ima::ImaOpts {
        algorithm: cmdopts.algorithm.clone(),
        key: cmdopts.key.clone(),
    };
    let signed_commit = ostree_ext::ima::ima_sign(repo, cmdopts.src_rev.as_str(), &signopts)?;
    repo.set_ref_immediate(
        None,
        cmdopts.target_ref.as_str(),
        Some(signed_commit.as_str()),
        gio::NONE_CANCELLABLE,
    )?;
    println!("{} => {}", cmdopts.target_ref, signed_commit);
    Ok(())
}

async fn run() -> Result<()> {
    tracing_subscriber::fmt::init();
    tracing::trace!("starting");
    let opt = Opt::from_args();
    match opt {
        Opt::Tar(TarOpts::Import(ref opt)) => tar_import(opt).await,
        Opt::Tar(TarOpts::Export(ref opt)) => tar_export(opt),
        Opt::Container(ContainerOpts::Info { imgref }) => container_info(imgref.as_str()).await,
        Opt::Container(ContainerOpts::Import { repo, imgref }) => {
            container_import(&repo, &imgref).await
        }
        Opt::Container(ContainerOpts::Export { repo, rev, imgref }) => {
            container_export(&repo, &rev, &imgref).await
        }
        Opt::ImaSign(ref opts) => ima_sign(opts),
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}
