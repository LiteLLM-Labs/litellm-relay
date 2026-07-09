from __future__ import annotations

import argparse
import asyncio
import sys

from .config import RelayConfig
from .pac import build_pac
from .proxy import RelayProxy


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="litellm-relay")
    subparsers = parser.add_subparsers(dest="command")
    subparsers.add_parser("serve", help="Run the local Relay proxy")
    subparsers.add_parser("pac", help="Print the PAC file served by Relay")
    args = parser.parse_args(argv)

    config = RelayConfig.from_env()
    if args.command == "pac":
        sys.stdout.write(build_pac(config))
        return 0
    if args.command in {None, "serve"}:
        try:
            asyncio.run(RelayProxy(config).serve_forever())
        except KeyboardInterrupt:
            return 130
        return 0
    parser.print_help()
    return 2


if __name__ == "__main__":
    raise SystemExit(main())

