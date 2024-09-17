import dataclasses
import os
import uuid
from typing import Optional, cast

import tmt
import tmt.base
import tmt.log
import tmt.steps
import tmt.steps.provision
import tmt.steps.provision.testcloud
import tmt.utils
from tmt.steps.provision.testcloud import GuestTestcloud
from tmt.utils import field
from tmt.utils.templates import render_template

DEFAULT_IMAGE_BUILDER = "quay.io/centos-bootc/bootc-image-builder:latest"
CONTAINER_STORAGE_DIR = "/var/lib/containers/storage"


class GuestBootc(GuestTestcloud):
    containerimage: str

    def __init__(self,
                 *,
                 data: tmt.steps.provision.GuestData,
                 name: Optional[str] = None,
                 parent: Optional[tmt.utils.Common] = None,
                 logger: tmt.log.Logger,
                 containerimage: Optional[str]) -> None:
        super().__init__(data=data, logger=logger, parent=parent, name=name)

        if containerimage:
            self.containerimage = containerimage

    def remove(self) -> None:
        tmt.utils.Command(
            "podman", "rmi", self.containerimage
            ).run(cwd=self.workdir, stream_output=True, logger=self._logger)

        super().remove()


@dataclasses.dataclass
class BootcData(tmt.steps.provision.testcloud.ProvisionTestcloudData):
    containerfile: Optional[str] = field(
        default=None,
        option='--containerfile',
        metavar='CONTAINERFILE',
        help="""
             Select container file to be used to build a container image
             that is then used by bootc image builder to create a disk image.

             Cannot be used with containerimage.
             """)

    containerfile_workdir: str = field(
        default=".",
        option=('--containerfile-workdir'),
        metavar='CONTAINERFILE_WORKDIR',
        help="""
             Select working directory for the podman build invocation.
             """)

    containerimage: Optional[str] = field(
        default=None,
        option=('--containerimage'),
        metavar='CONTAINERIMAGE',
        help="""
             Select container image to be used to build a bootc disk.
             This takes priority over containerfile.
             """)

    add_deps: bool = field(
        default=True,
        is_flag=True,
        option=('--add-deps'),
        help="""
             Add tmt dependencies to the supplied container image or image built
             from the supplied Containerfile.
             This will cause a derived image to be built from the supplied image.
             """)

    image_builder: str = field(
        default=DEFAULT_IMAGE_BUILDER,
        option=('--image-builder'),
        metavar='IMAGEBUILDER',
        help="""
             The full repo:tag url of the bootc image builder image to use for
             building the bootc disk image.
             """)


