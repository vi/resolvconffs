use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use gumdrop::Options;
use nix::fcntl::OFlag;

/// Special FUSE filesystem that maps its sole file to other files based on network namespace of process that queries the file.
/// To be used for /etc/resolv.conf in setups where network namespaces are used without accompanying mount namespaces (without /etc/netns)
#[derive(Options)]
struct Opts {
    help: bool,

    /// Directory where to look for resolv.conf-like files for each netns.
    #[options(free, required)]
    backing_directory: PathBuf,

    /// Filename extension. resolvconffs maps its file to <backing_directory>/<netns_identifier><postfix>
    #[options(short = 'p', default = "conf")]
    extension: PathBuf,

    /// In case of target file does not exist, copy this file to target instead of returning ENOENT.
    #[options(short = 'd')]
    default_file: Option<PathBuf>,

    /// Directory where to look up network namespace IDs based on PIDs.
    #[options(short = 'P', default = "/proc")]
    procfs: PathBuf,

    #[options(free, required)]
    mountpoint_file: PathBuf,

    #[options(short = 'o', long = "fuse-opt")]
    other_fuse_opts: Vec<String>,
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Debug, Hash)]
pub struct UidGidPid {
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
}

trait_set::trait_set! {
    pub trait Mapper = FnMut(UidGidPid) -> Option<PathBuf>;
}

pub struct FileMapperFs<F: Mapper> {
    mapper: F,
}

impl<F: Mapper> FileMapperFs<F> {
    fn get_backing_file(&mut self, rq: &fuser::Request<'_>) -> nix::Result<PathBuf> {
        match (self.mapper)(UidGidPid {
            uid: rq.uid(),
            gid: rq.gid(),
            pid: rq.pid(),
        }) {
            Some(x) => Ok(x),
            None => Err(nix::errno::Errno::ENOENT),
        }
    }

    pub fn new(mapper: F) -> Self {
        Self { mapper }
    }
}

macro_rules! nftry {
    ($e:expr, $reply:ident) => {
        match $e {
            Ok(x) => x,
            Err(e) => return $reply.error(e as i32),
        }
    };
}

fn getattr_impl(f: impl AsRef<Path>, ino: u64, reply: fuser::ReplyAttr) {
    let st = nftry!(nix::sys::stat::stat(f.as_ref()), reply);

    reply.attr(
        &Duration::from_secs(3600),
        &fuser::FileAttr {
            ino,
            size: st.st_size as u64,
            blocks: st.st_blocks as u64,
            atime: SystemTime::UNIX_EPOCH
                + Duration::new(st.st_atime as u64, st.st_atime_nsec as u32),
            mtime: SystemTime::UNIX_EPOCH
                + Duration::new(st.st_mtime as u64, st.st_mtime_nsec as u32),
            ctime: SystemTime::UNIX_EPOCH
                + Duration::new(st.st_ctime as u64, st.st_ctime_nsec as u32),
            crtime: SystemTime::UNIX_EPOCH, // https://github.com/nix-rust/nix/issues/1649
            kind: fuser::FileType::RegularFile,
            perm: st.st_mode as u16,
            nlink: 1,
            uid: st.st_uid,
            gid: st.st_gid,
            rdev: 0,
            blksize: st.st_blksize as u32,
            flags: 0,
        },
    );
}

impl<F: Mapper> fuser::Filesystem for FileMapperFs<F> {
    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        if ino == 1 {
            let bf = nftry!(self.get_backing_file(_req), reply);
            getattr_impl(bf, ino, reply);
        } else {
            reply.error(libc::ENOENT)
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, flags: i32, reply: fuser::ReplyOpen) {
        if ino != 1 {
            return reply.error(libc::ENOENT);
        }
        let bf = nftry!(self.get_backing_file(_req), reply);

        match nix::fcntl::open(
            &bf,
            OFlag::from_bits_truncate(flags),
            nix::sys::stat::Mode::from_bits_truncate(0o666),
        ) {
            Ok(fh) => {
                return reply.opened(fh as u64, fuser::consts::FOPEN_DIRECT_IO);
            }
            Err(e) => {
                return reply.error(e as i32);
            }
        }
    }

    fn release(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        let fh = _fh as i32;
        match nix::unistd::close(fh) {
            Ok(()) => return reply.ok(),
            Err(e) => return reply.error(e as i32),
        }
    }

    fn fsync(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        _fh: u64,
        datasync: bool,
        reply: fuser::ReplyEmpty,
    ) {
        let fh = _fh as i32;
        if datasync {
            match nix::unistd::fdatasync(fh) {
                Ok(()) => return reply.ok(),
                Err(e) => return reply.error(e as i32),
            }
        } else {
            match nix::unistd::fsync(fh) {
                Ok(()) => return reply.ok(),
                Err(e) => return reply.error(e as i32),
            }
        }
    }

    fn read(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        _fh: u64,
        offset: i64,
        mut size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyData,
    ) {
        let fh = _fh as i32;
        size = size.min(4096 * 16);
        let mut buf = vec![0u8; size as usize];
        let ret = nftry!(nix::sys::uio::pread(fh, &mut buf[..], offset), reply);
        reply.data(&buf[0..ret])
    }

