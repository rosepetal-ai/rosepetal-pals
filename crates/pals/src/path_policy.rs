//! Which files of this machine the agent reads for a peer, and where it
//! writes what a peer sends: both rules in one place.
//!
//! **Reading** ([`open_attachment`]). A file the outbox sends with a
//! question is read only when, resolved first (symbolic links and `..`
//! included), it is:
//!
//! - outside the agent's data directory, which holds its key, its store
//!   with the grants it holds, its blobs and its tasks' files;
//! - outside `/proc`, `/sys` and `/dev`, and, on Linux, on neither a procfs
//!   nor a sysfs filesystem, wherever that is mounted (`/host/proc`): the
//!   kernel calls files such as `/proc/self/environ`, the daemon's
//!   environment and its secrets, regular ones. devtmpfs has no filesystem
//!   type of its own (it reports tmpfs, as `/tmp` may): its device files
//!   are not regular files, which the next rule refuses wherever they are;
//! - a regular file, checked before it is opened, so that a FIFO cannot
//!   block the call, and again on the file opened, which must be the one
//!   checked. Anything else readable is sent.
//!
//! **Writing** ([`download_path`]). `pals_fetch` writes a peer's bytes only
//! inside the download directory (`[mcp] download_dir`, `~/Downloads/pals`
//! by default): an allowlist, since a new file almost anywhere else can be
//! code that runs (`~/.bash_aliases`, `~/.claude/CLAUDE.md`,
//! `~/.ssh/authorized_keys`, a project's `conftest.py`, `/etc/cron.d/*`).
//!
//! - The download directory is created on first use, with mode `0700`
//!   (the directories above it with the default mode), and is never inside
//!   the data directory: the configuration refuses one that is, and the
//!   check is repeated with both resolved.
//! - `dest` is a file name or a relative path, resolved inside the download
//!   directory, whose missing directories are created with mode `0700`; or
//!   an absolute path whose directory exists and, resolved, is inside it.
//!   Either way the directory reached, resolved, must be inside it.
//! - No component of the path below the download directory starts with
//!   `.`: no `..`, and no hidden file or directory such as `.bashrc` or
//!   `.git/hooks/…`.
//! - The file must be new: the caller creates it with `create_new`, so
//!   nothing is ever written over, nor through a symbolic link.
//! - Nothing is created for a call that is refused: [`check_download`]
//!   makes every check that does not need the file without creating
//!   anything (paths resolved as far as they exist, the rest taken by
//!   name), the caller makes it before it even gets the blob to write, and
//!   [`download_path`] makes it again before it creates the download
//!   directory and the missing parts of `dest`. An absolute `dest` must be
//!   in a directory that exists, or in the download directory itself.

use std::fs::{File, Metadata};
use std::path::{Component, Path, PathBuf};

/// The directories whose files are never sent, whatever they say they are.
const SYSTEM_DIRS: [&str; 3] = ["/proc", "/sys", "/dev"];

/// Why a path under [`SYSTEM_DIRS`], or on procfs or sysfs, is refused.
const SYSTEM_FILE: &str = "it is a system file (under /proc, /sys or /dev, or on procfs or sysfs), \
                           which is never sent";

/// Opens the attachment at `path` to read it, as the reading rule says:
/// outside `data_dir`, outside the system's pseudo-filesystems, a regular
/// file, and the file opened the one checked.
pub(crate) fn open_attachment(path: &Path, data_dir: Option<&Path>) -> Result<File, String> {
    let resolved = std::fs::canonicalize(path).map_err(|err| err.to_string())?;
    if let Some(data_dir) = data_dir
        && resolved.starts_with(data_dir)
    {
        return Err(format!(
            "it is inside this agent's data directory {}, which holds its key and its grants: \
             nothing in it is ever sent",
            data_dir.display()
        ));
    }
    if SYSTEM_DIRS.iter().any(|dir| resolved.starts_with(dir)) {
        return Err(SYSTEM_FILE.to_owned());
    }
    let checked = std::fs::metadata(&resolved).map_err(|err| err.to_string())?;
    if !checked.is_file() {
        return Err("not a regular file".to_owned());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // Should the path have become a FIFO or a link since, opening it does
    // not wait for a writer nor follow the link.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(
        &mut options,
        libc::O_NONBLOCK | libc::O_NOFOLLOW,
    );
    let file = options.open(&resolved).map_err(|err| err.to_string())?;
    let opened = file.metadata().map_err(|err| err.to_string())?;
    if !opened.is_file() || !same_file(&checked, &opened) {
        return Err("it changed while it was being opened".to_owned());
    }
    if on_kernel_filesystem(&file).map_err(|err| err.to_string())? {
        return Err(SYSTEM_FILE.to_owned());
    }
    Ok(file)
}

