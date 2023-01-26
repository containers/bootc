---
nav_order: 1
---

# Rationale

The bootc project starts from a basic premise: Docker/OCI style containers
are successful, what if we supported putting a Linux kernel binary inside
one too, and created client tooling (like `docker`/`podman`) that understood
how to use container images for in-place transactional (default stateful)
operating system upgrades.

With `bootc`, bootable operating systems can be created and deployed using all the same
familiar tools and techniques one uses for *application* container images.
