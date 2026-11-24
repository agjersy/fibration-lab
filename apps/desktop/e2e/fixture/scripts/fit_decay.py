"""Fit a decay model to a CSV file of t,y samples."""

import argparse
import json
import sys

import numpy as np

from analysis.models import single_exp


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Fit a decay model to measured data.")
    parser.add_argument("input", help="CSV file with t,y columns")
    parser.add_argument("--model", choices=["single", "double"], default="single", help="model to fit")
    parser.add_argument("--label", required=True, help="label written into the results")
    parser.add_argument("--bounds", type=float, nargs=2, default=[0.1, 10.0], help="lower and upper bound for tau")
    parser.add_argument("--no-plot", dest="plot", action="store_false", help="skip the figure")
    parser.add_argument("--log-scale", action="store_true", help="logarithmic y axis")
    parser.add_argument("--weights", action=argparse.BooleanOptionalAction, default=True, help="weight points by their uncertainty")
    parser.add_argument("-v", "--verbose", action="count", default=0, help="more logging")
    return parser


def main(args: argparse.Namespace, extra: list[str]) -> None:
    print("argv: " + json.dumps(sys.argv[1:], separators=(",", ":")))
    t, y = np.loadtxt(args.input, delimiter=",", skiprows=1, unpack=True)
    print(f"{args.label}: {len(t)} samples, model={args.model}, bounds={args.bounds}, extra={extra}")
    print(single_exp(t, 1.0, 2.5)[:3], y[:3])


if __name__ == "__main__":
    known, rest = build_parser().parse_known_args()
    main(known, rest)
