use std::collections::HashMap;
use std::ffi::CString;
use std::ffi::OsStr;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use fuser::Errno;
use fuser::FileAttr;
use fuser::FileHandle;
use fuser::FileType;
use fuser::Filesystem;
use fuser::FopenFlags;
use fuser::Generation;
use fuser::INodeNo;
use fuser::LockOwner;
use fuser::ReplyAttr;
use fuser::ReplyCreate;
use fuser::ReplyData;
use fuser::ReplyDirectory;
use fuser::ReplyEmpty;
use fuser::ReplyEntry;
use fuser::ReplyOpen;
use fuser::ReplyStatfs;
use fuser::ReplyWrite;
use fuser::Request;
use fuser::TimeOrNow;
use fuser::WriteFlags;

use crate::inode_map::InodeMap;
use crate::intercept::InterceptMatcher;
use crate::provider::ContentProvider;

const TTL_NORMAL: Duration = Duration::from_secs(1);
const TTL_INTERCEPTED: Duration = Duration::from_secs(0);

pub(crate) struct ProxyFs {
    root_fd: OwnedFd,
    inodes: Mutex<InodeMap>,
    matcher: InterceptMatcher,
    provider: Box<dyn ContentProvider>,
    handles: Mutex<HashMap<u64, OwnedFd>>,
    next_fh: AtomicU64,
    dir_handles: Mutex<HashMap<u64, OwnedFd>>,
}

impl ProxyFs {
    pub fn new(
        root_fd: OwnedFd,
        matcher: InterceptMatcher,
        provider: Box<dyn ContentProvider>,
    ) -> Self {
        Self {
            root_fd,
            inodes: Mutex::new(InodeMap::new()),
            matcher,
            provider,
            handles: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
            dir_handles: Mutex::new(HashMap::new()),
        }
    }

    fn alloc_fh(&self) -> FileHandle {
        FileHandle(self.next_fh.fetch_add(1, Ordering::Relaxed))
    }

    fn root_raw(&self) -> RawFd {
        self.root_fd.as_raw_fd()
    }

    fn inode_path(&self, ino: INodeNo) -> Option<PathBuf> {
        self.inodes
            .lock()
            .unwrap()
            .get_path(ino.0)
            .map(|p| p.to_path_buf())
    }

