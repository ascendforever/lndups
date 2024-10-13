extern crate shlex;
extern crate structopt;
use std::io::{Read, Write, BufReader, BufRead};
use std::path::{Path,PathBuf};
use std::collections::HashMap;
use std::os::linux::fs::MetadataExt as MetadataExtLinux;
use crate::structopt::StructOpt;


macro_rules! s_default_target_separator { () => { ";" } }

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
    about="Hardlink duplicate files recursively\nSymlinks are treated as normal files",
    usage=concat!(env!("CARGO_PKG_NAME"), " [OPTION]... TARGET... ['", s_default_target_separator!(), "' TARGET...]")
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
                help="Prompt once before operating\nDoesn't occurs if no targets are provided")]
    prompt: bool,

    #[structopt(short, long, value_name="VALUE",
                help="Minimum file size to be considered for hardlinking\nNever goes below 1 (the default)")]
    min_size: Option<u64>,

    #[structopt(short, long, value_name="SEPARATOR",
                help=concat!("Separator between sets of targets (default: ", s_default_target_separator!(), ")"))]
    separator: Option<String>,

    #[structopt(long, value_name="FILE",
                help="File to source arguments from (can be '-' for stdin)")]
    argument_file: Option<String>,

    #[structopt(value_name="TARGET",
                help="Target files and directories (recursive)\nEach SEPARATOR denotes a new set of targets\n  Each set of targets are separate from all other sets\n  All targets must be on the same device\nAll symlinks are ignored\n'-' is not treated as special")]
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


fn read_file_lines(path: &Path, dest: &mut Vec<String>) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("File does not exist or is not a normal file ({})", shlex::quote(&path.to_string_lossy())));
    }
    if let Ok(f) = std::fs::File::open(path) {
        let reader = BufReader::new(f);
        for line in reader.lines() {
            match line {
                Ok(line) => dest.push(line),
                Err(err) => return Err(format!("Error reading line: {}", err))
            }
        }
        Ok(())
    } else {
        Err(format!("Could not open {}", shlex::quote(&path.to_string_lossy())))
    }
}


/// may exit
fn process_args() -> (Vec<Vec<PathBuf>>, Config) {
    let mut args = CLIArguments::from_args();
    let verbosity = args.verbose - args.quiet;

    if let Some(arg_file) = args.argument_file {
        if !args.targets.is_empty() {
            eprintln!("No targets should be provided as cli arguments if arguments are being read from file");
            std::process::exit(1);
        }
        let path = Path::new(&arg_file);
        if let Err(s) = read_file_lines(path, &mut args.targets) {
            eprintln!("Error reading argument file: {}", s);
            std::process::exit(1);
        }
    }

    let run_targets: Vec<Vec<String>> = split_vec(&args.targets, &args.separator.unwrap_or(s_default_target_separator!().to_string()));

    if run_targets.is_empty() {
        if verbosity > 0 {
            println!("No targets provided");
        }
        std::process::exit(0);
    }

    if args.prompt {
        if !prompt_confirm(&run_targets) {
            std::process::exit(0);
        }
    }

    let run_paths: Vec<Vec<PathBuf>> = run_targets.iter().enumerate().map(
        |(_,spaths)| spaths.iter().map(
            |spath| Path::new(spath).canonicalize().unwrap_or_else(
                |_| {
                    eprintln!("Failed to retrieve absolute path for {}", shlex::quote(spath));
                    std::process::exit(1);
                }
            )
        ).collect()
    ).collect();


    for paths in &run_paths {
        if let Err(s) = check_all_same_device(paths) {
            eprintln!("{}", s);
            std::process::exit(1);
        }
    }

    (run_paths, Config {
        min_size: args.min_size.map(|v| if v > 1 { v } else { 1 }).unwrap_or(1),
        no_brace_output: args.no_brace_output,
        dry_run: args.dry_run,
        verbosity
    })
}


/// exit on error
fn get_st_dev(file: &PathBuf) -> Result<u64, String> {
    if let Ok(metadata) = std::fs::metadata(file) {
        Ok(metadata.st_dev())
    } else {
        Err(format!("Failed to retrive device id for {}", shlex::quote(&file.to_string_lossy())))
    }
}

