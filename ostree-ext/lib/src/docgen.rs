// Copyright 2022 Red Hat, Inc.
//
// SPDX-License-Identifier: Apache-2.0 OR MIT

use anyhow::{Context, Result};
use camino::Utf8Path;
use clap::{Command, CommandFactory};
use std::fs::OpenOptions;
use std::io::Write;

pub fn generate_manpages(directory: &Utf8Path) -> Result<()> {
    generate_one(directory, crate::cli::Opt::command())
}

fn generate_one(directory: &Utf8Path, cmd: Command) -> Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let name = cmd.get_name();
    let path = directory.join(format!("{name}.8"));
    println!("Generating {path}...");

    let mut out = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .with_context(|| format!("opening {path}"))
        .map(std::io::BufWriter::new)?;
    clap_mangen::Man::new(cmd.clone())
        .title("ostree-ext")
        .section("8")
        .source(format!("ostree-ext {version}"))
        .render(&mut out)
        .with_context(|| format!("rendering {name}.8"))?;
    out.flush().context("flushing man page")?;
    drop(out);

    for subcmd in cmd.get_subcommands().filter(|c| !c.is_hide_set()) {
        let subname = format!("{}-{}", name, subcmd.get_name());
        // SAFETY: Latest clap 4 requires names are &'static - this is
        // not long-running production code, so we just leak the names here.
        let subname = &*std::boxed::Box::leak(subname.into_boxed_str());
        let subcmd = subcmd.clone().name(subname).alias(subname).version(version);
        generate_one(directory, subcmd)?;
    }
    Ok(())
}
