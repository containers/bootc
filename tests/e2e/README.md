## End to end (e2e) Test

### Scenarios

End to end (e2e) test includes `bootc install to-existing-root`, `bootc install to-disk`, `bootc upgrade`, and `bootc switch` tests

*  bootc install/upgrade/switch scenario will install, upgrade, and switch bootc image and have some system checking, such as check mount point/permission, run podman with root and rootless, check persistent log, etc.

### Run end to end Test

Test run is drived by [Packit](https://packit.dev/) and running on [Testing-farm](https://docs.testing-farm.io/).
