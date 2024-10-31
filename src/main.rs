extern crate shlex;
extern crate smallvec;
extern crate structopt;
use std::borrow::Borrow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{Read, Write, BufReader, BufRead};
use std::os::linux::fs::MetadataExt as MetadataExtLinux;
use std::path::{Path, PathBuf};
use crate::structopt::StructOpt;
use crate::smallvec::*;



macro_rules! s_arg_target_file_name { () => { "target-file" } }
macro_rules! s_default_target_separator { () => { ";" } }


#[derive(StructOpt)]
#[structopt(
    about="Hardlink duplicate files recursively\nSymlinks are treated as normal files",
    usage=concat!(env!("CARGO_PKG_NAME"), " [OPTION]... TARGET... ['", s_default_target_separator!(), "' TARGET...]")
)]
struct CLIArguments {
    #[structopt(short, long, parse(from_occurrences), help="Increase verbosity")]
    verbose: i8,

    #[structopt(short, long, parse(from_occurrences), help="Decrease verbosity")]
    quiet: i8,

    #[structopt(long, help=concat!(
        "Disable brace notation for output\n",
        "  Ex: /home/user/{dir,backup}/file",
    ))]
    no_brace_output: bool,

    #[structopt(long, help=concat!(
        "Perform no operations on the filesystem",
    ))]
    dry_run: bool,

    #[structopt(short="i", help=concat!(
        "Prompt once before operating\n",
        "Doesn't occurs if no targets are provided",
    ))]
    prompt: bool,

    #[structopt(short, long, value_name="VALUE", help=concat!(
        "Minimum file size to be considered for hardlinking\n",
        "Never goes below 1 (the default)",
    ))]
    min_size: Option<u64>,

    #[structopt(short, long, value_name="SEPARATOR", help=concat!(
        "Separator between sets of targets (default: ", s_default_target_separator!(), ")",
    ))]
    separator: Option<String>,

    #[structopt(long=s_arg_target_file_name!(), value_name="FILE", help=concat!(
        "File to source targets from (can be '-' for stdin)\n",
        "Same rules as CLI argument targets apply\n",
        "Mutually exclusive with CLI argument targets",
    ))]
    file_containing_targets: Option<String>,

    #[structopt(value_name="TARGET", help=concat!(
        "Target files and directories (recursive)\n",
        "Each SEPARATOR denotes a new set of targets\n",
        "  Each set of targets are separate from all other sets\n",
        "  All targets must be on the same device\n",
        "All symlinks are ignored\n",
        "'-' is not treated as special\n",
        "Mutually exclusive with --", s_arg_target_file_name!(),
    ))]
    targets: Vec<String>,
}



struct Config {
    dry_run: bool,
    min_size: u64,
    verbosity: i8,
    no_brace_output: bool
}



fn main() -> Result<(), i32> {
    let mut args = CLIArguments::from_args();
    let verbosity = args.verbose - args.quiet;

    let config = Config {
        min_size: args.min_size.map(|v| if v > 1 { v } else { 1 }).unwrap_or(1),
        no_brace_output: args.no_brace_output,
        dry_run: args.dry_run,
        verbosity
    };

    let run_targets: Vec<Vec<&String>> = obtain_run_targets(
        args.file_containing_targets.as_ref(),
        &mut args.targets,
        args.separator.as_ref().unwrap_or(&s_default_target_separator!().to_string()),
        verbosity,
    )?;
    if run_targets.is_empty() {
        if verbosity >= 1 {
            println!("No targets provided");
        }
        return Ok(());
    }

    let run_paths: Vec<Vec<PathWithMetadata>> = obtain_run_paths(
        run_targets.iter().map(|v| v.iter()),
        verbosity,
    )?;

    for paths in &run_paths {
        if let Err(s) = check_all_same_device(paths) {
            eprintln!("{}", s);
            return Err(1);
        }
    }

    if run_paths.len() == 0 {
        return Ok(());
    }

    if args.prompt {
        if !prompt_confirm(&run_targets) {
            return Ok(());
        }
    }

    for paths in run_paths {
        run(paths, &config);
    }

    Ok(())
}


