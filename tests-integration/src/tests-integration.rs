use clap::Parser;

mod hostpriv;
mod install;

#[derive(Debug, Parser)]
#[clap(name = "bootc-integration-tests", version, rename_all = "kebab-case")]
pub(crate) enum Opt {
    InstallAlongside {
        /// Source container image reference
        image: String,
        #[clap(flatten)]
        testargs: libtest_mimic::Arguments,
    },
    HostPrivileged {
        image: String,
        #[clap(flatten)]
        testargs: libtest_mimic::Arguments,
    },
}

fn main() {
    let opt = Opt::parse();
    let r = match opt {
        Opt::InstallAlongside { image, testargs } => install::run_alongside(&image, testargs),
        Opt::HostPrivileged { image, testargs } => hostpriv::run_hostpriv(&image, testargs),
    };
    if let Err(e) = r {
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }
}
