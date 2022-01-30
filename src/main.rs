use std::path::Path;
use std::time::Duration;
use std::{os::unix::prelude::OpenOptionsExt, path::PathBuf};

use gumdrop::Options;
use nix::fcntl::OFlag;

#[derive(Options)]
struct Opts {
    help: bool,

    #[options(free, required)]
    backing_file: PathBuf,

    #[options(free, required)]
    mountpoint_file: PathBuf,

    #[options(short = 'o', long = "fuse-opt")]
    other_fuse_opts: Vec<String>,
}

struct ReCoFs {
    backing_file: PathBuf,
}

impl ReCoFs {
    fn get_backing_file(&self) -> nix::Result<PathBuf> {
        Ok(self.backing_file.clone())
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
            atime: std::time::SystemTime::UNIX_EPOCH
                + Duration::new(st.st_atime as u64, st.st_atime_nsec as u32),
            mtime: std::time::SystemTime::UNIX_EPOCH
                + Duration::new(st.st_mtime as u64, st.st_mtime_nsec as u32),
            ctime: std::time::SystemTime::UNIX_EPOCH
                + Duration::new(st.st_ctime as u64, st.st_ctime_nsec as u32),
            crtime: std::time::SystemTime::UNIX_EPOCH, // https://github.com/nix-rust/nix/issues/1649
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

impl fuser::Filesystem for ReCoFs {
    fn getattr(&mut self, _req: &fuser::Request<'_>, ino: u64, reply: fuser::ReplyAttr) {
        if ino == 1 {
            let bf = nftry!(self.get_backing_file(), reply);
            getattr_impl(bf, ino, reply);
        } else {
            reply.error(libc::ENOENT)
        }
    }

    fn open(&mut self, _req: &fuser::Request<'_>, ino: u64, flags: i32, reply: fuser::ReplyOpen) {
        if ino != 1 {
            return reply.error(libc::ENOENT);
        }
        let bf = nftry!(self.get_backing_file(), reply);

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
        _ctime: Option<std::time::SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<std::time::SystemTime>,
        _chgtime: Option<std::time::SystemTime>,
        _bkuptime: Option<std::time::SystemTime>,
        _flags: Option<u32>,
        reply: fuser::ReplyAttr,
    ) {
        if ino != 1 {
            return reply.error(libc::ENOENT);
        }

        let bf = nftry!(self.get_backing_file(), reply);

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

fn main() -> std::io::Result<()> {
    env_logger::init();
    let opts: Opts = gumdrop::parse_args_or_exit(gumdrop::ParsingStyle::AllOptions);
    let mut fuse_opts = Vec::<fuser::MountOption>::with_capacity(2 + opts.other_fuse_opts.len());
    fuse_opts.push(fuser::MountOption::FSName("resolvconffs".to_owned()));
    fuse_opts.push(fuser::MountOption::DefaultPermissions);
    let fs = ReCoFs {
        backing_file: opts.backing_file,
    };

    for x in opts.other_fuse_opts {
        fuse_opts.push(fuser::MountOption::CUSTOM(x));
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
