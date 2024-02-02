use anyhow::Result;
use camino::Utf8Path;
use cap_std::fs::Dir;
use cap_std_ext::{cap_std, dirext::CapStdExtDirExt};
use fn_error_context::context;

const ETC_TMPFILES: &str = "etc/tmpfiles.d";
const ROOT_SSH_TMPFILE: &str = "bootc-root-ssh.conf";

#[context("Injecting root authorized_keys")]
pub(crate) fn inject_root_ssh_authorized_keys(root: &Dir, contents: &str) -> Result<()> {
    // While not documented right now, this one looks like it does not newline wrap
    let b64_encoded = ostree_ext::glib::base64_encode(contents.as_bytes());
    // See the example in https://systemd.io/CREDENTIALS/
    let tmpfiles_content = format!("f~ /root/.ssh/authorized_keys 600 root root - {b64_encoded}\n");

    let tmpfiles_dir = Utf8Path::new(ETC_TMPFILES);
    root.create_dir_all(tmpfiles_dir)?;
    let target = tmpfiles_dir.join(ROOT_SSH_TMPFILE);
    root.atomic_write(&target, &tmpfiles_content)?;
    println!("Injected: {target}");
    Ok(())
}

#[test]
fn test_inject_root_ssh() -> Result<()> {
    let root = &cap_std_ext::cap_tempfile::TempDir::new(cap_std::ambient_authority())?;

    inject_root_ssh_authorized_keys(root, "ssh-ed25519 ABCDE example@demo\n").unwrap();

    let content = root.read_to_string(format!("etc/tmpfiles.d/{ROOT_SSH_TMPFILE}"))?;
    assert_eq!(
        content,
        "f~ /root/.ssh/authorized_keys 600 root root - c3NoLWVkMjU1MTkgQUJDREUgZXhhbXBsZUBkZW1vCg==\n"
    );
    Ok(())
}
