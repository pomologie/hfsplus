"""Mount only the given disposable software image and detach it on exit."""

from contextlib import contextmanager
import os
import plistlib
import re


HDIUTIL = "/usr/bin/hdiutil"
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
RAW_IMAGE_ARGUMENT = ("-imagekey", "diskimage-class=CRawDiskImage")


def mount_argv(image, mount, owners, *, raw_image):
	image_format = RAW_IMAGE_ARGUMENT if raw_image else ()
	return (HDIUTIL, "attach", "-readonly", "-nobrowse", "-noautoopen", "-owners", owners,
		"-mountpoint", mount, "-plist", *image_format, image)


@contextmanager
def mounted_read_only(run, image, mount, owners, *, raw_image=False):
	attachment = plistlib.loads(run(*mount_argv(image, mount, owners, raw_image=raw_image)))
	entries = attachment["system-entities"]
	devices = [entry["dev-entry"] for entry in entries if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))]
	try:
		if len(devices) != 1 or not any(entry.get("mount-point") == str(mount) for entry in entries):
			raise ValueError("expected the disposable partitionless image at the requested mount point")
		mounted = os.statvfs(mount)
		if not mounted.f_flag & os.ST_RDONLY:
			raise ValueError("expected a read-only mounted volume")
		yield mounted
	finally:
		for device in devices:
			run(HDIUTIL, "detach", device)