@tmt.steps.provides_method('bootc')
class ProvisionBootc(tmt.steps.provision.ProvisionPlugin[BootcData]):
    """
    Provision a local virtual machine using a bootc container image

    Minimal config which uses the Fedora bootc image:

    .. code-block:: yaml

        provision:
            how: bootc
            containerimage: quay.io/fedora/fedora-bootc:40

    Here's a config example using a containerfile:

    .. code-block:: yaml

        provision:
            how: bootc
            containerfile: "./my-custom-image.containerfile"
            containerfile-workdir: .
            image_builder: quay.io/centos-bootc/bootc-image-builder:stream9
            disk: 100

    Another config example using an image that includes tmt dependencies:

    .. code-block:: yaml

        provision:
            how: bootc
            add_deps: false
            containerimage: localhost/my-image-with-deps

    This plugin is an extension of the virtual.testcloud plugin.
    Essentially, it takes a container image as input, builds a
    bootc disk image from the container image, then uses the virtual.testcloud
    plugin to create a virtual machine using the bootc disk image.

    The bootc disk creation requires running podman as root, this is typically
    done by running the command in a rootful podman-machine. The podman-machine
    also needs access to ``/var/tmp/tmt``. An example command to initialize the
    machine:

    .. code-block:: shell

        podman machine init --rootful --disk-size 200 --memory 8192 \
        --cpus 8 -v /var/tmp/tmt:/var/tmp/tmt -v $HOME:$HOME
    """

    _data_class = BootcData
    _guest_class = GuestTestcloud
    _guest = None
    _id = str(uuid.uuid4())[:8]

    def _get_id(self) -> str:
        # FIXME: cast() - https://github.com/teemtee/tmt/issues/1372
        parent = cast(tmt.steps.provision.Provision, self.parent)
        assert parent.plan is not None
        assert parent.plan.my_run is not None
        assert parent.plan.my_run.unique_id is not None
        return parent.plan.my_run.unique_id

    def _expand_path(self, relative_path: str) -> str:
        """ Expand the path to the full path relative to the current working dir """
        if relative_path.startswith("/"):
            return relative_path
        return f"{os.getcwd()}/{relative_path}"

    def _build_derived_image(self, base_image: str) -> str:
        """ Build a "derived" container image from the base image with tmt dependencies added """
        if not self.workdir:
            raise tmt.utils.ProvisionError(
                "self.workdir must be defined")

        self._logger.debug("Building modified container image with necessary tmt packages/config")
        containerfile_template = '''
            FROM {{ base_image }}

            RUN \
            dnf -y install cloud-init rsync && \
            ln -s ../cloud-init.target /usr/lib/systemd/system/default.target.wants && \
            rm /usr/local -rf && ln -sr /var/usrlocal /usr/local && mkdir -p /var/usrlocal/bin && \
            dnf clean all
        '''
        containerfile_parsed = render_template(
            containerfile_template,
            base_image=base_image)
        (self.workdir / 'Containerfile').write_text(containerfile_parsed)

        image_tag = f'localhost/tmtmodified-{self._get_id()}'
        tmt.utils.Command(
            "podman", "build", f'{self.workdir}',
            "-f", f'{self.workdir}/Containerfile',
            "-t", image_tag
            ).run(cwd=self.workdir, stream_output=True, logger=self._logger)

        return image_tag

    def _build_base_image(self, containerfile: str, workdir: str) -> str:
        """ Build the "base" or user supplied container image """
        image_tag = f'localhost/tmtbase-{self._get_id()}'
        self._logger.debug("Building container image")
        tmt.utils.Command(
            "podman", "build", self._expand_path(workdir),
            "-f", self._expand_path(containerfile),
            "-t", image_tag
            ).run(cwd=self.workdir, stream_output=True, logger=self._logger)
        return image_tag

    def _build_bootc_disk(self, containerimage: str, image_builder: str) -> None:
        """ Build the bootc disk from a container image using bootc image builder """
        self._logger.debug("Building bootc disk image")
        tmt.utils.Command(
            "podman", "run", "--rm", "--privileged",
            "-v", f'{CONTAINER_STORAGE_DIR}:{CONTAINER_STORAGE_DIR}',
            "--security-opt", "label=type:unconfined_t",
            "-v", f"{self.workdir}:/output",
            image_builder, "build",
            "--type", "qcow2",
            "--local", containerimage
            ).run(cwd=self.workdir, stream_output=True, logger=self._logger)

    def go(self, *, logger: Optional[tmt.log.Logger] = None) -> None:
        """ Provision the bootc instance """
        super().go(logger=logger)

        data = BootcData.from_plugin(self)
        data.image = f"file://{self.workdir}/qcow2/disk.qcow2"
        data.show(verbose=self.verbosity_level, logger=self._logger)

        if data.containerimage is not None:
            containerimage = data.containerimage
            if data.add_deps:
                containerimage = self._build_derived_image(data.containerimage)
            self._build_bootc_disk(containerimage, data.image_builder)
        elif data.containerfile is not None:
            containerimage = self._build_base_image(data.containerfile, data.containerfile_workdir)
            if data.add_deps:
                containerimage = self._build_derived_image(containerimage)
            self._build_bootc_disk(containerimage, data.image_builder)
        else:
            raise tmt.utils.ProvisionError(
                "Either containerfile or containerimage must be specified.")

        self._guest = GuestBootc(
            logger=self._logger,
            data=data,
            name=self.name,
            parent=self.step,
            containerimage=containerimage)
        self._guest.start()
        self._guest.setup()

    def guest(self) -> Optional[tmt.steps.provision.Guest]:
        return self._guest
