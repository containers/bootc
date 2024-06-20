# This test does:
# bootc image push
# podman build <from that image>
# bootc switch <to the local image>
use std assert
use tap.nu

# This code runs on *each* boot.
# Here we just capture information.
bootc status
let st = bootc status --json | from json
let booted = $st.status.booted.image.image

# Run on the first boot
def initial_build [] {
    tap begin "local image push + pull + upgrade"

    let td = mktemp -d
    cd $td

    do --ignore-errors { podman image rm localhost/bootc o+e>| ignore }
    bootc image push
    let img = podman image inspect localhost/bootc | from json

    # A simple derived container
    "FROM localhost/bootc
RUN echo test content > /usr/share/blah.txt
" | save Dockerfile
    # Build it
    podman build -t localhost/bootc-derived .
    # Just sanity check it
    let v = podman run --rm localhost/bootc-derived cat /usr/share/blah.txt | str trim
    assert equal $v "test content"
    # Now, fetch it back into the bootc storage!
    bootc switch --transport containers-storage localhost/bootc-derived
    # And reboot into it
    tmt-reboot
}

# The second boot; verify we're in the derived image
def second_boot [] {
    assert equal $booted.transport containers-storage
    assert equal $booted.image localhost/bootc-derived
    let t = open /usr/share/blah.txt | str trim
    assert equal $t "test content"
    tap ok
}

def main [] {
    # See https://tmt.readthedocs.io/en/stable/stories/features.html#reboot-during-test
    match $env.TMT_REBOOT_COUNT? {
        null | "0" => initial_build,
        "1" => second_boot,
        $o => { error make {msg: $"Invalid TMT_REBOOT_COUNT ($o)" } },
    }
}
