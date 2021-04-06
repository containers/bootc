use anyhow::Result;
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

// #[derive(Debug, StructOpt)]
// enum ContainerOpts {
//     /// Import an ostree commit embedded in a container image
//     Import {
//         /// Path to remote image, e.g. quay.io/exampleos/exampleos:latest
//         imgref: String,
//     },

//     /// Export an ostree commit to an OCI layout
//     Export(ExportOpts),
// }

#[derive(Debug, StructOpt)]
#[structopt(name = "ostree-ext")]
#[structopt(rename_all = "kebab-case")]
enum Opt {
    /// Import and export to tar
    Tar(TarOpts),
    // Container(ContainerOpts),
}

fn tar_import(opts: &ImportOpts) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, opts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    let imported = if let Some(path) = opts.path.as_ref() {
        let instream = std::io::BufReader::new(std::fs::File::open(path)?);
        ostree_ext::tar::import_tar(repo, instream)?
    } else {
        let stdin = std::io::stdin();
        let stdin = stdin.lock();
        ostree_ext::tar::import_tar(repo, stdin)?
    };
    println!("Imported: {}", imported);
    Ok(())
}

fn tar_export(opts: &ExportOpts) -> Result<()> {
    let repo = &ostree::Repo::open_at(libc::AT_FDCWD, opts.repo.as_str(), gio::NONE_CANCELLABLE)?;
    ostree_ext::tar::export_commit(repo, opts.rev.as_str(), std::io::stdout())?;
    Ok(())
}

fn run() -> Result<()> {
    let opt = Opt::from_args();
    match opt {
        Opt::Tar(TarOpts::Import(ref opt)) => tar_import(opt),
        Opt::Tar(TarOpts::Export(ref opt)) => tar_export(opt),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {:#}", e);
        std::process::exit(1);
    }
}
