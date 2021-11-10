/*
 * Copyright (C) 2020 Red Hat, Inc.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

use anyhow::{bail, Context, Result};
use fn_error_context::context;
use nix::sys::socket as nixsocket;
use serde::{Deserialize, Serialize};
use std::os::unix::io::RawFd;

pub(crate) const BOOTUPD_SOCKET: &str = "/run/bootupd.sock";
pub(crate) const MSGSIZE: usize = 1_048_576;
/// Sent between processes along with SCM credentials
pub(crate) const BOOTUPD_HELLO_MSG: &str = "bootupd-hello\n";

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum DaemonToClientReply<T> {
    Success(T),
    Failure(String),
}

pub(crate) struct ClientToDaemonConnection {
    fd: i32,
}

impl Drop for ClientToDaemonConnection {
    fn drop(&mut self) {
        if self.fd != -1 {
            nix::unistd::close(self.fd).expect("close");
        }
    }
}

impl ClientToDaemonConnection {
    pub(crate) fn new() -> Self {
        Self { fd: -1 }
    }

    #[context("connecting to {}", BOOTUPD_SOCKET)]
    pub(crate) fn connect(&mut self) -> Result<()> {
        use nix::sys::uio::IoVec;
        self.fd = nixsocket::socket(
            nixsocket::AddressFamily::Unix,
            nixsocket::SockType::SeqPacket,
            nixsocket::SockFlag::SOCK_CLOEXEC,
            None,
        )?;
        let addr = nixsocket::SockAddr::new_unix(BOOTUPD_SOCKET)?;
        nixsocket::connect(self.fd, &addr)?;
        let creds = libc::ucred {
            pid: nix::unistd::getpid().as_raw(),
            uid: nix::unistd::getuid().as_raw(),
            gid: nix::unistd::getgid().as_raw(),
        };
        let creds = nixsocket::UnixCredentials::from(creds);
        let creds = nixsocket::ControlMessage::ScmCredentials(&creds);
        let _ = nixsocket::sendmsg(
            self.fd,
            &[IoVec::from_slice(BOOTUPD_HELLO_MSG.as_bytes())],
            &[creds],
            nixsocket::MsgFlags::MSG_CMSG_CLOEXEC,
            None,
        )?;
        Ok(())
    }

    pub(crate) fn send<S: serde::ser::Serialize, T: serde::de::DeserializeOwned>(
        &mut self,
        msg: &S,
    ) -> Result<T> {
        {
            let serialized = bincode::serialize(msg)?;
            let _ = nixsocket::send(self.fd, &serialized, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)
                .context("client sending request")?;
        }
        let reply: DaemonToClientReply<T> = {
            let mut buf = [0u8; MSGSIZE];
            let n = nixsocket::recv(self.fd, &mut buf, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)
                .context("client recv")?;
            let buf = &buf[0..n];
            if buf.is_empty() {
                bail!("Server sent an empty reply");
            }
            bincode::deserialize(buf).context("client parsing reply")?
        };
        match reply {
            DaemonToClientReply::Success::<T>(r) => Ok(r),
            DaemonToClientReply::Failure(buf) => {
                // For now we just prefix server
                anyhow::bail!("internal error: {}", buf);
            }
        }
    }

    pub(crate) fn shutdown(&mut self) -> Result<()> {
        nixsocket::shutdown(self.fd, nixsocket::Shutdown::Both)?;
        Ok(())
    }
}

pub(crate) struct UnauthenticatedClient {
    fd: RawFd,
}

impl UnauthenticatedClient {
    pub(crate) fn new(fd: RawFd) -> Self {
        Self { fd }
    }

    pub(crate) fn authenticate(mut self) -> Result<AuthenticatedClient> {
        use nix::sys::uio::IoVec;
        let fd = self.fd;
        let mut buf = [0u8; 1024];

        nixsocket::setsockopt(fd, nix::sys::socket::sockopt::PassCred, &true)?;
        let iov = IoVec::from_mut_slice(buf.as_mut());
        let mut cmsgspace = nix::cmsg_space!(nixsocket::UnixCredentials);
        let msg = nixsocket::recvmsg(
            fd,
            &[iov],
            Some(&mut cmsgspace),
            nixsocket::MsgFlags::MSG_CMSG_CLOEXEC,
        )?;
        let mut creds = None;
        for cmsg in msg.cmsgs() {
            if let nixsocket::ControlMessageOwned::ScmCredentials(c) = cmsg {
                creds = Some(c);
                break;
            }
        }
        if let Some(creds) = creds {
            if creds.uid() != 0 {
                bail!("unauthorized pid:{} uid:{}", creds.pid(), creds.uid())
            }
            println!("Connection from pid:{}", creds.pid());
        } else {
            bail!("No SCM credentials provided");
        }
        let hello = String::from_utf8_lossy(&buf[0..msg.bytes]);
        if hello != BOOTUPD_HELLO_MSG {
            bail!("Didn't receive correct hello message, found: {:?}", &hello);
        }
        let r = AuthenticatedClient { fd: self.fd };
        self.fd = -1;
        Ok(r)
    }
}

impl Drop for UnauthenticatedClient {
    fn drop(&mut self) {
        if self.fd != -1 {
            nix::unistd::close(self.fd).expect("close");
        }
    }
}

pub(crate) struct AuthenticatedClient {
    pub(crate) fd: RawFd,
}

impl Drop for AuthenticatedClient {
    fn drop(&mut self) {
        if self.fd != -1 {
            nix::unistd::close(self.fd).expect("close");
        }
    }
}
