"""Export the day's rows to a text file."""
import argparse


def main(argv=None):
    parser = argparse.ArgumentParser(prog="export")
    parser.add_argument("--output", required=True, help="file to write")
    args = parser.parse_args(argv)
    with open(args.output, "w", encoding="utf-8") as fh:
        fh.write("rows\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
