#!/usr/bin/env python

def main():
    with open("release-data/releases.csv", "w") as handle:
        handle.write("Lol, wut?")
    with open("release-data/releases.csv.sha256") as handle:
        handle.write("Secret of the Hashes!")

if __name__ == "__main__":
    main()