    fn write(
        &mut self,
        _req: &fuser::Request<'_>,
        _ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        let fh = _fh as i32;
        let ret = nftry!(nix::sys::uio::pwrite(fh, data, offset), reply);
        // FIXME: u32 overflow handling
        reply.written(ret as u32)
    }

    fn setattr(
        &mut self,
        _req: &fuser::Request<'_>,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        _atime: Option<fuser::TimeOrNow>,
        _mtime: Option<fuser::TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: fuser::ReplyAttr,
    ) {
        if ino != 1 {
            return reply.error(libc::ENOENT);
        }

        let bf = nftry!(self.get_backing_file(_req), reply);

        if let Some(size) = _size {
            if let Some(fh) = _fh {
                let fh = fh as i32;
                nftry!(nix::unistd::ftruncate(fh, size as i64), reply);
            } else {
                nftry!(nix::unistd::truncate(&bf, size as i64), reply);
            }
        }

        getattr_impl(bf, ino, reply);
    }
}

struct NetnsMapper {
    backing_directory: PathBuf,
    extension: PathBuf,
    default_file: Option<PathBuf>,
    procfs: PathBuf,
}

impl NetnsMapper {
    fn sanity_check(&self) {
        if std::fs::metadata(&self.backing_directory)
            .map(|x| x.is_dir())
            .ok()
            != Some(true)
        {
            eprintln!(
                "Backing directory {:?} may be not accessible",
                self.backing_directory
            );
        }

        if let Some(ref deffile) = self.default_file {
            if std::fs::File::open(deffile).is_err() {
                eprintln!("Default file {:?} may be unopeneable", deffile);
            }
        }

        let inits_netns = self.procfs.join("1/ns/net");
        if std::fs::read_link(&inits_netns).is_err() {
            eprintln!("Failed to resolve {:?}.\nYou may want to run resolvconffs as root if you want to serve multiple users.", inits_netns);
        }
    }

    fn map(&self, rq: UidGidPid) -> Option<PathBuf> {
        let mut netnslink = PathBuf::with_capacity(self.backing_directory.as_os_str().len() + 12);
        netnslink.push(&self.procfs);
        netnslink.push(format!("{}", rq.pid));
        netnslink.push("ns/net");
        let netns = if let Ok(netns) = std::fs::read_link(&netnslink) {
            netns
        } else {
            eprintln!("Failed to readlink {:?}", netnslink);
            return None;
        };

        let netns = if let Some(x) = netns.to_str() {
            x
        } else {
            eprintln!("Invalid netns symlink content in {:?}", netnslink);
            return None;
        };
        // net:[4026532413]

        let (net, ns) = if let Some(x) = netns.split_once(':') {
            x
        } else {
            eprintln!("netns symlink content has no `:` character in {:?}", netnslink);
            return None;
        };

        if net != "net" {
            eprintln!("netns symlink content does not start with 'net:' in {:?}", netnslink);
            return None;
        }

        let nsonly = ns.trim_end_matches(']').trim_start_matches('[');

        let mut targetfile = PathBuf::with_capacity(self.backing_directory.as_os_str().len() + 2 + nsonly.len() + self.extension.as_os_str().len());
        targetfile.push(&self.backing_directory);
        targetfile.push(nsonly);
        if self.extension.as_os_str().len() > 0 {
            targetfile.set_extension(self.extension.as_os_str());
        }

        if let Some(ref deffile) = self.default_file {
            if std::fs::metadata(&targetfile).is_err() {
                if std::fs::copy(deffile, &targetfile).is_err() {
                    eprintln!("Cannot copy from {:?} to {:?}", deffile, targetfile);
                }
            }
        } 

        Some(targetfile)
    }
}

fn main() -> std::io::Result<()> {
    use fuser::MountOption;

    env_logger::init();
    let opts: Opts = gumdrop::parse_args_or_exit(gumdrop::ParsingStyle::AllOptions);

    let mapper = NetnsMapper {
        backing_directory: opts.backing_directory,
        extension: opts.extension,
        default_file: opts.default_file,
        procfs: opts.procfs,
    };

    mapper.sanity_check();

    let mut fuse_opts = Vec::<MountOption>::with_capacity(3 + opts.other_fuse_opts.len());
    fuse_opts.push(MountOption::FSName("resolvconffs".to_owned()));
    fuse_opts.push(MountOption::DefaultPermissions);
    fuse_opts.push(MountOption::AllowOther);
    let fs = FileMapperFs::new(move |rq| mapper.map(rq));

    for x in opts.other_fuse_opts {
        fuse_opts.push(MountOption::CUSTOM(x));
    }

    if std::fs::symlink_metadata(&opts.mountpoint_file)
        .map(|x| x.is_file())
        .ok()
        != Some(true)
    {
        eprintln!("Use regular file as a mountpoint, not a directory.");
    }

    fuser::mount2(fs, opts.mountpoint_file, &fuse_opts)
}
