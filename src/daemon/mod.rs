//! Daemon logic.

use crate::ipc;
use anyhow::{bail, Result};
use nix::sys::socket as nixsocket;

pub fn daemon() -> Result<()> {
    use libsystemd::daemon::{self, NotifyState};
    use std::os::unix::io::IntoRawFd;

    if !daemon::booted() {
        bail!("Not running systemd")
    }
    let mut fds = libsystemd::activation::receive_descriptors(true)
        .map_err(|e| anyhow::anyhow!("Failed to receieve systemd descriptors: {}", e))?;
    let srvsock_fd = if let Some(fd) = fds.pop() {
        fd
    } else {
        bail!("No fd passed from systemd");
    };
    let srvsock_fd = srvsock_fd.into_raw_fd();
    let sent = daemon::notify(true, &[NotifyState::Ready]).expect("notify failed");
    if !sent {
        bail!("Failed to notify systemd");
    }
    loop {
        let client = ipc::UnauthenticatedClient::new(nixsocket::accept4(
            srvsock_fd,
            nixsocket::SockFlag::SOCK_CLOEXEC,
        )?);
        let mut client = client.authenticate()?;
        daemon_process_one(&mut client)?;
    }
}

fn daemon_process_one(client: &mut ipc::AuthenticatedClient) -> Result<()> {
    use crate::ClientRequest;

    let mut buf = [0u8; ipc::MSGSIZE];
    loop {
        let n = nixsocket::recv(client.fd, &mut buf, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)?;
        let buf = &buf[0..n];
        if buf.is_empty() {
            println!("Client disconnected");
            break;
        }

        let msg = bincode::deserialize(&buf)?;
        let r = match msg {
            ClientRequest::Update { component } => {
                println!("Processing update");
                bincode::serialize(&match crate::update(component.as_str()) {
                    Ok(v) => ipc::DaemonToClientReply::Success::<crate::ComponentUpdateResult>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
            ClientRequest::Validate { component } => {
                println!("Processing validate");
                bincode::serialize(&match crate::validate(component.as_str()) {
                    Ok(v) => ipc::DaemonToClientReply::Success::<crate::ValidationResult>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
            ClientRequest::Status => {
                println!("Processing status");
                bincode::serialize(&match crate::status() {
                    Ok(v) => ipc::DaemonToClientReply::Success::<crate::Status>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
        };
        let written = nixsocket::send(client.fd, &r, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)?;
        if written != r.len() {
            bail!("Wrote {} bytes to client, expected {}", written, r.len());
        }
    }
    Ok(())
}