/// Whether `file` is on procfs or sysfs, wherever that is mounted.
#[cfg(target_os = "linux")]
fn on_kernel_filesystem(file: &File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: an all-zero `statfs` is a valid value of this plain C struct,
    // which `fstatfs` only writes to.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is open for as long as `file` lives, and
    // `stats` is valid for writes for the whole call.
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stats) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // The field's and the constants' integer types vary between targets.
    #[allow(clippy::unnecessary_cast)]
    let kind = stats.f_type as i64;
    #[allow(clippy::unnecessary_cast)]
    let kernel = [libc::PROC_SUPER_MAGIC as i64, libc::SYSFS_MAGIC as i64];
    Ok(kernel.contains(&kind))
}

/// Whether `file` is on procfs or sysfs: elsewhere than Linux, the paths
/// say it.
#[cfg(not(target_os = "linux"))]
fn on_kernel_filesystem(_file: &File) -> std::io::Result<bool> {
    Ok(false)
}

/// Whether `a` and `b` describe the same file.
#[cfg(unix)]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Whether `a` and `b` describe the same file: as far as this platform
/// tells, files of the same size.
#[cfg(not(unix))]
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.len() == b.len()
}

/// Refuses, without touching the disk, a `dest` the writing rule refuses
/// whatever is on the disk: a relative one with a part that is not a plain
/// name or starts with a dot, or a path that names no file.
pub(crate) fn check_dest(dest: &str) -> Result<(), String> {
    let path = Path::new(dest);
    if path.is_absolute() {
        return match path.file_name() {
            Some(_) => Ok(()),
            None => Err(format!("dest {dest} names no file")),
        };
    }
    check_components(dest, path)
}

/// Refuses, without creating anything, a `dest` that [`download_path`]
/// would refuse: its form ([`check_dest`]); a download directory that is
/// not a directory or is inside `data_dir`; an absolute `dest` whose
/// directory is outside the download directory, holds a part that starts
/// with a dot below it, or does not exist (unless it is the download
/// directory itself); a relative one with an existing part that leads out
/// of the download directory or is not a directory; and a file inside
/// `data_dir`.
pub(crate) fn check_download(
    download_dir: &Path,
    data_dir: Option<&Path>,
    dest: &str,
) -> Result<(), String> {
    check_dest(dest)?;
    let root = resolve_existing(download_dir)?;
    if root.exists() && !root.is_dir() {
        return Err(format!(
            "the download directory {} is not a directory",
            download_dir.display()
        ));
    }
    let data_dir = data_dir.and_then(|dir| std::fs::canonicalize(dir).ok());
    if let Some(data_dir) = &data_dir
        && root.starts_with(data_dir)
    {
        return Err(format!(
            "the download directory {} is inside this agent's data directory {}: pals_fetch \
             never writes there",
            download_dir.display(),
            data_dir.display()
        ));
    }
    let outside = || {
        format!(
            "dest {dest} is outside this agent's download directory {}: pals_fetch writes only \
             there; give a file name or a path inside it",
            download_dir.display()
        )
    };
    let dest_path = Path::new(dest);
    let file = if dest_path.is_absolute() {
        let (Some(parent), Some(name)) = (dest_path.parent(), dest_path.file_name()) else {
            return Err(format!("dest {dest} names no file"));
        };
        let dir = resolve_existing(parent).map_err(|_| outside())?;
        let Ok(inside) = dir.strip_prefix(&root) else {
            return Err(outside());
        };
        check_components(dest, &inside.join(name))?;
        if !parent.exists() && dir != root {
            return Err(format!(
                "the directory of dest {dest} does not exist: give a path inside the download \
                 directory instead, whose missing directories are created"
            ));
        }
        dir.join(name)
    } else {
        let mut current = root.clone();
        let mut names: Vec<_> = dest_path.components().collect();
        let file_name = names.pop();
        for name in names {
            let next = current.join(name);
            if std::fs::symlink_metadata(&next).is_err() {
                // From here on nothing exists: it will be created.
                current = next;
                continue;
            }
            current = std::fs::canonicalize(&next)
                .map_err(|err| format!("cannot use {}: {err}", next.display()))?;
            if !current.starts_with(&root) {
                return Err(outside());
            }
            if !current.is_dir() {
                return Err(format!("{} is not a directory", next.display()));
            }
        }
        match file_name {
            Some(name) => current.join(name),
            None => return Err(format!("dest {dest} names no file")),
        }
    };
    if let Some(data_dir) = &data_dir
        && file.starts_with(data_dir)
    {
        return Err(outside());
    }
    Ok(())
}