/// result may be empty; contents each nonempty
fn obtain_run_targets<'a>(
    arg_file: Option<&String>,
    arg_targets: &'a mut Vec<String>,
    separator: &String, verbosity: i8
) -> Result<Vec<Vec<&'a String>>, i32> {
    if let Some(arg_file) = &arg_file {
        if !arg_targets.is_empty() {
            if verbosity >= 0 {
                eprintln!("No targets should be provided as cli arguments if arguments are being read from file");
            }
            return Err(1);
        }
        if let Err(s) = {
            if *arg_file == "-" {
                read_lines(std::io::stdin().lock(), arg_targets)
            } else {
                read_file_lines(Path::new(&arg_file), arg_targets)
            }
        } {
            if verbosity >= 0 {
                eprintln!("Error reading file containing targets: {}", s);
            }
            return Err(1);
        }
    } else {
        for target in arg_targets.iter() {
            if target.contains('\0') {
                if verbosity >= 0 {
                    eprintln!("Paths can never contain null byte: {}", target);
                }
                return Err(1);
            }
        }
    }

    let mut run_targets = split_vec(arg_targets, &separator);
    for i in (0..run_targets.len()).rev() {
        if run_targets[i].len() == 0 {
            run_targets.swap_remove(i);
        }
    }
    Ok(run_targets)
}


/// result has no symlinks; may be empty; contents each nonempty
fn obtain_run_paths<T, Y, U>(run_targets: T, verbosity: i8) -> Result<Vec<Vec<PathWithMetadata>>, i32>
where
    T: Iterator<Item=Y> + ExactSizeIterator,
    Y: Iterator<Item=U> + ExactSizeIterator,
    U: AsRef<str>,
{
    let mut run_paths: Vec<Vec<PathWithMetadata>> = Vec::with_capacity(run_targets.len());
    for spaths in run_targets {
        let mut paths = Vec::with_capacity(spaths.len());
        for spath in spaths {
            let path = Path::new(spath.as_ref()).canonicalize().map_err(|_| {
                if verbosity >= 1 {
                    eprintln!("Failed to retrieve absolute path for {}", shlex::try_quote(spath.as_ref()).unwrap());
                }
                1
            })?;
            let pwmd = PathWithMetadata::new(path).map_err(|s| {
                if verbosity >= 1 {
                    eprintln!("{}", s);
                }
                1
            })?;
            if !pwmd.md().file_type().is_symlink() {
                paths.push(pwmd);
            }
        }
        if paths.len() > 0 {
            run_paths.push(paths);
        }
    }
    Ok(run_paths)
}


/// perform a full run
fn run(pwmds: Vec<PathWithMetadata>, cfg: &Config) {
    let mut registry: HashMap<u64, Vec<PathWithMetadata>> = HashMap::new();
    for pwmd in pwmds {
        register(pwmd, &mut registry, cfg);
    }
    registry.retain(|_,files| files.len() >= 2);

    let mut stdout_buffer = (cfg.verbosity >= 0).then(|| std::io::BufWriter::new(std::io::stdout().lock()));

    if let Some(stdout_buffer) = &mut stdout_buffer {
        if cfg.verbosity >= 0 {
            writeln!(stdout_buffer, "Considering {} total files for duplicates", registry.iter().map(|(_,files)| files.len()).sum::<usize>()).unwrap();
        }
    }

    for (fsize, pwmds) in registry {
        run_one_size(fsize, &pwmds, cfg, stdout_buffer.as_mut());
    }
}

