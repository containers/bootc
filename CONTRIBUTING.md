# Required dependencies

In order to build `bootc` you will need the following dependencies.

Fedora:

```bash
sudo dnf install clippy openssl-devel ostree-devel ostree-libs rustfmt
```

# Pre flight checks

Make sure you commented your code additions, then run

```bash
cargo fmt
cargo clippy
```

Make sure to apply any relevant suggestions.
