use anyhow::Result;
use fn_error_context::context;
use libtest_mimic::Trial;
use xshell::cmd;

/// Tests that require real root (e.g. CAP_SYS_ADMIN) to do things like
/// create loopback devices, but are *not* destructive.  At the current time
/// these tests are defined to reference a bootc container image.
#[context("Hostpriv tests")]
pub(crate) fn run_hostpriv(image: &str, testargs: libtest_mimic::Arguments) -> Result<()> {
    // Just leak the image name so we get a static reference as required by the test framework
    let image: &'static str = String::from(image).leak();
    let base_args = super::install::BASE_ARGS;

    let tests = [Trial::test("loopback install", move || {
        let sh = &xshell::Shell::new()?;
        let size = 10 * 1000 * 1000 * 1000;
        let mut tmpdisk = tempfile::NamedTempFile::new_in("/var/tmp")?;
        tmpdisk.as_file_mut().set_len(size)?;
        let tmpdisk = tmpdisk.into_temp_path();
        let tmpdisk = tmpdisk.to_str().unwrap();
        cmd!(sh, "sudo {base_args...} -v {tmpdisk}:/disk {image} bootc install to-disk --via-loopback --skip-fetch-check /disk").run()?;
        Ok(())
    })];

    libtest_mimic::run(&testargs, tests.into()).exit()
}
