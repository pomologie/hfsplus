#!/usr/bin/env python3
"""Keep Rust library and module entry points structural."""

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
TOKEN = re.compile(
    r'\s+|//[^\n]*|/\*|(?:br|cr|r)(?P<hashes>\#*)".*?"(?P=hashes)'
    r'|(?:b|c)?"(?:\\.|[^"\\])*"|b?\'(?:\\.|[^\'\\\n])\''
    r'|(?:r\#)?[^\W\d]\w*|.',
    re.DOTALL,
)
BLOCK_COMMENT = re.compile(r'/\*|\*/')
IDENTIFIER = re.compile(r'(?:r#)?[^\W\d]\w*')
DELIMITERS = {'(': ')', '[': ']', '{': '}'}
REEXPORT_PUNCTUATION = {':', '{', '}', ',', '*'}


def tokens(source):
    offset = 0
    while offset < len(source):
        match = TOKEN.match(source, offset)
        token = match[0]
        offset = match.end()
        if token == '/*':
            depth = 1
            for marker in BLOCK_COMMENT.finditer(source, offset):
                depth += 1 if marker[0] == '/*' else -1
                if depth == 0:
                    offset = marker.end()
                    break
            else:
                raise ValueError('unterminated block comment')
        elif not (token.isspace() or token.startswith('//')):
            yield token, match.start()


def skip_group(items, opening):
    closing = [DELIMITERS[opening]]
    for token, _ in items:
        if token == closing[-1]:
            closing.pop()
            if not closing:
                return
        elif token in DELIMITERS:
            closing.append(DELIMITERS[token])
    raise ValueError(f'unterminated {opening} group')


def first_violation(source):
    """Return the line and start token of the first nonstructural item, if any."""
    source = source.removeprefix('\ufeff')
    items = tokens(source)
    for start, offset in items:
        token = start
        if token == '#':
            token, _ = next(items, ('', 0))
            if token == '!':
                token, _ = next(items, ('', 0))
            if token == '[':
                skip_group(items, token)
                continue

        public = token == 'pub'
        if public:
            token, _ = next(items, ('', 0))
            if token == '(':
                skip_group(items, token)
                token, _ = next(items, ('', 0))

        if token == 'mod':
            name, _ = next(items, ('', 0))
            terminator, _ = next(items, ('', 0))
            if IDENTIFIER.fullmatch(name) and terminator == ';':
                continue
        if public and token == 'use':
            for part, _ in items:
                if part == ';':
                    break
                if part not in REEXPORT_PUNCTUATION and not IDENTIFIER.fullmatch(part):
                    return source.count('\n', 0, offset) + 1, start
            else:
                return source.count('\n', 0, offset) + 1, start
            continue
        return source.count('\n', 0, offset) + 1, start
    return None


def main():
    paths = subprocess.check_output(
        ['git', 'ls-files', '-z', '--cached', '--others', '--exclude-standard', '--',
         ':(glob)**/lib.rs', ':(glob)**/mod.rs'],
        cwd=ROOT,
    ).decode().split('\0')
    errors = []
    checked = 0
    for name in sorted(set(filter(None, paths))):
        path = ROOT / name
        if not path.is_file():
            continue
        checked += 1
        try:
            violation = first_violation(path.read_text(encoding='utf-8'))
        except ValueError as error:
            errors.append(f'{name}: {error}')
            continue
        if violation:
            line, token = violation
            errors.append(f'{name}:{line}: move the item starting with {token!r} into a named module')
    if errors:
        sys.exit('\n'.join(errors))
    print(f'Checked {checked} Rust library and module entry points')


if __name__ == '__main__':
    main()