/// `path` resolved as far as it exists (symbolic links and `..` included),
/// and the rest of it taken by name; refused when a part that does not
/// exist is not a plain name.
fn resolve_existing(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(&existing) {
            resolved.extend(missing.iter().rev());
            return Ok(resolved);
        }
        match existing.components().next_back() {
            Some(Component::Normal(name)) => {
                missing.push(name.to_owned());
                existing.pop();
            }
            _ => {
                return Err(format!(
                    "{} climbs out of the directories that exist",
                    path.display()
                ));
            }
        }
    }
}

/// Where `pals_fetch` writes the file `dest` names, as the writing rule
/// says: inside `download_dir`, which must not be inside `data_dir`
/// (resolved), after [`check_download`]; creates the download directory if
/// missing, and the missing directories of a relative `dest`. The file
/// itself is the caller's to create, new.
pub(crate) fn download_path(
    download_dir: &Path,
    data_dir: Option<&Path>,
    dest: &str,
) -> Result<PathBuf, String> {
    check_download(download_dir, data_dir, dest)?;
    let root = open_download_dir(download_dir, data_dir)?;
    let outside = || {
        format!(
            "dest {dest} is outside this agent's download directory {}: pals_fetch writes only \
             there; give a file name or a path inside it",
            root.display()
        )
    };
    let dest_path = Path::new(dest);
    // The directory to write in, resolved, and what is left below it.
    let (dir, below) = if dest_path.is_absolute() {
        let (Some(parent), Some(name)) = (dest_path.parent(), dest_path.file_name()) else {
            return Err(format!("dest {dest} names no file"));
        };
        let parent = std::fs::canonicalize(parent)
            .map_err(|err| format!("cannot use the directory of {dest}: {err}"))?;
        let Ok(inside) = parent.strip_prefix(&root) else {
            return Err(outside());
        };
        check_components(dest, &inside.join(name))?;
        (parent, PathBuf::from(name))
    } else {
        check_components(dest, dest_path)?;
        (root.clone(), dest_path.to_owned())
    };
    let mut names: Vec<_> = below.components().collect();
    let Some(Component::Normal(file_name)) = names.pop() else {
        return Err(format!("dest {dest} names no file"));
    };
    let mut current = dir;
    for name in names {
        let next = current.join(name);
        if !next.exists() {
            create_private_dir(&next)
                .map_err(|err| format!("cannot create {}: {err}", next.display()))?;
        }
        current = std::fs::canonicalize(&next)
            .map_err(|err| format!("cannot use {}: {err}", next.display()))?;
        if !current.starts_with(&root) {
            return Err(outside());
        }
        if !current.is_dir() {
            return Err(format!("{} is not a directory", next.display()));
        }
    }
    let path = current.join(file_name);
    if let Some(data_dir) = data_dir.and_then(|dir| std::fs::canonicalize(dir).ok())
        && path.starts_with(&data_dir)
    {
        return Err(outside());
    }
    Ok(path)
}

