"""Run persistent control echoes with a contention measurement timeout."""

import argparse

from traffic import sample, serve

if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("serve", "sample"))
    parser.add_argument("--destination")
    parser.add_argument("--output")
    parser.add_argument("--control", action="store_true")
    arguments = parser.parse_args()
    if arguments.mode == "serve":
        serve(True)
    else:
        sample(arguments.destination, arguments.output, True, response_timeout=0.5)
