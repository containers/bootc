# Using bootc via API

At the current time, bootc is primarily intended to be
driven via a fork/exec model. The core CLI verbs
are stable and will not change.

The core API is meant to be used as a "state machine": run a command,
such as `bootc upgrade --check` or `bootc switch`, and then query the
updated state with `bootc status --json`. The status command contains all
relevant state about the current deployment, including cached results
from `bootc upgrade --check`.

## `bootc status` JSON Schema

`bootc status --json --format-version=1` outputs a machine-readable JSON
that contains all relevant information about the current
deployment. This API is considered stable. You can find a
[JSON schema](https://json-schema.org/) describing the
version `org.containers.bootc/v1` here:
[host-v1.schema.json](host-v1.schema.json). This schema was generated
directly from the Rust bootc code.
You can either reference or feed it to a code generator such as
[go-jsonschema](https://github.com/omissis/go-jsonschema) to generate
client bindings.

In order to be forwards compatible with a future introduction of
a v2 or newer format, please include an explicit version in your
status request with `--format-version=1` as referenced above.
(Available since bootc 0.1.15, `--format-version=0` in bootc 0.1.14).

## Interactive progress with `--json-fd`

While the `bootc status` tooling allows a client to discover the state
of the system, during interactive changes such as `bootc upgrade`
or `bootc switch` it is possible to monitor the status of downloads
or other operations at a fine-grained level with `--json-fd`.

The format of data output over `--json-fd` is [JSON Lines](https://jsonlines.org)
which is a series of JSON objects separated by newlines (the intermediate
JSON content is guaranteed not to contain a literal newline).

The current API version is `org.containers.bootc/progress/v1`. You can find
the JSON schema describing this version here:
[progress-v1.schema.json](progress-v1.schema.json).

Deploying a new image with either switch or upgrade consists
of three stages: `pulling`, `importing`, and `staging`. The `pulling` step
downloads the image from the registry, offering per-layer and progress in
each message. The `importing` step imports the image into storage and consists
of a single step. Finally, `staging` runs a variety of staging
tasks. Currently, they are staging the image to disk, pulling bound images,
and removing old images.

Note that new stages or fields may be added at any time.

Importing and staging are affected by disk speed and the total image size. Pulling
is affected by network speed and how many layers invalidate between pulls.
Therefore, a large image with a good caching strategy will have longer
importing and staging times, and a small bespoke container image will have
negligible importing and staging times.

## Using `bootc edit`

While bootc does not depend on Kubernetes, it does currently
also offer a Kubernetes *style* API, especially oriented
towards the [spec and status and other conventions](https://kubernetes.io/docs/reference/using-api/api-concepts/).

In general, most use cases of driving bootc via API are probably
most easily done by forking off `bootc upgrade` when desired,
and viewing `bootc status --json --format-version=1`.
