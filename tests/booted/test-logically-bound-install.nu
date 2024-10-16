use std assert
use tap.nu

let images = podman --storage-opt=additionalimagestore=/usr/lib/bootc/storage images --format {{.Repository}} | from csv --noheaders
print "IMAGES:"
podman --storage-opt=additionalimagestore=/usr/lib/bootc/storage images # for debugging
assert ($images | any {|item| $item.column1 == "quay.io/curl/curl"})
assert ($images | any {|item| $item.column1 == "quay.io/curl/curl-base"})
assert ($images | any {|item| $item.column1 == "registry.access.redhat.com/ubi9/podman:latest"}) # this image is signed

tap ok
