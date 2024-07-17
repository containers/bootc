# Using bootc via API

At the current time, bootc is primarily intended to be
driven via a fork/exec model. The core CLI verbs
are stable and will not change.

## Using `bootc edit` and `bootc status --json --format-version=0`

While bootc does not depend on Kubernetes, it does currently
also offere a Kubernetes *style* API, especially oriented
towards the [spec and status and other conventions](https://kubernetes.io/docs/reference/using-api/api-concepts/).

In general, most use cases of driving bootc via API are probably
most easily done by forking off `bootc upgrade` when desired,
and viewing `bootc status --json --format-version=0`.

## JSON Schema

The current API is classified as `org.containers.bootc/v1alpha1` but
it will likely be officially stabilized mostly as is. However,
you should still request the current "v0" format via an explicit
`--format-version=0` as referenced above.

There is a [JSON schema](https://json-schema.org/) generated from
the Rust source code available here: [host-v0.schema.json](host-v0.schema.json).

A common way to use this is to run a code generator such as
[go-jsonschema](https://github.com/omissis/go-jsonschema) on the
input schema.
