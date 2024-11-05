use std assert
use tap.nu

# This list reflects the LBIs specified in bootc/tests/containerfiles/lbi/usr/share/containers/systemd
let expected_images = [
    "quay.io/curl/curl:latest",
    "quay.io/curl/curl-base:latest",
    "registry.access.redhat.com/ubi9/podman:latest" # this image is signed
]

def validate_images [images: table] {
    print $"Validating images ($images)"
    for expected in $expected_images {
        assert ($images | any {|item| $item.image == $expected})
    }
}

# This test checks that bootc actually populated the bootc storage with the LBI images
def test_logically_bound_images_in_storage [] {
    # Use podman to list the images in the bootc storage
    let images = podman --storage-opt=additionalimagestore=/usr/lib/bootc/storage images --format {{.Repository}}:{{.Tag}} | from csv --noheaders | rename --column { column1: image }

    # Debug print
    print "IMAGES:"
    podman --storage-opt=additionalimagestore=/usr/lib/bootc/storage images

    validate_images $images
}

# This test makes sure that bootc itself knows how to list the LBI images in the bootc storage
def test_bootc_image_list [] {
    # Use bootc to list the images in the bootc storage
    let images = bootc image list --type logical --format json | from json

    validate_images $images
}

test_logically_bound_images_in_storage
test_bootc_image_list

tap ok
