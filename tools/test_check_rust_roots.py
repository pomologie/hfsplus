"""Regression tests for the Rust entry point source check."""

import unittest

from check_rust_roots import first_violation


class RustRootTests(unittest.TestCase):
    def test_structural_items_and_attributes_are_allowed(self):
        source = '''
//! Crate documentation
#![no_std]
#![cfg_attr(not(feature = "std"), forbid(unsafe_code))]
#![doc = include_str!("../README.md")]
/* A /* nested */ block comment. */
#[cfg(kani)]
#[path = "../verification/kani/proof.rs"]
mod proof;
#[cfg_attr(all(feature = "docs", unix),
    doc = r#"A comma, a bracket ], and a quote " in documentation."#,
    cfg_attr(unix, cfg(feature = "api"), doc(hidden)),
)]
pub(crate) mod api;
pub use api::{Service, Read as Reader, r#type as foo\u0301};
pub(in crate::scope) use api::Path;
'''
        self.assertIsNone(first_violation(source))

    def test_definitions_are_rejected_even_when_disabled(self):
        declarations = [
            'struct Record;',
            'enum Kind { One }',
            'trait Read {}',
            'impl Record { fn read() {} }',
            'fn read() {}',
            'const PAGE_SIZE: usize = 4096;',
            'static COUNTER: usize = 0;',
            'type Id = u64;',
            'mod tests { #[test] fn reads() {} }',
            'include!("generated.rs");',
            'use api::Service;',
        ]
        for declaration in declarations:
            with self.subTest(declaration=declaration):
                source = f'mod api;\n#[cfg(any())]\n{declaration}'
                violation = first_violation(source)
                self.assertIsNotNone(violation)
                self.assertEqual(violation[0], 3)

    def test_code_generating_outer_attributes_are_rejected(self):
        attributes = [
            '#[generate]',
            '#[macros::generate]',
            '#[cfg_attr(unix, generate)]',
            '#[cfg_attr(unix, doc(hidden), cfg_attr(unix, macros::generate))]',
            '#[cfg_attr(unix, cfg_attr(unix, doc(hidden)), generate)]',
        ]
        for attribute in attributes:
            for declaration in ('pub mod api;', 'pub use api::Service;'):
                with self.subTest(attribute=attribute, declaration=declaration):
                    source = f'//! Crate documentation\n{attribute}\n{declaration}'
                    violation = first_violation(source)
                    self.assertIsNotNone(violation)
                    self.assertEqual(violation[0], 2)

    def test_inline_definitions_in_disabled_attributes_are_rejected(self):
        attributes = [
            '#[cfg_attr(any(), path = { fn read() {} "api.rs" })]',
            '#![cfg_attr(any(), doc = { fn read() {} "documentation" })]',
        ]
        for attribute in attributes:
            with self.subTest(attribute=attribute):
                self.assertIsNotNone(first_violation(f'{attribute}\nmod api;'))


if __name__ == '__main__':
    unittest.main()
