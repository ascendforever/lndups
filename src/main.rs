extern crate shlex;
extern crate structopt;
use std::io::{Read,Write};
use std::path::{Path,PathBuf};
use std::collections::HashMap;
use std::os::linux::fs::MetadataExt as MetadataExtLinux;
use crate::structopt::StructOpt;


fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (run_paths, cfg) = process_args();
    for paths in run_paths {
        run(paths, &cfg)?;
    }

    Ok(())
}

struct Config {
    dry_run: bool,
    min_size: u64,
    verbosity: i8,
    no_brace_output: bool
}

#[derive(StructOpt)]
#[structopt(
    about="Hardlink duplicate files recursively",
    usage=concat!(env!("CARGO_PKG_NAME"), " [OPTION]... TARGET... [';' TARGET...]")
)]
struct CLIArguments {
    #[structopt(short, long, parse(from_occurrences),
                help="Increase verbosity")]
    verbose: i8,

    #[structopt(short, long, parse(from_occurrences),
                help="Decrease verbosity")]
    quiet: i8,

    #[structopt(long,
                help="Disable brace notation for output\n  Ex: /home/user/{dir,backup}/file")]
    no_brace_output: bool,

    #[structopt(long,
                help="Perform no operations on the filesystem")]
    dry_run: bool,

    #[structopt(short="i",
                help="Prompt once before operating")]
    prompt: bool,

    #[structopt(short, long, value_name="VALUE",
                help="Minimum file size to be considered for hardlinking\nNever goes below 1 (the default)")]
    min_size: Option<u64>,

    #[structopt(value_name="TARGET",
                help="Target files and directories (recursive)\nEach ';' denotes a new set of targets\n  Each set of targets are separate from all other sets\n  All targets must be on the same device\nAll symlinks are ignored")]
    targets: Vec<String>,
}

/// return whether or not user gave confirmation
fn prompt_confirm(run_targets: &Vec<Vec<String>>) -> bool {
    println!("Are you sure you want to link all duplicates in each of these sets of targets?");
    for spaths in run_targets {
        println!("  {}", shlex::join(spaths.iter().map(|string| string.as_str())));
    }
    print!("> ");
    std::io::stdout().flush().unwrap_or_else(|_| ());

    let mut response = String::new();
    std::io::stdin().read_line(&mut response).unwrap_or_else(
        |_| {
            eprintln!("Problem reading input");
            std::process::exit(1);
        }
    );

    response.to_lowercase().starts_with("y")
}

fn process_args() -> (Vec<Vec<PathBuf>>, Config) {
    let args = CLIArguments::from_args();

    let run_targets: Vec<Vec<String>> = split_vec(&args.targets, ";");

    if args.prompt {
        if !prompt_confirm(&run_targets) {
            std::process::exit(0);
        }
    }

    let run_paths: Vec<Vec<PathBuf>> = run_targets.iter().enumerate().map(
        |(i,spaths)| {
            if spaths.len() < 2 {
                eprintln!("Not enough targets for run {} (args: {})", i+1, shlex::join(spaths.iter().map(|string| string.as_str())));
                std::process::exit(1);
            }
            spaths.iter().map(
                |spath| Path::new(spath).canonicalize().unwrap_or_else(
                    |_| {
                        eprintln!("Failed to retrieve absolute path for {}", shlex::quote(spath));
                        std::process::exit(1);
                    }
                )
            ).collect()
        }
    ).collect();


    for paths in &run_paths {
        assert_all_same_device(paths);
    }

    (run_paths, Config {
        min_size: args.min_size.unwrap_or(1),
        no_brace_output: args.no_brace_output,
        dry_run: args.dry_run,
        verbosity: args.verbose - args.quiet
    })
}

/// minimum length of slice = 2
fn assert_all_same_device(paths: &[PathBuf]) {
    let first_device_id = if let Ok(metadata) = std::fs::metadata(&paths[0]) {
        metadata.st_dev()
    } else {
        eprintln!("Failed to retrive device id for {}", shlex::quote(&paths[0].to_string_lossy()));
        std::process::exit(1);
    };
    for path in &paths[1..] {
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.st_dev() != first_device_id {
                eprintln!("Device ids must all be the same; got different for: {}", shlex::quote(&path.to_string_lossy()));
                std::process::exit(1);
            }
        } else {
            eprintln!("Failed to retrive device id for {}", shlex::quote(&path.to_string_lossy()));
            std::process::exit(1);
        }
    }
}

/// perform a full run with pre-processed inputs
fn run(paths: Vec<PathBuf>, cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry: HashMap<u64, Vec<PathBuf>> = HashMap::new();

    for path in paths {
        register(path.to_path_buf(), &mut registry, cfg);
    }

    registry.retain(|_, files| files.len() >= 2);

    let stdout = std::io::stdout();
    let mut stdout_handle = stdout.lock();
    if cfg.verbosity > 0 {
        stdout_handle.write_all(format!("considering {} total files for duplicates\n", registry.iter().map(|(_,files)| files.len()).sum::<usize>()).as_bytes()).unwrap();
    }

    for (fsize, mut files) in registry {
        if files.len() > 8 {
            files.sort_by_key(|path| path.file_name().unwrap_or_default().to_string_lossy().to_string());
        }
        if cfg.verbosity > 1 {
            stdout_handle.write_all(format!("considering {} files of size {} for duplicates\n", files.len(), fsize).as_bytes()).unwrap();
        }
        for i in (0..files.len()).rev() {
            let f1 = &files[i];
            for j in (0..i).rev() {
                let f2 = &files[j];
                if !are_hardlinked(f1, f2) && cmp(f1, f2) {
                    if !cfg.dry_run {
                        if let Err(msg) = hardlink(f1, f2) {
                            eprintln!("{}: {}", msg, format_pair(f1, f2, cfg));
                            continue
                        }
                    }
                    if cfg.verbosity >= 0 {
                        stdout_handle.write_all(b"hardlinked ").unwrap();
                        stdout_handle.write_all(format_pair(f1, f2, cfg).as_bytes()).unwrap();
                    }
                }
            }
        }
    }

    Ok(())
}