/// `download_dir`, created if missing, resolved: refused inside `data_dir`.
fn open_download_dir(download_dir: &Path, data_dir: Option<&Path>) -> Result<PathBuf, String> {
    if !download_dir.exists() {
        if let Some(parent) = download_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "cannot create the download directory {}: {err}",
                    download_dir.display()
                )
            })?;
        }
        if let Err(err) = create_private_dir(download_dir)
            && err.kind() != std::io::ErrorKind::AlreadyExists
        {
            return Err(format!(
                "cannot create the download directory {}: {err}",
                download_dir.display()
            ));
        }
    }
    let root = std::fs::canonicalize(download_dir).map_err(|err| {
        format!(
            "cannot use the download directory {}: {err}",
            download_dir.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!(
            "the download directory {} is not a directory",
            root.display()
        ));
    }
    if let Some(data_dir) = data_dir.and_then(|dir| std::fs::canonicalize(dir).ok())
        && root.starts_with(&data_dir)
    {
        return Err(format!(
            "the download directory {} is inside this agent's data directory {}: pals_fetch \
             never writes there",
            root.display(),
            data_dir.display()
        ));
    }
    Ok(root)
}

/// Refuses a path below the download directory with a component that is
/// not a plain name, or that starts with `.`.
fn check_components(dest: &str, below: &Path) -> Result<(), String> {
    let mut any = false;
    for component in below.components() {
        match component {
            Component::Normal(name) if !name.to_string_lossy().starts_with('.') => any = true,
            _ => {
                return Err(format!(
                    "dest {dest} has a part that starts with a dot or climbs out ({}): \
                     pals_fetch writes no hidden file and never leaves its download directory",
                    component.as_os_str().to_string_lossy()
                ));
            }
        }
    }
    if any {
        Ok(())
    } else {
        Err(format!("dest {dest} names no file"))
    }
}

/// Creates the directory `dir`, readable by its owner only.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// `path` with its `.` and `..` components resolved by name only, without
/// reading the filesystem: for comparing configured paths that may not
/// exist yet.
pub(crate) fn lexically_normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn procfs_and_sysfs_are_kernel_filesystems_wherever_they_are() {
        for kernel in ["/proc/self/status", "/sys/kernel/uevent_seqnum"] {
            let file = File::open(kernel).unwrap();
            assert!(on_kernel_filesystem(&file).unwrap(), "{kernel}");
        }
        let ordinary = tempfile::NamedTempFile::new().unwrap();
        assert!(!on_kernel_filesystem(ordinary.as_file()).unwrap());
    }

    #[test]
    fn system_dirs_are_refused_by_path() {
        for path in ["/proc/self/environ", "/proc/self/status"] {
            let refused = open_attachment(Path::new(path), None).unwrap_err();
            assert!(refused.contains("system file"), "{path}: {refused}");
        }
    }

    #[test]
    fn components_below_the_download_dir_are_plain_names() {
        for good in ["a.txt", "sub/a.txt", "a/b/c.tar.gz", "a..b"] {
            check_components(good, Path::new(good)).unwrap();
        }
        for bad in [
            ".bashrc",
            "../a.txt",
            "sub/../../a.txt",
            "./a.txt",
            "sub/.git/hooks/post-checkout",
            "",
        ] {
            assert!(check_components(bad, Path::new(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn lexical_normal_form_resolves_dots_by_name() {
        assert_eq!(
            lexically_normal(Path::new("/a/./b/../c/")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            lexically_normal(Path::new("/a/.pals/../.pals/dl")),
            PathBuf::from("/a/.pals/dl")
        );
    }
}
