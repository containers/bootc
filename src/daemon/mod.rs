//! Daemon logic.

use crate::ipc;
use anyhow::{bail, Context, Result};
use nix::sys::socket as nixsocket;
use std::os::unix::io::RawFd;

/// Run daemon core-logic loop, endlessly.
pub fn run_coreloop() -> Result<()> {
    let srvsock_fd = systemd_activation().context("systemd service activation error")?;

    loop {
        // Accept an incoming client.
        let client = match accept_authenticate_client(srvsock_fd) {
            Ok(auth_client) => auth_client,
            Err(e) => {
                log::error!("failed to authenticate client: {}", e);
                continue;
            }
        };

        // Process all requests from this client.
        if let Err(e) = process_client_requests(client) {
            log::error!("failed to process request from client: {}", e);
            continue;
        }
    }
}

/// Perform initialization steps required by systemd service activation.
///
/// This ensures that the system is running under systemd, then receives the
/// socket-FD for main IPC logic, and notifies systemd about ready-state.
fn systemd_activation() -> Result<RawFd> {
    use libsystemd::daemon::{self, NotifyState};
    use std::os::unix::io::IntoRawFd;

    if !daemon::booted() {
        bail!("daemon is not running as a systemd service");
    }

    let srvsock_fd = {
        let mut fds = libsystemd::activation::receive_descriptors(true)
            .map_err(|e| anyhow::anyhow!("failed to receive file-descriptors: {}", e))?;
        let srvsock_fd = if let Some(fd) = fds.pop() {
            fd
        } else {
            bail!("no socket-fd received on service activation");
        };
        srvsock_fd.into_raw_fd()
    };

    let sent = daemon::notify(true, &[NotifyState::Ready])
        .map_err(|e| anyhow::anyhow!("failed to notify ready-state: {}", e))?;
    if !sent {
        log::warn!("failed to notify ready-state: service notifications not supported");
    }

    Ok(srvsock_fd)
}

/// Accept an incoming connection, then authenticate the client.
fn accept_authenticate_client(srvsock_fd: RawFd) -> Result<ipc::AuthenticatedClient> {
    let accepted = nixsocket::accept4(srvsock_fd, nixsocket::SockFlag::SOCK_CLOEXEC)?;
    let client = ipc::UnauthenticatedClient::new(accepted);

    let authed = client.authenticate()?;

    Ok(authed)
}

/// Process all requests from a given client.
///
/// This sequentially processes all requests from a client, until it
/// disconnects or a connection error is encountered.
fn process_client_requests(client: ipc::AuthenticatedClient) -> Result<()> {
    use crate::ClientRequest;

    let mut buf = [0u8; ipc::MSGSIZE];
    loop {
        let n = nixsocket::recv(client.fd, &mut buf, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)?;
        let buf = &buf[0..n];
        if buf.is_empty() {
            log::trace!("client disconnected");
            break;
        }

        let msg = bincode::deserialize(&buf)?;
        let r = match msg {
            ClientRequest::Update { component } => {
                log::trace!("processing 'update' request");
                bincode::serialize(&match crate::update(component.as_str()) {
                    Ok(v) => ipc::DaemonToClientReply::Success::<crate::ComponentUpdateResult>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
            ClientRequest::Validate { component } => {
                log::trace!("processing 'validate' request");
                bincode::serialize(&match crate::validate(component.as_str()) {
                    Ok(v) => ipc::DaemonToClientReply::Success::<crate::ValidationResult>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
            ClientRequest::Status => {
                log::trace!("processing 'status' request");
                bincode::serialize(&match crate::status() {
                    Ok(v) => ipc::DaemonToClientReply::Success::<crate::Status>(v),
                    Err(e) => ipc::DaemonToClientReply::Failure(format!("{:#}", e)),
                })?
            }
        };
        let written = nixsocket::send(client.fd, &r, nixsocket::MsgFlags::MSG_CMSG_CLOEXEC)?;
        if written != r.len() {
            bail!("wrote {} bytes to client, expected {}", written, r.len());
        }
    }
    Ok(())
}
