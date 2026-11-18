"""Write a large amount of output, with a carriage-return progress bar on stderr."""

import argparse
import sys


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Flood stdout to exercise the output view.")
    parser.add_argument("--mib", type=float, default=1.9, help="Megabytes of stdout to write.")
    parser.add_argument("--n", type=int, default=200, help="Samples per simulated block.")
    return parser


def main(args: argparse.Namespace) -> None:
    print(f"Simulating n={args.n}", flush=True)
    line = "sample " + "x" * 100
    total = int(args.mib * 1024 * 1024)
    written = 0
    i = 0
    while written < total:
        text = f"{i:08d} {line}\n"
        sys.stdout.write(text)
        written += len(text)
        i += 1
    sys.stdout.flush()
    for pct in (25, 50, 75, 100):
        sys.stderr.write(f"\rfit {pct:3d}%")
        sys.stderr.flush()
    sys.stderr.write("\n")
    print("residual rms 0.0012")


if __name__ == "__main__":
    main(build_parser().parse_args())
