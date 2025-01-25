use clap::Parser;

#[derive(Parser)]
pub(crate) struct Cli {
    /// The bootc container image to install, e.g. quay.io/fedora/fedora-bootc:41
    pub(crate) bootc_image: String,
}
