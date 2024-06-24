use std assert
use tap.nu

tap begin "verify bootc status --json looks sane"

let st = bootc status --json | from json
assert equal $st.apiVersion org.containers.bootc/v1alpha1
tap ok
