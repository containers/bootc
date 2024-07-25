use std assert
use tap.nu

tap begin "verify bootc-owned container storage"

# This should currently be empty by default...
podman --storage-opt=additionalimagestore=/usr/lib/bootc/storage images
tap ok
