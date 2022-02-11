//! # Losslessly export and import ostree commits as tar archives
//!
//! Convert an ostree commit into a tarball stream, and import it again, including
//! support for OSTree signature verification.
//!
//! In the current libostree C library, while it supports export to tar, this
//! process is lossy - commit metadata is discarded.  Further, re-importing
//! requires recalculating all of the object checksums, and tying these
//! together, it does not support verifying ostree level cryptographic signatures
//! such as GPG/ed25519.
//!
//! # Tar stream layout
//!
//! In order to solve these problems, this new tar serialization format effectively
//! combines *both* a `/sysroot/ostree/repo/objects` directory and a checkout in `/usr`,
//! where the latter are hardlinks to the former.
//!
//! The exported stream will have the ostree metadata first; in particular the commit object.
//! Following the commit object is the `.commitmeta` object, which contains any cryptographic
//! signatures.
//!
//! This library then supports verifying the pair of (commit, commitmeta) using an ostree
//! remote, in the same way that `ostree pull` will do.
//!
//! The remainder of the stream is a breadth-first traversal of dirtree/dirmeta objects and the
//! content objects they reference.
//!
//! # `bare-split-xattrs` repository mode
//!
//! In format version 1, the tar stream embeds a proper ostree repository using a tailored
//! `bare-split-xattrs` mode.
//!
//! This is because extended attributes (xattrs) are a complex subject for tar, which has
//! many variants.
//! Further, when exporting bootable ostree commits to container images, it is not actually
//! desired to have the container runtime try to unpack and apply those.
//!
//! For these reasons, extended attributes (xattrs) get serialized into detached objects
//! which are associated with the relevant content objects.
//!
//! At a low level, two dedicated object types are used:
//!  * `file-xattrs` as regular files storing (and de-duplicating) xattrs content.
//!  * `file-xattrs-link` as hardlinks which associate a `file` object to its corresponding
//!    `file-xattrs` object.

mod import;
pub use import::*;
mod export;
pub use export::*;
mod write;
pub use write::*;
