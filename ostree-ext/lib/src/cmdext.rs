use std::os::unix::prelude::{CommandExt, RawFd};

pub(crate) trait CommandRedirectionExt {
    /// Pass a file descriptor into the target process.
    /// IMPORTANT: `fd` must be valid (i.e. cannot be closed) until after [`std::Process::Command::spawn`] or equivalent is invoked.
    fn take_fd_n(&mut self, fd: i32, target: i32) -> &mut Self;
}

#[allow(unsafe_code)]
impl CommandRedirectionExt for std::process::Command {
    fn take_fd_n(&mut self, fd: i32, target: i32) -> &mut Self {
        unsafe {
            self.pre_exec(move || {
                nix::unistd::dup2(fd, target as RawFd)
                    .map(|_r| ())
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("{}", e)))
            });
        }
        self
    }
}
