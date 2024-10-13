extern crate shlex;
extern crate structopt;
use std::borrow::Borrow;
use std::io::{Read, Write, BufReader, BufRead};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::os::linux::fs::MetadataExt as MetadataExtLinux;
use crate::structopt::StructOpt;



macro_rules! s_default_target_separator { () => { ";" } }



fn main() -> Result<(), i32> {
    let mut args = CLIArguments::from_args();
    let verbosity = args.verbose - args.quiet;

    let config = Config {
        min_size: args.min_size.map(|v| if v > 1 { v } else { 1 }).unwrap_or(1),
        no_brace_output: args.no_brace_output,
        dry_run: args.dry_run,
        verbosity
    };

    if let Some(arg_file) = args.argument_file {
        if !args.targets.is_empty() {
            eprintln!("No targets should be provided as cli arguments if arguments are being read from file");
            return Err(1);
        }
        let path = Path::new(&arg_file);
        if let Err(s) = read_file_lines(path, &mut args.targets) {
            eprintln!("Error reading argument file: {}", s);
            return Err(1);
        }
    }

    for target in &args.targets {
        if target.contains('\0') {
            eprintln!("Paths can never contain null byte: {}", target);
            return Err(1);
        }
    }
    let run_targets: Vec<Vec<&String>> = split_vec(&args.targets, &args.separator.unwrap_or(s_default_target_separator!().to_string()));

    if run_targets.is_empty() {
        if verbosity > 0 {
            println!("No targets provided");
        }
        return Ok(());
    }

    let mut bad = false;
    let run_paths: Vec<Vec<PathBuf>> = run_targets.iter().enumerate().map(
        |(_,spaths)| spaths.iter().map(
            |spath| Path::new(spath).canonicalize().unwrap_or_else(
                |_| {
                    eprintln!("Failed to retrieve absolute path for {}", shlex::try_quote(spath).unwrap());
                    bad = true;
                    Default::default()
                }
            )
        ).collect()
    ).collect();
    if bad {
        return Err(1);
    }

    if args.prompt {
        if !prompt_confirm(&run_targets) {
            return Ok(());
        }
    }


    for paths in &run_paths {
        if let Err(s) = check_all_same_device(paths) {
            eprintln!("{}", s);
            return Err(1);
        }
    }


    for paths in run_paths {
        run(paths, &config);
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
fn prompt_confirm<'a, T: Borrow<[Y]>, Y: AsRef<str>>(run_targets: &[T]) -> bool {
    println!("Are you sure you want to link all duplicates in each of these sets of targets?");
    for spaths in run_targets {
        println!("  {}", shlex::try_join(spaths.borrow().iter().map(|s| s.as_ref())).unwrap());
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
        return Err(format!("File does not exist or is not a normal file ({})", shlex::try_quote(&path.to_string_lossy()).unwrap()));
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
        Err(format!("Could not open {}", shlex::try_quote(&path.to_string_lossy()).unwrap()))
    }
}


/// exit on error
fn get_st_dev(file: &PathBuf) -> Result<u64, String> {
    if let Ok(metadata) = std::fs::metadata(file) {
        Ok(metadata.st_dev())
    } else {
        Err(format!("Failed to retrive device id for {}", shlex::try_quote(&file.to_string_lossy()).unwrap()))
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
            s.push_str(&shlex::try_quote(&path.to_string_lossy()).unwrap());
            s.push_str("\n");
        }
        s.pop(); // remove last newline
        Err(s)
    }
}


/// perform a full run
fn run(paths: Vec<PathBuf>, cfg: &Config) {
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
                if !are_hardlinked(f1, f2) && cmp(f1, f2).unwrap_or(false) {
                    if !cfg.dry_run {
                        if let Err(msg) = hardlink(f1, f2) {
                            eprintln!("{}: {}", msg, format_pair(&f1.to_string_lossy(), &f2.to_string_lossy(), cfg));
                            continue
                        }
                    }
                    if let Some(stdout_buffer) = &mut stdout_buffer {
                        if cfg.verbosity >= 0 {
                            writeln!(stdout_buffer, "hardlinked {}", format_pair(&f1.to_string_lossy(), &f2.to_string_lossy(), cfg)).unwrap();
                        }
                    }
                }
            }
        }
    }
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