    fn stat_path(&self, rel: &Path) -> Result<libc::stat, Errno> {
        let c_path = path_to_cstring(rel);
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::fstatat(
                self.root_raw(),
                c_path.as_ptr(),
                &mut st,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == -1 { Err(last_errno()) } else { Ok(st) }
    }

    fn stat_to_attr(&self, st: &libc::stat, rel_path: &Path) -> FileAttr {
        let file_type = mode_to_filetype(st.st_mode);
        let is_file = file_type == FileType::RegularFile;
        let filename = rel_path.file_name().unwrap_or_default();
        let intercepted = is_file && self.matcher.is_intercepted(filename);

        let size = if intercepted {
            InterceptMatcher::inflated_size(st.st_size as u64, rel_path, self.provider.as_ref())
                .unwrap_or(st.st_size as u64)
        } else {
            st.st_size as u64
        };

        FileAttr {
            ino: INodeNo(st.st_ino),
            size,
            blocks: st.st_blocks as u64,
            atime: timespec_to_systime(st.st_atime, st.st_atime_nsec),
            mtime: timespec_to_systime(st.st_mtime, st.st_mtime_nsec),
            ctime: timespec_to_systime(st.st_ctime, st.st_ctime_nsec),
            crtime: UNIX_EPOCH,
            kind: file_type,
            perm: (st.st_mode & 0o7777) as u16,
            nlink: st.st_nlink as u32,
            uid: st.st_uid,
            gid: st.st_gid,
            rdev: st.st_rdev as u32,
            blksize: st.st_blksize as u32,
            flags: 0,
        }
    }

    fn ttl_for(&self, rel_path: &Path) -> Duration {
        let filename = rel_path.file_name().unwrap_or_default();
        if self.matcher.is_intercepted(filename) {
            TTL_INTERCEPTED
        } else {
            TTL_NORMAL
        }
    }
}

impl Filesystem for ProxyFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let parent_path = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let child_path = parent_path.join(name);
        let st = match self.stat_path(&child_path) {
            Ok(st) => st,
            Err(e) => return reply.error(e),
        };
        self.inodes
            .lock()
            .unwrap()
            .insert(child_path.clone(), st.st_ino);
        let attr = self.stat_to_attr(&st, &child_path);
        let ttl = self.ttl_for(&child_path);
        reply.entry(&ttl, &attr, Generation(0));
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let st = match self.stat_path(&rel_path) {
            Ok(st) => st,
            Err(e) => return reply.error(e),
        };
        let attr = self.stat_to_attr(&st, &rel_path);
        let ttl = self.ttl_for(&rel_path);
        reply.attr(&ttl, &attr);
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: fuser::OpenFlags, reply: ReplyOpen) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let c_path = path_to_cstring(&rel_path);
        let fd = unsafe {
            libc::openat(
                self.root_raw(),
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY,
            )
        };
        if fd == -1 {
            return reply.error(last_errno());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let fh = self.alloc_fh();
        self.dir_handles.lock().unwrap().insert(fh.0, owned);
        reply.opened(fh, FopenFlags::empty());
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let parent_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        let dir_fd = {
            let handles = self.dir_handles.lock().unwrap();
            match handles.get(&fh.0) {
                Some(fd) => fd.as_raw_fd(),
                None => return reply.error(Errno::EBADF),
            }
        };

        // Dup the fd so nix::dir::Dir can own it without closing our handle
        let dup_fd = unsafe { libc::dup(dir_fd) };
        if dup_fd == -1 {
            return reply.error(last_errno());
        }
        let owned_dup = unsafe { OwnedFd::from_raw_fd(dup_fd) };
        let mut dir = match nix::dir::Dir::from_fd(owned_dup) {
            Ok(d) => d,
            Err(e) => return reply.error(Errno::from_i32(e as i32)),
        };

        let mut entries: Vec<(u64, std::ffi::OsString, FileType)> = Vec::new();

        // Reset to beginning
        unsafe { libc::lseek(dup_fd, 0, libc::SEEK_SET) };

        for entry in dir.iter() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            let child_path = if name == "." {
                parent_path.clone()
            } else if name == ".." {
                parent_path
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from("."))
            } else {
                parent_path.join(name)
            };

            let child_ino = match entry.ino() {
                0 => continue,
                i => {
                    self.inodes.lock().unwrap().insert(child_path, i);
                    i
                }
            };

            let ft = match entry.file_type() {
                Some(nix::dir::Type::Directory) => FileType::Directory,
                Some(nix::dir::Type::File) => FileType::RegularFile,
                Some(nix::dir::Type::Symlink) => FileType::Symlink,
                Some(nix::dir::Type::BlockDevice) => FileType::BlockDevice,
                Some(nix::dir::Type::CharacterDevice) => FileType::CharDevice,
                Some(nix::dir::Type::Fifo) => FileType::NamedPipe,
                Some(nix::dir::Type::Socket) => FileType::Socket,
                _ => FileType::RegularFile,
            };

            entries.push((child_ino, name.to_os_string(), ft));
        }

        for (i, (child_ino, name, ft)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(INodeNo(*child_ino), (i + 1) as u64, *ft, name) {
                break;
            }
        }
        reply.ok();
    }

    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: fuser::OpenFlags,
        reply: ReplyEmpty,
    ) {
        self.dir_handles.lock().unwrap().remove(&fh.0);
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: fuser::OpenFlags, reply: ReplyOpen) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let c_path = path_to_cstring(&rel_path);
        let open_flags = flags.0 & !(libc::O_APPEND | libc::O_TRUNC | libc::O_CREAT);
        let fd = unsafe { libc::openat(self.root_raw(), c_path.as_ptr(), open_flags) };
        if fd == -1 {
            return reply.error(last_errno());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let fh = self.alloc_fh();
        self.handles.lock().unwrap().insert(fh.0, owned);
        reply.opened(fh, FopenFlags::empty());
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        let handles = self.handles.lock().unwrap();
        let fd = match handles.get(&fh.0) {
            Some(fd) => fd.as_raw_fd(),
            None => return reply.error(Errno::EBADF),
        };

        let filename = rel_path.file_name().unwrap_or_default();
        if self.matcher.is_intercepted(filename) {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut st) } == -1 {
                return reply.error(last_errno());
            }
            match InterceptMatcher::assemble(
                fd,
                st.st_size as u64,
                &rel_path,
                self.provider.as_ref(),
            ) {
                Ok(full) => {
                    let start = (offset as usize).min(full.len());
                    let end = (start + size as usize).min(full.len());
                    reply.data(&full[start..end]);
                }
                Err(e) => {
                    tracing::error!("assemble failed for {}: {e}", rel_path.display());
                    reply.error(Errno::EIO);
                }
            }
        } else {
            let mut buf = vec![0u8; size as usize];
            let n = unsafe {
                libc::pread(
                    fd,
                    buf.as_mut_ptr().cast(),
                    size as usize,
                    offset as libc::off_t,
                )
            };
            if n == -1 {
                reply.error(last_errno());
            } else {
                reply.data(&buf[..n as usize]);
            }
        }
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.handles.lock().unwrap().remove(&fh.0);
        reply.ok();
    }

    fn access(&self, _req: &Request, ino: INodeNo, mask: fuser::AccessFlags, reply: ReplyEmpty) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let c_path = path_to_cstring(&rel_path);
        let rc = unsafe { libc::faccessat(self.root_raw(), c_path.as_ptr(), mask.bits(), 0) };
        if rc == -1 {
            reply.error(last_errno());
        } else {
            reply.ok();
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let c_path = path_to_cstring(&rel_path);
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        let n = unsafe {
            libc::readlinkat(
                self.root_raw(),
                c_path.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n == -1 {
            reply.error(last_errno());
        } else {
            reply.data(&buf[..n as usize]);
        }
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::fstatvfs(self.root_raw(), &mut st) };
        if rc == -1 {
            return reply.error(last_errno());
        }
        reply.statfs(
            st.f_blocks,
            st.f_bfree,
            st.f_bavail,
            st.f_files,
            st.f_ffree,
            st.f_bsize as u32,
            st.f_namemax as u32,
            st.f_frsize as u32,
        );
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        let handles = self.handles.lock().unwrap();
        let fd = match handles.get(&fh.0) {
            Some(fd) => fd.as_raw_fd(),
            None => return reply.error(Errno::EBADF),
        };
        let n =
            unsafe { libc::pwrite(fd, data.as_ptr().cast(), data.len(), offset as libc::off_t) };
        if n == -1 {
            reply.error(last_errno());
        } else {
            reply.written(n as u32);
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<FileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<fuser::BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let rel_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };

        let owned_fd: Option<OwnedFd>;
        let fd = if let Some(fh) = fh {
            let handles = self.handles.lock().unwrap();
            match handles.get(&fh.0) {
                Some(f) => f.as_raw_fd(),
                None => return reply.error(Errno::EBADF),
            }
        } else {
            let c_path = path_to_cstring(&rel_path);
            let raw = unsafe { libc::openat(self.root_raw(), c_path.as_ptr(), libc::O_RDWR) };
            if raw == -1 {
                let raw2 =
                    unsafe { libc::openat(self.root_raw(), c_path.as_ptr(), libc::O_WRONLY) };
                if raw2 == -1 {
                    return reply.error(last_errno());
                }
                owned_fd = Some(unsafe { OwnedFd::from_raw_fd(raw2) });
            } else {
                owned_fd = Some(unsafe { OwnedFd::from_raw_fd(raw) });
            }
            owned_fd.as_ref().unwrap().as_raw_fd()
        };

        if let Some(mode) = mode
            && unsafe { libc::fchmod(fd, mode) } == -1
        {
            return reply.error(last_errno());
        }

        if uid.is_some() || gid.is_some() {
            let u = uid.unwrap_or(u32::MAX);
            let g = gid.unwrap_or(u32::MAX);
            if unsafe { libc::fchown(fd, u, g) } == -1 {
                return reply.error(last_errno());
            }
        }

        if let Some(size) = size
            && unsafe { libc::ftruncate(fd, size as libc::off_t) } == -1
        {
            return reply.error(last_errno());
        }

        if atime.is_some() || mtime.is_some() {
            let times = [
                time_or_now_to_timespec(atime),
                time_or_now_to_timespec(mtime),
            ];
            if unsafe { libc::futimens(fd, times.as_ptr()) } == -1 {
                return reply.error(last_errno());
            }
        }

        let st = match self.stat_path(&rel_path) {
            Ok(st) => st,
            Err(e) => return reply.error(e),
        };
        let attr = self.stat_to_attr(&st, &rel_path);
        let ttl = self.ttl_for(&rel_path);
        reply.attr(&ttl, &attr);
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let parent_path = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let child_path = parent_path.join(name);
        let c_path = path_to_cstring(&child_path);
        let fd = unsafe {
            libc::openat(
                self.root_raw(),
                c_path.as_ptr(),
                flags | libc::O_CREAT,
                mode,
            )
        };
        if fd == -1 {
            return reply.error(last_errno());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };

        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } == -1 {
            return reply.error(last_errno());
        }

        self.inodes
            .lock()
            .unwrap()
            .insert(child_path.clone(), st.st_ino);
        let attr = self.stat_to_attr(&st, &child_path);
        let ttl = self.ttl_for(&child_path);
        let fh = self.alloc_fh();
        self.handles.lock().unwrap().insert(fh.0, owned);
        reply.created(&ttl, &attr, Generation(0), fh, FopenFlags::empty());
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let child_path = parent_path.join(name);
        let c_path = path_to_cstring(&child_path);
        let rc = unsafe { libc::unlinkat(self.root_raw(), c_path.as_ptr(), 0) };
        if rc == -1 {
            return reply.error(last_errno());
        }
        self.inodes.lock().unwrap().remove_path(&child_path);
        reply.ok();
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let parent_path = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let child_path = parent_path.join(name);
        let c_path = path_to_cstring(&child_path);
        let rc = unsafe { libc::mkdirat(self.root_raw(), c_path.as_ptr(), mode) };
        if rc == -1 {
            return reply.error(last_errno());
        }
        let st = match self.stat_path(&child_path) {
            Ok(st) => st,
            Err(e) => return reply.error(e),
        };
        self.inodes
            .lock()
            .unwrap()
            .insert(child_path.clone(), st.st_ino);
        let attr = self.stat_to_attr(&st, &child_path);
        reply.entry(&TTL_NORMAL, &attr, Generation(0));
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let parent_path = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let child_path = parent_path.join(name);
        let c_path = path_to_cstring(&child_path);
        let rc = unsafe { libc::unlinkat(self.root_raw(), c_path.as_ptr(), libc::AT_REMOVEDIR) };
        if rc == -1 {
            return reply.error(last_errno());
        }
        self.inodes.lock().unwrap().remove_path(&child_path);
        reply.ok();
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        _flags: fuser::RenameFlags,
        reply: ReplyEmpty,
    ) {
        let old_parent = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let new_parent = match self.inode_path(newparent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let old_path = old_parent.join(name);
        let new_path = new_parent.join(newname);
        let c_old = path_to_cstring(&old_path);
        let c_new = path_to_cstring(&new_path);
        let rc = unsafe {
            libc::renameat(
                self.root_raw(),
                c_old.as_ptr(),
                self.root_raw(),
                c_new.as_ptr(),
            )
        };
        if rc == -1 {
            return reply.error(last_errno());
        }
        self.inodes.lock().unwrap().rename(&old_path, new_path);
        reply.ok();
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        reply.ok();
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        datasync: bool,
        reply: ReplyEmpty,
    ) {
        let handles = self.handles.lock().unwrap();
        let fd = match handles.get(&fh.0) {
            Some(fd) => fd.as_raw_fd(),
            None => return reply.error(Errno::EBADF),
        };
        let rc = if datasync {
            unsafe { libc::fdatasync(fd) }
        } else {
            unsafe { libc::fsync(fd) }
        };
        if rc == -1 {
            reply.error(last_errno());
        } else {
            reply.ok();
        }
    }

    fn symlink(
        &self,
        _req: &Request,
        parent: INodeNo,
        link_name: &OsStr,
        target: &Path,
        reply: ReplyEntry,
    ) {
        let parent_path = match self.inode_path(parent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let link_path = parent_path.join(link_name);
        let c_target = CString::new(target.as_os_str().as_bytes()).unwrap_or_default();
        let c_link = path_to_cstring(&link_path);
        let rc = unsafe { libc::symlinkat(c_target.as_ptr(), self.root_raw(), c_link.as_ptr()) };
        if rc == -1 {
            return reply.error(last_errno());
        }
        let st = match self.stat_path(&link_path) {
            Ok(st) => st,
            Err(e) => return reply.error(e),
        };
        self.inodes
            .lock()
            .unwrap()
            .insert(link_path.clone(), st.st_ino);
        let attr = self.stat_to_attr(&st, &link_path);
        reply.entry(&TTL_NORMAL, &attr, Generation(0));
    }

    fn link(
        &self,
        _req: &Request,
        ino: INodeNo,
        newparent: INodeNo,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        let old_path = match self.inode_path(ino) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let new_parent = match self.inode_path(newparent) {
            Some(p) => p,
            None => return reply.error(Errno::ENOENT),
        };
        let new_path = new_parent.join(newname);
        let c_old = path_to_cstring(&old_path);
        let c_new = path_to_cstring(&new_path);
        let rc = unsafe {
            libc::linkat(
                self.root_raw(),
                c_old.as_ptr(),
                self.root_raw(),
                c_new.as_ptr(),
                0,
            )
        };
        if rc == -1 {
            return reply.error(last_errno());
        }
        let st = match self.stat_path(&new_path) {
            Ok(st) => st,
            Err(e) => return reply.error(e),
        };
        self.inodes
            .lock()
            .unwrap()
            .insert(new_path.clone(), st.st_ino);
        let attr = self.stat_to_attr(&st, &new_path);
        reply.entry(&TTL_NORMAL, &attr, Generation(0));
    }
}

fn path_to_cstring(p: &Path) -> CString {
    CString::new(p.as_os_str().as_bytes()).unwrap_or_else(|_| CString::new(".").unwrap())
}

fn last_errno() -> Errno {
    Errno::from(std::io::Error::last_os_error())
}

fn timespec_to_systime(sec: i64, nsec: i64) -> SystemTime {
    if sec >= 0 {
        UNIX_EPOCH + Duration::new(sec as u64, nsec as u32)
    } else {
        UNIX_EPOCH
    }
}

fn mode_to_filetype(mode: u32) -> FileType {
    match mode & libc::S_IFMT {
        libc::S_IFDIR => FileType::Directory,
        libc::S_IFREG => FileType::RegularFile,
        libc::S_IFLNK => FileType::Symlink,
        libc::S_IFBLK => FileType::BlockDevice,
        libc::S_IFCHR => FileType::CharDevice,
        libc::S_IFIFO => FileType::NamedPipe,
        libc::S_IFSOCK => FileType::Socket,
        _ => FileType::RegularFile,
    }
}

fn time_or_now_to_timespec(t: Option<TimeOrNow>) -> libc::timespec {
    match t {
        Some(TimeOrNow::SpecificTime(st)) => {
            let d = st.duration_since(UNIX_EPOCH).unwrap_or_default();
            libc::timespec {
                tv_sec: d.as_secs() as libc::time_t,
                tv_nsec: d.subsec_nanos() as libc::c_long,
            }
        }
        Some(TimeOrNow::Now) => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_NOW,
        },
        None => libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
    }
}