fn run_one_size<W: Write>(fsize: u64, pwmds: &[PathWithMetadata], cfg: &Config, mut stdout_buffer: Option<&mut W>) {
    if let Some(stdout_buffer) = stdout_buffer.as_mut() {
        if cfg.verbosity >= 1 {
            writeln!(stdout_buffer, "Considering {} files of size {} for duplicates", pwmds.len(), fsize).unwrap();
        }
    }
    // if cfg.verbosity >= 0 {
    //     pwmds.sort_by_key(|pwmd| pwmd.path.file_name().unwrap_or_default().to_string_lossy().to_string());
    // }
    let mut by_inode: Vec<SmallVec<[&PathWithMetadata; 1]>> = Vec::with_capacity((pwmds.len() as f64 * 0.8) as usize); // each nonempty
    let mut inodes: Vec<u64> = Vec::with_capacity(by_inode.len());
    for pwmd in pwmds {
        let inode: u64 = pwmd.md().st_ino();
        match inodes.binary_search(&inode) {
            Ok(i) => {
                by_inode[i].push(pwmd);
            },
            Err(i) => {
                inodes.insert(i, inode);
                by_inode.insert(i, smallvec![pwmd]);
            }
        }
    }
    drop(inodes);
    by_inode.sort_by(|a,b| b.len().cmp(&a.len())); // descending size order

    // compare each with eachother
    let mut i = 0;
    while i < by_inode.len() {
        let mut j = i+1;
        while j < by_inode.len() {
            let (keeps, replaces) = get2mut(&mut by_inode, i, j);
            if hardlink_all(keeps, replaces, cfg, stdout_buffer.as_mut()) {
                by_inode.swap_remove(j);
            } else {
                j += 1;
            }
        }
        i += 1;
    }
}


/// recursively register path or its contents if directory into registry
/// eprints errors
fn register(
    pwmd: PathWithMetadata,
    registry: &mut HashMap<u64, Vec<PathWithMetadata>>,
    cfg: &Config,
) {
    if pwmd.md().file_type().is_symlink() {
        return;
    }

    if pwmd.path.is_file() {
        let size = pwmd.md().st_size();
        if size >= cfg.min_size {
            registry.entry(size).or_default().push(pwmd);
        }
        return;
    }

    if pwmd.path.is_dir() { match std::fs::read_dir(&pwmd.path) {
        Ok(entries) => for entry in entries { match entry {
            Ok(entry) => match PathWithMetadata::new(entry.path()) {
                Ok(child_pwmd) => register(child_pwmd, registry, cfg),
                Err(s) => if cfg.verbosity >= 1 {
                    eprintln!("{}", s);
                },
            },
            Err(error) => if cfg.verbosity >= 1 {
                eprintln!("Failed to inspect {}: {}", shlex::try_quote(&pwmd.path.to_string_lossy()).unwrap(), error);
            },
        } },
        Err(error) => if cfg.verbosity >= 1 {
            eprintln!("Failed to read dir {}: {}", shlex::try_quote(&pwmd.path.to_string_lossy()).unwrap(), error);
        },
    } }
}



struct PathWithMetadata {
    pub path: PathBuf,
    md: RefCell<std::fs::Metadata>,
}
impl PathWithMetadata {
    pub fn new(path: PathBuf) -> Result<Self, String>{
        let md = RefCell::new(Self::get_md(&path)?);
        Ok(PathWithMetadata{ path, md })
    }
    #[inline(always)]
    pub fn md(&self) -> std::cell::Ref<std::fs::Metadata> {
        self.md.borrow()
    }
    pub fn reset_md(&self) -> Result<(), String> {
        *self.md.borrow_mut() = Self::get_md(&self.path)?;
        Ok(())
    }
    fn get_md(path: &PathBuf) -> Result<std::fs::Metadata, String> {
        std::fs::symlink_metadata(path).map_err(|_| format!("Failed to retrive metadata for {}", shlex::try_quote(&path.to_string_lossy()).unwrap()))
    }

}
impl AsRef<PathBuf> for PathWithMetadata {
    fn as_ref(&self) -> &PathBuf {
        return &self.path;
    }
}
impl AsRef<Path> for PathWithMetadata {
    fn as_ref(&self) -> &Path {
        return &self.path.as_ref();
    }
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

fn read_lines(reader: impl BufRead, dest: &mut Vec<String>) -> Result<(), String> {
    for line in reader.lines() {
        dest.push(line.map_err(|e| format!("Error reading line: {}", e))?);
    }
    Ok(())
}

fn read_file_lines(path: &Path, dest: &mut Vec<String>) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("File does not exist or is not a normal file ({})", shlex::try_quote(&path.to_string_lossy()).unwrap()));
    }
    let reader = BufReader::new(std::fs::File::open(path).map_err(
        |e| format!("Could not open {}: {}", shlex::try_quote(&path.to_string_lossy()).unwrap(), e)
    )?);
    read_lines(reader, dest)
}


