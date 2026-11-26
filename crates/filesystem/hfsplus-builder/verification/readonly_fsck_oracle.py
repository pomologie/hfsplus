"""Check only an unmounted read-only software device backed by the generated image."""

import hashlib
import json
import plistlib
import re
import stat


HDIUTIL = "/usr/bin/hdiutil"
FSCK_HFS = "/sbin/fsck_hfs"
ATTACH = (HDIUTIL, "attach", "-readonly", "-nomount", "-nobrowse", "-noautoopen", "-plist",
	"-imagekey", "diskimage-class=CRawDiskImage")
CHECK = (FSCK_HFS, "-fn")
WHOLE_DEVICE = re.compile(r"/dev/disk[0-9]+")
EXPECTED_IMAGE_SHA256 = "16af9f39cddb01e6a2a5c182f70879d3a4e26969660cd3f887c6fd40a4d07182"


def verify_readonly_fsck(image, expected_digest, run):
	if not stat.S_ISREG(image.lstat().st_mode):
		raise ValueError("expected a regular generated image file")
	if expected_digest != EXPECTED_IMAGE_SHA256:
		raise ValueError("native check requires the pinned valid image")
	print(f"Native attachment argv {json.dumps([*ATTACH, '<fixture>'])}")
	attachment = plistlib.loads(run(*ATTACH, image))
	entities = attachment["system-entities"]
	devices = [entry["dev-entry"] for entry in entities
		if WHOLE_DEVICE.fullmatch(entry.get("dev-entry", ""))]
	try:
		if len(devices) != 1 or any(entry.get("mount-point") for entry in entities):
			raise ValueError("expected one unmounted software disk-image device")
		device = devices[0]
		print(f"Native attachment checked unmounted {device}")
		print(f"Native fsck argv {json.dumps([*CHECK, device])}")
		print(run(*CHECK, device).decode("utf-8").strip())
	finally:
		for device in devices:
			run(HDIUTIL, "detach", device)
			print(f"Native attachment detached {device}")
	with image.open("rb") as source:
		digest = hashlib.file_digest(source, "sha256").hexdigest()
	if digest != expected_digest:
		raise ValueError("the no-write check altered the generated image")
	print(f"Native no-write image SHA-256 {digest}")
