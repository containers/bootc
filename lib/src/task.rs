use std::{
    ffi::OsStr,
    process::{Command, Stdio},
};

use anyhow::{Context, Result};

pub(crate) struct Task {
    description: String,
    quiet: bool,
    pub(crate) cmd: Command,
}

impl Task {
    pub(crate) fn new(description: impl AsRef<str>, exe: impl AsRef<str>) -> Self {
        Self::new_cmd(description, Command::new(exe.as_ref()))
    }

    pub(crate) fn new_cmd(description: impl AsRef<str>, mut cmd: Command) -> Self {
        let description = description.as_ref().to_string();
        // Default to noninteractive
        cmd.stdin(Stdio::null());
        Self {
            description,
            quiet: false,
            cmd,
        }
    }

    pub(crate) fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    pub(crate) fn args<S: AsRef<OsStr>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        self.cmd.args(args);
        self
    }

    /// Run the command, returning an error if the command does not exit successfully.
    pub(crate) fn run(self) -> Result<()> {
        let description = self.description;
        let mut cmd = self.cmd;
        if !self.quiet {
            println!("{description}");
        }
        tracing::debug!("exec: {cmd:?}");
        let st = cmd.status()?;
        if !st.success() {
            anyhow::bail!("Task {description} failed: {st:?}");
        }
        Ok(())
    }

    /// Like [`run()`], but return stdout.
    pub(crate) fn read(self) -> Result<String> {
        let description = self.description;
        let mut cmd = self.cmd;
        if !self.quiet {
            println!("{description}");
        }
        tracing::debug!("exec: {cmd:?}");
        cmd.stdout(Stdio::piped());
        let child = cmd
            .spawn()
            .with_context(|| format!("Spawning {description} failed"))?;
        let o = child
            .wait_with_output()
            .with_context(|| format!("Executing {description} failed"))?;
        let st = o.status;
        if !st.success() {
            anyhow::bail!("Task {description} failed: {st:?}");
        }
        Ok(String::from_utf8(o.stdout)?)
    }

    pub(crate) fn new_and_run<'a>(
        description: impl AsRef<str>,
        exe: impl AsRef<str>,
        args: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        let mut t = Self::new(description.as_ref(), exe);
        t.cmd.args(args);
        t.run()
    }
}
