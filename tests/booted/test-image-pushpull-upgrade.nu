# This test does:
# bootc image copy-to-storage
# podman build <from that image>
# bootc switch <to the local image>
# <verify booted state>
# Then another build, and reboot into verifying that
use std assert
use tap.nu

const kargsv0 = ["testarg=foo", "othertestkarg", "thirdkarg=bar"]
const kargsv1 = ["testarg=foo", "thirdkarg=baz"]
let removed = ($kargsv0 | filter { not ($in in $kargsv1) })

# This code runs on *each* boot.
# Here we just capture information.
bootc status
let st = bootc status --json | from json
let booted = $st.status.booted.image

# Parse the kernel commandline into a list.
# This is not a proper parser, but good enough
# for what we need here.
def parse_cmdline []  {
    open /proc/cmdline | str trim | split row " "
}

# Run on the first boot
def initial_build [] {
    tap begin "local image push + pull + upgrade"

    let td = mktemp -d
    cd $td

    bootc image copy-to-storage
    let img = podman image inspect localhost/bootc | from json

    mkdir usr/lib/bootc/kargs.d
    { kargs: $kargsv0 } | to toml | save usr/lib/bootc/kargs.d/05-testkargs.toml
    # A simple derived container that adds a file, but also injects some kargs
    "FROM localhost/bootc
COPY usr/ /usr/
RUN echo test content > /usr/share/blah.txt
" | save Dockerfile
    # Build it
    podman build -t localhost/bootc-derived .
    # Just sanity check it
    let v = podman run --rm localhost/bootc-derived cat /usr/share/blah.txt | str trim
    assert equal $v "test content"

    let orig_root_mtime = ls -Dl /ostree/bootc | get modified

    # Now, fetch it back into the bootc storage!
    bootc switch --transport containers-storage localhost/bootc-derived

    # Also test that the mtime changes on modification
    let new_root_mtime = ls -Dl /ostree/bootc | get modified
    assert ($new_root_mtime > $orig_root_mtime)

    # And reboot into it
    tmt-reboot
}

# The second boot; verify we're in the derived image
def second_boot [] {
    print "verifying second boot"
    # booted from the local container storage and image
    assert equal $booted.image.transport containers-storage
    assert equal $booted.image.image localhost/bootc-derived
    # We wrote this file
    let t = open /usr/share/blah.txt | str trim
    assert equal $t "test content"

    # Verify we have updated kargs
    let cmdline = parse_cmdline
    print $"cmdline=($cmdline)"
    for x in $kargsv0 {
        print $"verifying karg: ($x)"
        assert ($x in $cmdline)
    }

    # Now do another build where we drop one of the kargs
    let td = mktemp -d
    cd $td

    mkdir usr/lib/bootc/kargs.d
    { kargs: $kargsv1 } | to toml | save usr/lib/bootc/kargs.d/05-testkargs.toml
    "FROM localhost/bootc
COPY usr/ /usr/
RUN echo test content2 > /usr/share/blah.txt
" | save Dockerfile
    # Build it
    podman build -t localhost/bootc-derived .
    let booted_digest = $booted.imageDigest
    print $"booted_digest = ($booted_digest)"
    # We should already be fetching updates from container storage
    bootc upgrade
    # Verify we staged an update
    let st = bootc status --json | from json
    let staged_digest = $st.status.staged.image.imageDigest
    assert ($booted_digest != $staged_digest)
    # And reboot into the upgrade
    tmt-reboot
}

# Check we have the updated kargs
def third_boot [] {
    print "verifying third boot"
    assert equal $booted.image.transport containers-storage
    assert equal $booted.image.image localhost/bootc-derived
    let t = open /usr/share/blah.txt | str trim
    assert equal $t "test content2"

    # Verify we have updated kargs
    let cmdline = parse_cmdline
    print $"cmdline=($cmdline)"
    for x in $kargsv1 {
        print $"Verifying karg ($x)"
        assert ($x in $cmdline)
    }
    # And the kargs that should be removed are gone
    for x in $removed {
        assert not ($removed in $cmdline)
    }

    tap ok
}

def main [] {
    # See https://tmt.readthedocs.io/en/stable/stories/features.html#reboot-during-test
    match $env.TMT_REBOOT_COUNT? {
        null | "0" => initial_build,
        "1" => second_boot,
        "2" => third_boot,
        $o => { error make { msg: $"Invalid TMT_REBOOT_COUNT ($o)" } },
    }
}
