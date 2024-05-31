use anyhow::Result;
use fn_error_context::context;
use libtest_mimic::Trial;
use xshell::cmd;

struct TestState {
    image: String,
}

fn new_test(
    state: &'static TestState,
    description: &'static str,
    f: fn(&'static str) -> anyhow::Result<()>,
) -> libtest_mimic::Trial {
    Trial::test(description, move || f(&state.image).map_err(Into::into))
}

fn test_loopback_install(image: &'static str) -> Result<()> {
    let base_args = super::install::BASE_ARGS;
    let sh = &xshell::Shell::new()?;
    let size = 10 * 1000 * 1000 * 1000;
    let mut tmpdisk = tempfile::NamedTempFile::new_in("/var/tmp")?;
    tmpdisk.as_file_mut().set_len(size)?;
    let tmpdisk = tmpdisk.into_temp_path();
    let tmpdisk = tmpdisk.to_str().unwrap();
    cmd!(sh, "sudo {base_args...} -v {tmpdisk}:/disk {image} bootc install to-disk --via-loopback --skip-fetch-check /disk").run()?;
    Ok(())
}

/// Tests that require real root (e.g. CAP_SYS_ADMIN) to do things like
/// create loopback devices, but are *not* destructive.  At the current time
/// these tests are defined to reference a bootc container image.
#[context("Hostpriv tests")]
pub(crate) fn run_hostpriv(image: &str, testargs: libtest_mimic::Arguments) -> Result<()> {
    let state = Box::new(TestState {
        image: image.to_string(),
    });
    // Make this static because the tests require it
    let state: &'static TestState = Box::leak(state);

    let tests = [new_test(&state, "loopback install", test_loopback_install)];

    libtest_mimic::run(&testargs, tests.into()).exit()
}
