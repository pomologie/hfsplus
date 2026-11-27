#!/usr/bin/env python3
"""Compare the retained Apple optical catalog with p7zip's HFS+ view."""

from fnmatch import fnmatchcase
from hashlib import sha256
import json
from pathlib import Path
import subprocess
import sys


VERIFICATION = Path(__file__).resolve().parent
FIXTURES = VERIFICATION.parent / "tests/fixtures"
FIXTURE = "native-catalog-volume.bin"
LEDGER = "native-catalog-volume.json"
ROOT_CNID = 2
CATALOG_ROW = "HFS+ record catalog|"
WIRE_TIMEOUT_SECONDS = 180
P7ZIP_TIMEOUT_SECONDS = 30


def catalog_entries(wire, expected_records):
	objects, threads, lengths = {}, {}, {}
	for row in wire.splitlines():
		if not row.startswith(CATALOG_ROW):
			continue
		_, _, _, _, _, parent, units, kind, *value = row.split("|")
		if kind in {"folder", "file"}:
			cnid = int(value[0])
			if cnid in objects:
				raise ValueError(f"duplicate catalog object CNID {cnid}")
			objects[cnid] = int(parent), tuple(json.loads(units)), kind
			lengths[cnid] = json.loads(value[-2])[0] if kind == "file" else None
		elif kind in {"folder-thread", "file-thread"}:
			cnid = int(parent)
			if cnid in threads or json.loads(units):
				raise ValueError(f"duplicate or incorrectly keyed catalog thread CNID {cnid}")
			threads[cnid] = int(value[0]), tuple(json.loads(value[1])), kind.removesuffix("-thread")
	if objects != threads or 2 * len(objects) != expected_records:
		raise ValueError("catalog objects and threads must match by CNID, parent, name and kind")

	paths = {}
	for cnid, (_, _, kind) in objects.items():
		components = []
		current = cnid
		seen = set()
		while current in objects and current not in seen:
			seen.add(current)
			parent, units, _ = objects[current]
			components.append(b"".join(unit.to_bytes(2, "big") for unit in units).decode("utf-16-be"))
			if current == ROOT_CNID:
				break
			current = parent
		else:
			raise ValueError(f"catalog path for CNID {cnid} does not reach root")
		path = "/".join(reversed(components))
		if path in paths:
			raise ValueError(f"duplicate catalog path {path!r}")
		paths[path] = kind, lengths[cnid]
	return paths


def p7zip_entries(listing, tool):
	header, separator, listing = listing.partition("\n----------\n")
	name, version = tool.split(maxsplit=1)
	if not separator or f"{name} Version {version} " not in header:
		raise ValueError(f"expected listing from {tool}")
	paths = {}
	for paragraph in listing.strip().split("\n\n"):
		record = dict(line.split(" = ", 1) for line in paragraph.splitlines() if " = " in line)
		path, folder = record["Path"], record["Folder"]
		if path in paths or folder not in {"+", "-"}:
			raise ValueError(f"invalid p7zip listing entry {path!r}")
		paths[path] = ("folder", None) if folder == "+" else ("file", int(record["Size"]))
	return paths


def run(argv, *, cwd, timeout):
	result = subprocess.run(argv, cwd=cwd, timeout=timeout, capture_output=True)
	if result.returncode:
		diagnostic = (result.stdout + result.stderr).decode("utf-8", errors="replace")
		raise ValueError(f"{argv[0]} exited with {result.returncode}\n{diagnostic}")
	return result.stdout


def main():
	ledger = json.loads((FIXTURES / LEDGER).read_bytes())
	fixture = FIXTURES / FIXTURE
	if sha256(fixture.read_bytes()).hexdigest() != ledger["bounded_volume_sha256"]:
		raise ValueError("retained optical capture digest differs from its ledger")
	wire = run([sys.executable, "-B", VERIFICATION / "check-packed-wire.py"],
		cwd=VERIFICATION, timeout=WIRE_TIMEOUT_SECONDS).decode("utf-8")
	entries = catalog_entries(wire, ledger["catalog"]["leaf_records"])
	oracle = ledger["differential_check"]
	listing = run(oracle["listing_argv"], cwd=FIXTURES, timeout=P7ZIP_TIMEOUT_SECONDS).decode("utf-8")
	observed = p7zip_entries(listing, oracle["tool"])
	if entries != observed:
		different = sorted(path for path in entries.keys() | observed.keys()
			if entries.get(path) != observed.get(path))
		raise ValueError(f"catalog and p7zip disagree at {different[0]!r}")
	pattern = oracle["extraction_argv"][-1]
	matches = [item for path, item in entries.items() if fnmatchcase(path, pattern)]
	payload = run(oracle["extraction_argv"], cwd=FIXTURES, timeout=P7ZIP_TIMEOUT_SECONDS)
	digest = sha256(payload).hexdigest()
	if matches != [("file", len(payload))] or digest != oracle["payload_sha256"]:
		raise ValueError("p7zip payload does not match its single catalog entry and pinned digest")
	if sha256(fixture.read_bytes()).hexdigest() != ledger["bounded_volume_sha256"]:
		raise ValueError("retained optical capture changed during comparison")
	print(f"Matched {len(entries)} catalog objects and threads with p7zip paths, kinds, and file lengths")
	print(f"Matched {len(payload)} payload bytes SHA-256 {digest}")
	print(f"Unchanged optical capture SHA-256 {ledger['bounded_volume_sha256']}")


if __name__ == "__main__":
	try:
		main()
	except (OSError, KeyError, ValueError, subprocess.SubprocessError) as error:
		sys.exit(str(error))