fn format_pair(f1s: &str, f2s: &str, cfg: &Config) -> String {
    if cfg.no_brace_output {
        return format!(
            "{}  {}",
            shlex::try_quote(&f1s).unwrap(),
            shlex::try_quote(&f2s).unwrap()
        )
    }

    let prefix = common_prefix(&f1s, &f2s);
    let suffix = common_suffix(&f1s, &f2s);
    let prefixlong = prefix.len() > 2;
    let suffixlong = suffix.len() > 2;
    if prefixlong && suffixlong {
        format!(
            "{}{{{},{}}}{}",
            shlex::try_quote(prefix).unwrap(),
            shlex::try_quote(&f1s[ prefix.len()..std::cmp::max(prefix.len(), f1s.len()-suffix.len()) ]).unwrap(),
            shlex::try_quote(&f2s[ prefix.len()..std::cmp::max(prefix.len(), f2s.len()-suffix.len()) ]).unwrap(),
            shlex::try_quote(suffix).unwrap()
        )
    } else if prefixlong {
        format!(
            "{}{{{},{}}}",
            shlex::try_quote(prefix).unwrap(),
            shlex::try_quote(&f1s[prefix.len()..]).unwrap(),
            shlex::try_quote(&f2s[prefix.len()..]).unwrap()
        )
    } else if suffixlong {
        format!(
            "{{{},{}}}{}",
            shlex::try_quote(&f1s[..f1s.len()-suffix.len()]).unwrap(),
            shlex::try_quote(&f2s[..f2s.len()-suffix.len()]).unwrap(),
            shlex::try_quote(suffix).unwrap(),
        )
    } else {
        format!(
            "{} <-> {}",
            shlex::try_quote(&f1s).unwrap(),
            shlex::try_quote(&f2s).unwrap()
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
                registry.entry(size).or_insert_with(|| Vec::new()).push(path);
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
fn cmp(f1: &PathBuf, f2: &PathBuf) -> std::io::Result<bool> {
    if let (Ok(mut f1), Ok(mut f2)) = (std::fs::File::open(f1), std::fs::File::open(f2)) {
        cmp_files(&mut f1, &mut f2)
    } else { Ok(false) }
}

/// check equality of contents of two open files
fn cmp_files(f1: &mut std::fs::File, f2: &mut std::fs::File) -> std::io::Result<bool> {
    let buff1: &mut [u8] = &mut [0; 1024];
    let buff2: &mut [u8] = &mut [0; 1024];
    loop {
        let l1 = f1.read(buff1)?;
        let l2 = f2.read(buff2)?;
        if l1 != l2 { // different sizes
            return Ok(false);
        }
        if l1 == 0 { // end of both files
            return Ok(true);
        }
        if &buff1[0..l1] != &buff2[0..l2] { // compare data
            return Ok(false);
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


fn split_vec<'a, T: std::cmp::PartialEq>(input: &'a [T], delimiter: &T) -> Vec<Vec<&'a T>> {
    let mut result: Vec<Vec<&T>> = Vec::new();

    let mut chunk_start = 0;
    for (i,item) in input.iter().enumerate() {
        if item != delimiter {
            continue
        }
        if i == chunk_start { // zero size chunk
            continue
        }
        result.push(input[chunk_start..i].iter().collect::<Vec<&T>>());
        chunk_start = i+1; // next chunk starts on next index
    }
    if chunk_start < input.len() {
        result.push(input[chunk_start..].iter().collect::<Vec<&T>>());
    }
    result
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn _split_vec() {
        let v: Vec<_> = vec![";", "hi", "bye", ";", "1", ";", ";", "2", "2", ";"].into_iter().map(|s| s.to_string()).collect();
        let res = split_vec(&v[..], &";".to_string());
        println!("{:?}", v);
        println!("{:?}", res);
    }
}
