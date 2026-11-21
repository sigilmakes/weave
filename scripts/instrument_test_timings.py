#!/usr/bin/env python3
import argparse
from pathlib import Path
import re


TEST_ATTRIBUTE = re.compile(r"(?m)^([ \t]*)#\[(?:test|tokio::test(?:\([^]]*\))?)\]")
TIMING_ATTRIBUTE = "#[pa_test_scratch::pa_test]"


def code_mask(source):
    masked = list(source)
    index = 0
    while index < len(source):
        start = index
        if source.startswith("//", index):
            end = source.find("\n", index)
            index = len(source) if end < 0 else end
        elif source.startswith("/*", index):
            depth = 1
            index += 2
            while index < len(source) and depth:
                if source.startswith("/*", index):
                    depth += 1
                    index += 2
                elif source.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
        elif source[index] in "rb" and (raw := re.match(r'(?:br|r)(#*)"', source[index:])):
            close = '"' + raw[1]
            end = source.find(close, index + raw.end())
            if end < 0:
                raise ValueError("unterminated Rust raw string")
            index = end + len(close)
        elif source[index] == '"':
            index += 1
            while index < len(source):
                if source[index] == "\\":
                    index += 2
                elif source[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
        elif source[index] == "'" and (char := re.match(r"'(?:[^'\\\n]|\\(?:[^u]|u\{[^}]*\}))'", source[index:])):
            index += char.end()
        else:
            index += 1
            continue
        for offset in range(start, index):
            if masked[offset] != "\n":
                masked[offset] = " "
    return "".join(masked)


def instrument(source):
    insertions = []
    for match in TEST_ATTRIBUTE.finditer(code_mask(source)):
        if "tokio::test" in match[0]:
            following = source[match.end():].lstrip().splitlines()
            if following and following[0].strip() == TIMING_ATTRIBUTE:
                continue
            insertions.append((match.end(), "\n" + match[1] + TIMING_ATTRIBUTE))
        else:
            previous = source[:match.start()].rstrip().splitlines()
            if previous and previous[-1].strip() == TIMING_ATTRIBUTE:
                continue
            insertions.append((match.start(), match[1] + TIMING_ATTRIBUTE + "\n"))
    for offset, text in reversed(insertions):
        source = source[:offset] + text + source[offset:]
    return source, len(insertions)


def inventory(repo):
    integration = list((repo / "crates").glob("*/tests/**/*.rs"))
    units = [path for crate in ("pa-core", "pa-daemon")
             for path in (repo / "crates" / crate / "src").rglob("*.rs")]
    return sorted(set(integration + units))


def main():
    parser = argparse.ArgumentParser(description="Add timing attributes to integration tests and pa-core/pa-daemon unit tests.")
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    missing = 0
    files = 0
    for path in inventory(repo):
        source = path.read_text()
        updated, count = instrument(source)
        if count:
            print(f"{path.relative_to(repo)}: {count}")
            files += 1
            missing += count
            if args.write:
                path.write_text(updated)
    print(f"{missing} uninstrumented scoped test attributes in {files} files")
    if missing and not args.write:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
