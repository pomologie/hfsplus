"""Exercise the kernel graph checker against real Cargo resolution."""

from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

from check_kernel_graph import TARGETS, enforce, target_graph


ENGINE = "fixture-engine"
HOST_LIBRARY = "fixture-host-library"
HOST_MACRO = "fixture-host-macro"
HOST_REGRESSION = "host-regression"


def write_package(root, name, manifest="", source="#![no_std]\n"):
    directory = root / name
    (directory / "src").mkdir(parents=True)
    (directory / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2024"\n{manifest}',
        encoding="utf-8",
    )
    (directory / "src" / "lib.rs").write_text(source, encoding="utf-8")


class KernelGraphTests(unittest.TestCase):
    def test_target_graph_accepts_host_proc_macros_and_rejects_std_dependencies(self):
        with TemporaryDirectory(prefix="hfsplus-kernel-graph-") as temporary:
            root = Path(temporary)
            write_package(
                root, HOST_LIBRARY, source="use std::path::Path;\npub fn path() -> &'static Path { Path::new(\"\") }\n",
            )
            write_package(root, HOST_MACRO, f'''
[lib]
proc-macro = true

[dependencies]
{HOST_LIBRARY} = {{ path = "../{HOST_LIBRARY}" }}
''', source="extern crate proc_macro;\n")
            write_package(root, ENGINE, f'''
[workspace]
resolver = "3"

[features]
{HOST_REGRESSION} = ["dep:{HOST_LIBRARY}"]

[dependencies]
{HOST_MACRO} = {{ path = "../{HOST_MACRO}" }}
{HOST_LIBRARY} = {{ path = "../{HOST_LIBRARY}", optional = true }}
''')
            for target in TARGETS:
                with self.subTest(target=target):
                    engine = root / ENGINE
                    enforce(target_graph(engine, ENGINE, target, locked=False), {ENGINE: set()})
                    leaked = target_graph(engine, ENGINE, target, (HOST_REGRESSION,), locked=True)
                    with self.assertRaisesRegex(ValueError, rf"unexpected: \['{HOST_LIBRARY}'\]"):
                        enforce(leaked, {ENGINE: {HOST_REGRESSION}})

    def test_rejects_feature_leak_without_a_new_dependency(self):
        with self.assertRaisesRegex(ValueError, "changed features"):
            enforce({ENGINE: {HOST_REGRESSION}}, {ENGINE: set()})


if __name__ == "__main__":
    unittest.main()
