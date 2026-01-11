# lndups
```
Hardlink duplicate files recursively

This tool should only be used when you are sure that duplicate files should remain duplicate in perpetuity

Usage: lndups [OPTIONS] [TARGET]...

Arguments:
  [TARGET]...  Target files and directories (recursive)
                 Each SEPARATOR denotes a new set of targets
                   Each set of targets are separate from all other sets
                   All targets in a set must be on the same device
                 Symlinks are ignored
                 '-' is not treated as special
                 Mutually exclusive with --target-file

Options:
  -v, --verbose...             Increase verbosity
  -q, --quiet...               Decrease verbosity
  -r, --raw-output             Show only hardlink operations and errors, in an easily parseable format
                                 Outputs two columns separated by a tab
                                 Bypasses verbosity
  -n, --no-brace-output        Disable brace notation for output
                                 Ex: /home/user/{dir,backup}/file
  -d, --dry-run                Perform no operations on the filesystem
  -i, --prompt                 Prompt once before operating
                                 Doesn't occurs if no targets are provided
  -m, --min-size <SIZE>        Minimum file size to be considered for hardlinking
                                 Never goes below 1 [default: 1]
  -t, --threads <NUMBER>       Number of threads [default: 2]
  -s, --separator <SEPARATOR>  Separator between sets of targets [default: ;]
  -f, --target-file <FILE>     File to source targets from (can be '-' for stdin)
                                 Same rules as CLI argument targets apply
                                 Mutually exclusive with CLI argument targets
  -h, --help                   Print help
```


## Installation
Linux is the sole supported OS

4 ways to download and install:
1. `cargo install lndups`
2. `curl -O 'http://esil537kn3ooe3zwjqf4ybka5razkzpff6krdqgspv36yaxdu67iv7ad.onion/ascendforever/lndups/raw/branch/master/prebuilt-x86-64-linux/lndups'`
3. `curl -O 'https://git.ascendforever.com/ascendforever/lndups/raw/branch/master/prebuilt-x86-64-linux/lndups'`
4. `curl -O 'https://github.com/ascendforever/lndups/raw/refs/heads/master/prebuilt-x86-64-linux/lndups'`