fn check_all_same_device(paths: &[PathBuf]) -> Result<(), String> {
    if paths.len() <= 1 {
        return Ok(())
    }
    let first_device_id = get_st_dev(&paths[0])?;
    let mut wrong: Vec<&PathBuf> = Vec::new();
    for path in &paths[1..] {
        if get_st_dev(path)? != first_device_id {
            wrong.push(path);
        }
    }
    if wrong.is_empty() {
        Ok(())
    } else {
        let mut s = String::with_capacity(wrong.len()*128); // 75 max estimated len of path, 53 for prefix msg + nl
        for path in wrong {
            s.push_str("Device ids must all be the same; got different for: {}");
            s.push_str(&shlex::quote(&path.to_string_lossy()));
            s.push_str("\n");
        }
        s.pop(); // remove last newline
        Err(s)
    }
}


/// perform a full run
fn run(paths: Vec<PathBuf>, cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut registry: HashMap<u64, Vec<PathBuf>> = HashMap::new();

    for path in paths {
        register(path.to_path_buf(), &mut registry, cfg);
    }
    registry.retain(|_,files| files.len() >= 2);

    let mut stdout_buffer = if cfg.verbosity >= 0 {
        let stdout = std::io::stdout();
        let stdout_buffer = std::io::BufWriter::new(stdout.lock());
        Some(stdout_buffer)
    } else {
        None
    };

    if let Some(stdout_buffer) = &mut stdout_buffer {
        if cfg.verbosity >= 0 {
            writeln!(stdout_buffer, "Considering {} total files for duplicates", registry.iter().map(|(_,files)| files.len()).sum::<usize>()).unwrap();
        }
    }

    for (fsize, mut files) in registry {
        if files.len() > 8 {
            files.sort_by_key(|path| path.file_name().unwrap_or_default().to_string_lossy().to_string());
        }
        if let Some(stdout_buffer) = &mut stdout_buffer {
            if cfg.verbosity > 1 {
                writeln!(stdout_buffer, "Considering {} files of size {} for duplicates", files.len(), fsize).unwrap();
            }
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
                    if let Some(stdout_buffer) = &mut stdout_buffer {
                        if cfg.verbosity >= 0 {
                            writeln!(stdout_buffer, "hardlinked {}", format_pair(f1, f2, cfg)).unwrap();
                        }
                    }
                }
            }
        }
    }

    Ok(())
}


fn hardlink(f1: &PathBuf, f2: &PathBuf) -> Result<(), &'static str> {
    if let Err(_) = std::fs::remove_file(f2) {
        Err("Failed to remove second file for hardlinking")
    } else if let Err(_) = std::fs::hard_link(f1, f2) { // same as ln in terms of args: left args's inode becomes right arg's inode
        match std::fs::copy(f1, f2) {
            Ok(_) => Err("Failed to hardlink (copied instead)"),
            Err(_) => Err("Failed to hardlink or copy")
        }
    } else {
        Ok(())
    }
}


fn format_pair(f1: &PathBuf, f2: &PathBuf, cfg: &Config) -> String {
    let f1s = f1.to_string_lossy();
    let f2s = f2.to_string_lossy();
    if cfg.no_brace_output {
        return format!(
            "{}  {}",
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
            "{}{{{},{}}}{}",
            shlex::quote(prefix),
            shlex::quote(&f1s[ prefix.len()..std::cmp::max(prefix.len(), f1s.len()-suffix.len()) ]),
            shlex::quote(&f2s[ prefix.len()..std::cmp::max(prefix.len(), f2s.len()-suffix.len()) ]),
            shlex::quote(suffix)
        )
    } else if prefixlong {
        format!(
            "{}{{{},{}}}",
            shlex::quote(prefix),
            shlex::quote(&f1s[prefix.len()..]),
            shlex::quote(&f2s[prefix.len()..])
        )
    } else if suffixlong {
        format!(
            "{{{},{}}}{}",
            shlex::quote(&f1s[..f1s.len()-suffix.len()]),
            shlex::quote(&f2s[..f2s.len()-suffix.len()]),
            shlex::quote(suffix),
        )
    } else {
        format!(
            "{} <-> {}",
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

        if path.is_file() {
            let size = metadata.st_size();
            if size >= cfg.min_size {
                registry.entry(size).or_insert(Vec::new()).push(path);
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
    if let (Ok(mut f1), Ok(mut f2)) = (std::fs::File::open(f1), std::fs::File::open(f2)) {
        cmp_files(&mut f1, &mut f2)
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


fn split_vec(input: &[String], delimiter: &String) -> Vec<Vec<String>> {
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