fn check_all_same_device(pwmds: &[PathWithMetadata]) -> Result<(), String> {
    if pwmds.len() <= 1 {
        return Ok(())
    }
    let mut by_dev: HashMap<u64, Vec<&PathWithMetadata>> = Default::default();
    for pwmd in pwmds.iter() {
        by_dev.entry(pwmd.md().st_dev()).or_default().push(pwmd);
    }
    if by_dev.len() <= 1 {
        return Ok(());
    }
    let mut lines = Vec::with_capacity(1+by_dev.len());
    lines.push(String::from("Device ids must all be the same; got paths on different devices:"));
    lines.extend(by_dev.into_iter().map(|(dev,pwmds)| {
        if pwmds.len() == 1 {
            format!("  Device {}: {} path: {}", dev, pwmds.len(), &shlex::try_quote(&pwmds[0].path.to_string_lossy()).unwrap())
        } else {
            format!("  Device {}: {} paths", dev, pwmds.len())
        }
    }));
    Err(lines.join("\n"))
}


/// get two mutable references in an array
/// expects correct inputs
fn get2mut<'a, T>(v: &'a mut [T], i: usize, j: usize) -> (&'a mut T, &'a mut T) {
    let (left, right) = v.split_at_mut(j);
    (&mut left[i], &mut right[0])
}


fn hardlink(keep: &PathWithMetadata, replace: &PathWithMetadata) -> Result<(), String> {
    std::fs::remove_file(&replace.path).map_err(|_| "Failed to remove for hardlinking")?;
    std::fs::hard_link(&keep.path, &replace.path).map_err(|_| {
        match std::fs::copy(&keep.path, &replace.path) {
            Ok(_) => "Failed to hardlink (copied instead)",
            Err(_) => "Failed to hardlink or copy" // awful scenario but i believe it is impossible since i don't see how you could remove a file yet not create one in its place
        }
    })?;
    replace.reset_md()?;
    Ok(())
}

/// returns whether linking was done
/// eprints errors
fn hardlink_all<'a, 'b, T, W: Write>(keeps: &'a mut SmallVec<T>, replaces: &'a mut SmallVec<T>, cfg: &Config, mut stdout_buffer: Option<&mut W>) -> bool
where T: smallvec::Array<Item=&'b PathWithMetadata>,
{
    if !cmp(&replaces.first().unwrap().path, &keeps.first().unwrap().path).unwrap_or(false) {
        return false;
    }
    for replace in replaces.into_iter() {
        let keep = keeps.first().unwrap();
        if !cfg.dry_run {
            if let Err(msg) = hardlink(keep, replace) {
                if cfg.verbosity >= 0 {
                    eprintln!("{}: {}", msg, format_pair(&keep.path.to_string_lossy(), &replace.path.to_string_lossy(), cfg));
                }
                continue // path no longer valid
            }
        }
        if let Some(stdout_buffer) = stdout_buffer.as_mut() {
            if cfg.verbosity >= 0 {
                writeln!(stdout_buffer, "hardlinked {}", format_pair(&keep.path.to_string_lossy(), &replace.path.to_string_lossy(), cfg)).unwrap();
            }
        }
        drop(keep);
        keeps.push(replace);
    }
    true
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


/// check equality of contents of two paths to files
/// does not check sizes
fn cmp(f1: impl AsRef<Path>, f2: impl AsRef<Path>) -> std::io::Result<bool> {
    cmp_read(std::fs::File::open(f1)?, std::fs::File::open(f2)?)
}

/// check equality of contents of two open files
fn cmp_read(mut f1: impl Read, mut f2: impl Read) -> std::io::Result<bool> {
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


/// double delimiters will result in empty vecs
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
        let v: Vec<_> = vec![";", ";", ";"].into_iter().map(|s| s.to_string()).collect();
        let res = split_vec(&v[..], &";".to_string());
        assert_eq!(res.len(), 2)
    }
}