fn hardlink(f1: &PathBuf, f2: &PathBuf) -> Result<(), &'static str> {
    if let Err(_) = std::fs::remove_file(f2) {
        Err("failed to remove second file for hardlinking")
    } else if let Err(_) = std::fs::hard_link(f1, f2) { // same as ln in terms of args: left args's inode becomes right arg's inode
        match std::fs::copy(f1, f2) {
            Ok(_) => Err("failed to hardlink (copied instead)"),
            Err(_) => Err("failed to hardlink or copy")
        }
    } else {
        Ok(())
    }
}

/// adds newline at the end
fn format_pair(f1: &PathBuf, f2: &PathBuf, cfg: &Config) -> String {
    let f1s = f1.to_string_lossy();
    let f2s = f2.to_string_lossy();
    if cfg.no_brace_output {
        return format!(
            "hardlinked {}  {}\n",
            shlex::quote(&f1s),
            shlex::quote(&f2s)
        )
    }

    let prefix = common_prefix(&f1s, &f2s);
    let suffix = common_suffix(&f1s, &f2s);
    let prefixlong = prefix.len() > 2;
    let suffixlong = suffix.len() > 2;
    if prefixlong && suffixlong {
        format!(
            "hardlinked {}{{{},{}}}{}\n",
            shlex::quote(prefix),
            shlex::quote(&f1s[ prefix.len()..std::cmp::max(prefix.len(), f1s.len()-suffix.len()) ]),
            shlex::quote(&f2s[ prefix.len()..std::cmp::max(prefix.len(), f2s.len()-suffix.len()) ]),
            shlex::quote(suffix)
        )
    } else if prefixlong {
        format!(
            "hardlinked {}{{{},{}}}\n",
            shlex::quote(prefix),
            shlex::quote(&f1s[prefix.len()..]),
            shlex::quote(&f2s[prefix.len()..])
        )
    } else if suffixlong {
        format!(
            "hardlinked {{{},{}}}{}\n",
            shlex::quote(&f1s[..f1s.len()-suffix.len()]),
            shlex::quote(&f2s[..f2s.len()-suffix.len()]),
            shlex::quote(suffix),
        )
    } else {
        format!(
            "hardlinked {}  {}\n",
            shlex::quote(&f1s),
            shlex::quote(&f2s)
        )
    }
}


/// recursively register path or its contents if directory into registry
fn register(path: PathBuf, registry: &mut HashMap<u64, Vec<PathBuf>>, cfg: &Config) {
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() {
            return
        }
        if metadata.st_size() < cfg.min_size {
            return
        }
    } else { return }
    if path.is_file() {
        if let Some(size) = std::fs::metadata(&path).ok().map(|meta| meta.len()) {
            if size != 0 {
                registry.entry(size).or_insert(Vec::new()).push(path);
            }
        }
    } else if path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries {
                if let Ok(entry) = entry {
                    register(entry.path(), registry, cfg);
                }
            }
        }
    }
}

fn are_hardlinked(f1: &PathBuf, f2: &PathBuf) -> bool {
    if let (Ok(md1), Ok(md2)) = (std::fs::metadata(f1), std::fs::metadata(f2)) {
        md1.st_ino() == md2.st_ino()
    } else {
        false
    }
}

/// check equality of contents of two paths to files
fn cmp(f1: &PathBuf, f2: &PathBuf) -> bool {
    if let Ok(mut f1) = std::fs::File::open(f1) {
        if let Ok(mut f2) = std::fs::File::open(f2) {
            cmp_files(&mut f1, &mut f2)
        } else { false }
    } else { false }
}

/// check equality of contents of two open files
fn cmp_files(f1: &mut std::fs::File, f2: &mut std::fs::File) -> bool {
    let buff1: &mut[u8] = &mut [0; 1024];
    let buff2: &mut[u8] = &mut [0; 1024];
    loop {
        match f1.read(buff1) {
            Err(_) => return false,
            Ok(readlen1) => match f2.read(buff2) {
                Err(_) => return false,
                Ok(readlen2) => {
                    if readlen1 != readlen2 {
                        return false;
                    }
                    if readlen1 == 0 {
                        return true;
                    }
                    if &buff1[0..readlen1] != &buff2[0..readlen2] {
                        return false;
                    }
                }
            }
        }
    }
}

fn common_prefix<'a>(s1: &'a str, s2: &'a str) -> &'a str {
    let len = s1
        .chars()
        .zip(s2.chars())
        .take_while(|(char1, char2)| char1 == char2)
        .count();
    &s1[..len]
}
fn common_suffix<'a>(s1: &'a str, s2: &'a str) -> &'a str {
    let len = s1
        .chars()
        .rev()
        .zip(s2.chars().rev())
        .take_while(|(char1, char2)| char1 == char2)
        .count();
    &s1[s1.len() - len..]
}

fn split_vec(input: &[String], delimiter: &str) -> Vec<Vec<String>> {
    let mut result: Vec<Vec<String>> = Vec::new();
    let mut current_vec: Vec<String> = Vec::new();
    for item in input.iter() {
        if item == delimiter {
            if !current_vec.is_empty() {
                result.push(current_vec);
            }
            current_vec = Vec::new();
        } else {
            current_vec.push(item.to_string());
        }
    }
    if !current_vec.is_empty() {
        result.push(current_vec);
    }
    result
}
