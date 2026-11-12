//! Read-only virtio-fs FUSE server. Paths are single components opened from a
//! retained directory descriptor. Symlink text is returned to the guest and
//! never followed for host I/O. DAX, idmap, and POSIX ACLs are not offered.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io::{self, ErrorKind};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
use std::rc::Rc;

use super::MAX_MESSAGE;

const HEADER: usize = 40;
const OUT_HEADER: usize = 16;
const ATTR_LEN: usize = 88;
const ENTRY_LEN: usize = 40 + ATTR_LEN;
const MAX_WRITE: u32 = 65536;
const MAX_NAME: usize = 255;
const MAX_NODES: usize = 65536;
const MAX_HANDLES: usize = 1024;

const LOOKUP: u32 = 1;
const FORGET: u32 = 2;
const GETATTR: u32 = 3;
const SETATTR: u32 = 4;
const READLINK: u32 = 5;
const SYMLINK: u32 = 6;
const MKNOD: u32 = 8;
const MKDIR: u32 = 9;
const UNLINK: u32 = 10;
const RMDIR: u32 = 11;
const RENAME: u32 = 12;
const LINK: u32 = 13;
const OPEN: u32 = 14;
const READ: u32 = 15;
const WRITE: u32 = 16;
const STATFS: u32 = 17;
const RELEASE: u32 = 18;
const FSYNC: u32 = 20;
const SETXATTR: u32 = 21;
const GETXATTR: u32 = 22;
const LISTXATTR: u32 = 23;
const REMOVEXATTR: u32 = 24;
const FLUSH: u32 = 25;
const INIT: u32 = 26;
const OPENDIR: u32 = 27;
const READDIR: u32 = 28;
const RELEASEDIR: u32 = 29;
const FSYNCDIR: u32 = 30;
const ACCESS: u32 = 34;
const CREATE: u32 = 35;
const INTERRUPT: u32 = 36;
const DESTROY: u32 = 38;
const BATCH_FORGET: u32 = 42;
const FALLOCATE: u32 = 43;
const RENAME2: u32 = 45;
const COPY_FILE_RANGE: u32 = 47;

const EINVAL: i32 = 22;
const EIO: i32 = 5;
const EBADF: i32 = 9;
const ENOTDIR: i32 = 20;
const EISDIR: i32 = 21;
const EROFS: i32 = 30;
const ENAMETOOLONG: i32 = 36;
const ELOOP: i32 = 40;
const ENOSYS: i32 = 38;
const ENOTSUP: i32 = 95;
const ESTALE: i32 = 116;

const FUSE_ASYNC_READ: u32 = 1 << 0;
const FUSE_AUTO_INVAL_DATA: u32 = 1 << 12;
const FUSE_PARALLEL_DIROPS: u32 = 1 << 18;
const FUSE_MAX_PAGES: u32 = 1 << 22;
const FUSE_CACHE_SYMLINKS: u32 = 1 << 23;
const OFFERED_FLAGS: u32 = FUSE_ASYNC_READ
    | FUSE_AUTO_INVAL_DATA
    | FUSE_PARALLEL_DIROPS
    | FUSE_MAX_PAGES
    | FUSE_CACHE_SYMLINKS;

const LINUX_O_ACCMODE: u32 = 3;
const LINUX_O_TRUNC: u32 = 512;

enum Dispatch {
    Reply(Vec<u8>),
    Silent,
    Fatal(&'static str),
}

impl From<i32> for Dispatch {
    fn from(errno: i32) -> Self {
        Dispatch::Reply(errno.to_le_bytes().to_vec())
    }
}

struct Node {
    file: File,
    location: RefCell<(Option<Rc<Node>>, Vec<u8>)>,
}

impl Node {
    fn depth(&self) -> Result<usize, i32> {
        let mut depth = 0;
        let mut parent = self.location.borrow().0.clone();
        while let Some(node) = parent {
            depth += 1;
            if depth > 256 {
                return Err(ENAMETOOLONG);
            }
            parent = node.location.borrow().0.clone();
        }
        Ok(depth)
    }

    fn meta(&self) -> Result<Metadata, i32> {
        self.file.metadata().map_err(errno)
    }

    fn child(self: &Rc<Self>, name: &[u8]) -> Result<Rc<Self>, i32> {
        if name == b"." {
            return Ok(self.clone());
        }
        if name == b".." {
            return Ok(self
                .location
                .borrow()
                .0
                .clone()
                .unwrap_or_else(|| self.clone()));
        }
        if !self.meta()?.is_dir() {
            return Err(ENOTDIR);
        }
        if self.depth()? >= 256 {
            return Err(ENAMETOOLONG);
        }
        let c = component(name)?;
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC | libc::O_SYMLINK,
            )
        };
        let fd = cvt(fd)?;
        let node = Rc::new(Self {
            file: unsafe { File::from_raw_fd(fd) },
            location: RefCell::new((Some(self.clone()), name.to_vec())),
        });
        let meta = node.meta()?;
        if !meta.is_file() && !meta.is_dir() && !meta.file_type().is_symlink() {
            return Err(ENOTSUP);
        }
        Ok(node)
    }

    fn reopen_file(&self) -> Result<File, i32> {
        let meta = self.meta()?;
        if meta.file_type().is_symlink() {
            return Err(ELOOP);
        }
        if meta.is_dir() {
            return Err(EISDIR);
        }
        if !meta.is_file() {
            return Err(EINVAL);
        }
        let (parent, name) = self.location.borrow().clone();
        let parent = parent.ok_or(ESTALE)?;
        let c = component(&name)?;
        let fd = cvt(unsafe {
            libc::openat(
                parent.file.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        let file = unsafe { File::from_raw_fd(fd) };
        let current = file.metadata().map_err(errno)?;
        if (meta.dev(), meta.ino()) != (current.dev(), current.ino()) {
            return Err(ESTALE);
        }
        Ok(file)
    }

    fn read_link(&self) -> Result<Vec<u8>, i32> {
        if !self.meta()?.file_type().is_symlink() {
            return Err(EINVAL);
        }
        let (parent, name) = self.location.borrow().clone();
        let parent = parent.ok_or(ESTALE)?;
        let c = component(&name)?;
        let mut buf = vec![0u8; 4096];
        let n = unsafe {
            libc::readlinkat(
                parent.file.as_raw_fd(),
                c.as_ptr(),
                buf.as_mut_ptr().cast(),
                buf.len(),
            )
        };
        if n < 0 {
            return Err(errno(io::Error::last_os_error()));
        }
        let n = n as usize;
        if n == buf.len() {
            return Err(ENAMETOOLONG);
        }
        buf.truncate(n);
        Ok(buf)
    }
}

struct Inode {
    node: Rc<Node>,
    nlookup: u64,
    key: (u64, u64),
    parent: u64,
}

struct DirEnt {
    name: Vec<u8>,
    nodeid: u64,
    kind: u32,
}

struct Handle {
    file: Option<File>,
    dir: Option<Vec<DirEnt>>,
}

pub struct Server {
    root: Rc<Node>,
    inodes: HashMap<u64, Inode>,
    by_key: HashMap<(u64, u64), u64>,
    handles: HashMap<u64, Handle>,
    next_id: u64,
    next_fh: u64,
    negotiated: bool,
}

impl Server {
    pub fn new(path: &Path, read_only: bool) -> io::Result<Self> {
        if !read_only {
            return Err(io::Error::new(
                ErrorKind::ReadOnlyFilesystem,
                "virtiofs helper is read-only",
            ));
        }
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let root = Rc::new(Node {
            file: unsafe { File::from_raw_fd(fd) },
            location: RefCell::new((None, Vec::new())),
        });
        let mut server = Self {
            root: root.clone(),
            inodes: HashMap::new(),
            by_key: HashMap::new(),
            handles: HashMap::new(),
            next_id: 2,
            next_fh: 1,
            negotiated: false,
        };
        let key = dev_ino(&root).map_err(|e| io::Error::from_raw_os_error(e))?;
        server.inodes.insert(
            1,
            Inode {
                node: root,
                nlookup: 1,
                key,
                parent: 1,
            },
        );
        server.by_key.insert(key, 1);
        Ok(server)
    }

    pub fn reset(&mut self) {
        let root = self.inodes.remove(&1);
        self.inodes.clear();
        self.by_key.clear();
        self.handles.clear();
        self.next_id = 2;
        self.next_fh = 1;
        self.negotiated = false;
        if let Some(mut root) = root {
            root.nlookup = 1;
            self.by_key.insert(root.key, 1);
            self.inodes.insert(1, root);
        }
    }

    /// `Ok(None)` is a no-reply opcode. `Err` is a malformed message.
    pub fn request(&mut self, bytes: &[u8]) -> Result<Option<Vec<u8>>, String> {
        match self.dispatch(bytes) {
            Dispatch::Reply(body) => Ok(Some(body)),
            Dispatch::Silent => Ok(None),
            Dispatch::Fatal(why) => Err(why.into()),
        }
    }

    fn dispatch(&mut self, bytes: &[u8]) -> Dispatch {
        if bytes.len() < HEADER || bytes.len() > MAX_MESSAGE {
            return Dispatch::Fatal("fuse message length");
        }
        let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        if len != bytes.len() {
            return Dispatch::Fatal("fuse header length");
        }
        let opcode = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let unique = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let nodeid = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let ext = u16::from_le_bytes(bytes[36..38].try_into().unwrap());
        if ext != 0 {
            return Dispatch::Fatal("fuse extensions are not supported");
        }
        let body = &bytes[HEADER..];
        if !self.negotiated && opcode != INIT {
            return Dispatch::Reply(err(unique, EIO));
        }
        if matches!(opcode, FORGET | INTERRUPT | BATCH_FORGET) {
            return match self.silent(opcode, nodeid, body) {
                Ok(()) => Dispatch::Silent,
                Err(Dispatch::Fatal(why)) => Dispatch::Fatal(why),
                Err(_) => Dispatch::Fatal("fuse no-reply body"),
            };
        }
        let result = self.op(opcode, nodeid, body);
        match result {
            Ok(payload) => Dispatch::Reply(ok(unique, &payload)),
            Err(Dispatch::Fatal(why)) => Dispatch::Fatal(why),
            Err(Dispatch::Reply(payload)) => Dispatch::Reply(payload_or_err(unique, payload)),
            Err(Dispatch::Silent) => Dispatch::Silent,
        }
    }

    fn silent(&mut self, opcode: u32, nodeid: u64, body: &[u8]) -> Result<(), Dispatch> {
        match opcode {
            FORGET => {
                if body.len() < 8 {
                    return Err(Dispatch::Fatal("short forget"));
                }
                let n = u64::from_le_bytes(body[0..8].try_into().unwrap());
                self.forget(nodeid, n);
                Ok(())
            }
            INTERRUPT => {
                if body.len() < 8 {
                    return Err(Dispatch::Fatal("short interrupt"));
                }
                Ok(())
            }
            BATCH_FORGET => {
                if body.len() < 8 {
                    return Err(Dispatch::Fatal("short batch forget"));
                }
                let count = u32::from_le_bytes(body[0..4].try_into().unwrap()) as usize;
                if body.len() != 8 + count * 16 {
                    return Err(Dispatch::Fatal("batch forget length"));
                }
                for i in 0..count {
                    let at = 8 + i * 16;
                    let id = u64::from_le_bytes(body[at..at + 8].try_into().unwrap());
                    let n = u64::from_le_bytes(body[at + 8..at + 16].try_into().unwrap());
                    self.forget(id, n);
                }
                Ok(())
            }
            _ => Err(Dispatch::Fatal("not silent")),
        }
    }

    fn op(&mut self, opcode: u32, nodeid: u64, body: &[u8]) -> Result<Vec<u8>, Dispatch> {
        match opcode {
            INIT => self.init(body),
            DESTROY => {
                self.reset();
                Ok(Vec::new())
            }
            LOOKUP => self.lookup(nodeid, body),
            GETATTR => self.getattr(nodeid, body),
            READLINK => self.readlink(nodeid),
            OPEN => self.open(nodeid, body, false),
            OPENDIR => self.open(nodeid, body, true),
            READ => self.read(body, false),
            READDIR => self.read(body, true),
            RELEASE | RELEASEDIR => self.release(body),
            FLUSH | FSYNC | FSYNCDIR | ACCESS => self.access_or_nop(opcode, nodeid, body),
            STATFS => self.statfs(),
            SETATTR | SYMLINK | MKNOD | MKDIR | UNLINK | RMDIR | RENAME | LINK | WRITE | CREATE
            | FALLOCATE | RENAME2 | COPY_FILE_RANGE | SETXATTR | REMOVEXATTR => Err(EROFS.into()),
            GETXATTR | LISTXATTR => Err(ENOSYS.into()),
            _ => Err(ENOSYS.into()),
        }
    }

    fn init(&mut self, body: &[u8]) -> Result<Vec<u8>, Dispatch> {
        if body.len() < 16 {
            return Err(Dispatch::Fatal("short init"));
        }
        let major = u32::from_le_bytes(body[0..4].try_into().unwrap());
        let minor = u32::from_le_bytes(body[4..8].try_into().unwrap());
        let readahead = u32::from_le_bytes(body[8..12].try_into().unwrap());
        let flags = u32::from_le_bytes(body[12..16].try_into().unwrap());
        if major != 7 {
            let mut out = vec![0u8; 64];
            put_u32(&mut out, 0, 7);
            put_u32(&mut out, 4, 31);
            self.negotiated = false;
            return Ok(out);
        }
        let minor = minor.min(31);
        let mut out = vec![0u8; 64];
        put_u32(&mut out, 0, 7);
        put_u32(&mut out, 4, minor);
        put_u32(&mut out, 8, readahead.min(MAX_WRITE));
        put_u32(&mut out, 12, flags & OFFERED_FLAGS);
        put_u16(&mut out, 16, 16);
        put_u16(&mut out, 18, 12);
        put_u32(&mut out, 20, MAX_WRITE);
        put_u32(&mut out, 24, 1);
        put_u16(&mut out, 28, (MAX_WRITE / 4096) as u16);
        self.negotiated = true;
        self.handles.clear();
        Ok(out)
    }

    fn lookup(&mut self, parent: u64, body: &[u8]) -> Result<Vec<u8>, Dispatch> {
        let name = c_name(body)?;
        let id = if name == b"." {
            self.bump(parent)?;
            parent
        } else if name == b".." {
            let id = self.parent_of(parent)?;
            self.bump(id)?;
            id
        } else {
            let node = self.node(parent)?.child(name)?;
            let id = self.adopt(node, parent)?;
            self.bump(id)?;
            id
        };
        self.entry(id)
    }

    fn getattr(&mut self, nodeid: u64, body: &[u8]) -> Result<Vec<u8>, Dispatch> {
        if body.len() < 16 {
            return Err(Dispatch::Fatal("short getattr"));
        }
        let flags = u32::from_le_bytes(body[0..4].try_into().unwrap());
        if flags & 1 != 0 {
            let fh = u64::from_le_bytes(body[8..16].try_into().unwrap());
            if !self.handles.contains_key(&fh) {
                return Err(EBADF.into());
            }
        }
        let node = self.node(nodeid)?;
        let meta = node.meta()?;
        let mut out = vec![0u8; 16];
        out.extend(attr_bytes(nodeid, &meta));
        Ok(out)
    }

    fn readlink(&mut self, nodeid: u64) -> Result<Vec<u8>, Dispatch> {
        Ok(self.node(nodeid)?.read_link()?)
    }

    fn open(&mut self, nodeid: u64, body: &[u8], dir: bool) -> Result<Vec<u8>, Dispatch> {
        if body.len() < 8 {
            return Err(Dispatch::Fatal("short open"));
        }
        let flags = u32::from_le_bytes(body[0..4].try_into().unwrap());
        if flags & LINUX_O_ACCMODE != 0 || flags & LINUX_O_TRUNC != 0 {
            return Err(EROFS.into());
        }
        let node = self.node(nodeid)?;
        let meta = node.meta()?;
        if dir {
            if !meta.is_dir() {
                return Err(ENOTDIR.into());
            }
        } else if meta.is_dir() {
            return Err(EISDIR.into());
        } else if meta.file_type().is_symlink() {
            return Err(ELOOP.into());
        }
        if self.handles.len() >= MAX_HANDLES {
            return Err(24.into());
        }
        let handle = if dir {
            Handle {
                file: None,
                dir: Some(self.snapshot(nodeid, &node)?),
            }
        } else {
            Handle {
                file: Some(node.reopen_file()?),
                dir: None,
            }
        };
        let fh = self.next_fh;
        self.next_fh = self.next_fh.wrapping_add(1).max(1);
        self.handles.insert(fh, handle);
        let mut out = vec![0u8; 16];
        put_u64(&mut out, 0, fh);
        Ok(out)
    }

    fn read(&mut self, body: &[u8], dir: bool) -> Result<Vec<u8>, Dispatch> {
        if body.len() < 24 {
            return Err(Dispatch::Fatal("short read"));
        }
        let fh = u64::from_le_bytes(body[0..8].try_into().unwrap());
        let offset = u64::from_le_bytes(body[8..16].try_into().unwrap());
        let size = u32::from_le_bytes(body[16..20].try_into().unwrap()).min(MAX_WRITE) as usize;
        let handle = self.handles.get(&fh).ok_or(EBADF)?;
        if dir {
            let entries = handle.dir.as_ref().ok_or(ENOTDIR)?;
            Ok(encode_dir(entries, offset as usize, size))
        } else {
            let file = handle.file.as_ref().ok_or(EISDIR)?;
            let mut buf = vec![0u8; size];
            let n = file.read_at(&mut buf, offset).map_err(errno)?;
            buf.truncate(n);
            Ok(buf)
        }
    }

    fn release(&mut self, body: &[u8]) -> Result<Vec<u8>, Dispatch> {
        if body.len() < 8 {
            return Err(Dispatch::Fatal("short release"));
        }
        let fh = u64::from_le_bytes(body[0..8].try_into().unwrap());
        if self.handles.remove(&fh).is_none() {
            return Err(EBADF.into());
        }
        Ok(Vec::new())
    }

    fn access_or_nop(
        &mut self,
        opcode: u32,
        nodeid: u64,
        body: &[u8],
    ) -> Result<Vec<u8>, Dispatch> {
        let _ = self.node(nodeid)?;
        if opcode == ACCESS {
            if body.len() < 4 {
                return Err(Dispatch::Fatal("short access"));
            }
            let mask = u32::from_le_bytes(body[0..4].try_into().unwrap());
            if mask & 2 != 0 {
                return Err(EROFS.into());
            }
        }
        Ok(Vec::new())
    }

    fn statfs(&mut self) -> Result<Vec<u8>, Dispatch> {
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatfs(self.root.file.as_raw_fd(), &mut st) } < 0 {
            return Err(errno(io::Error::last_os_error()).into());
        }
        // fuse_kstatfs is 80 bytes: five u64 counters, four u32 fields,
        // followed by six reserved u32 words.
        let mut out = vec![0u8; 80];
        put_u64(&mut out, 0, st.f_blocks);
        put_u64(&mut out, 8, st.f_bfree);
        put_u64(&mut out, 16, st.f_bavail);
        put_u64(&mut out, 24, st.f_files);
        put_u64(&mut out, 32, st.f_ffree);
        put_u32(&mut out, 40, st.f_bsize as u32);
        put_u32(&mut out, 44, 255);
        put_u32(&mut out, 48, st.f_bsize as u32);
        Ok(out)
    }

    fn snapshot(&mut self, nodeid: u64, node: &Rc<Node>) -> Result<Vec<DirEnt>, i32> {
        let parent = self.parent_of(nodeid)?;
        let mut entries = vec![
            DirEnt {
                name: b".".to_vec(),
                nodeid,
                kind: 4,
            },
            DirEnt {
                name: b"..".to_vec(),
                nodeid: parent,
                kind: 4,
            },
        ];
        for (name, kind) in list_dir(node.file.as_raw_fd())? {
            let child = match node.child(&name) {
                Ok(child) => child,
                Err(_) => continue,
            };
            let id = self.adopt(child, nodeid)?;
            entries.push(DirEnt {
                name,
                nodeid: id,
                kind,
            });
        }
        Ok(entries)
    }

    fn entry(&self, id: u64) -> Result<Vec<u8>, Dispatch> {
        let node = self.node(id)?;
        let mut out = vec![0u8; 40];
        put_u64(&mut out, 0, id);
        let meta = node.meta()?;
        out.extend(attr_bytes(id, &meta));
        debug_assert_eq!(out.len(), ENTRY_LEN);
        Ok(out)
    }

    fn node(&self, id: u64) -> Result<Rc<Node>, i32> {
        self.inodes
            .get(&id)
            .map(|inode| inode.node.clone())
            .ok_or(ESTALE)
    }

    fn parent_of(&self, id: u64) -> Result<u64, i32> {
        self.inodes.get(&id).map(|inode| inode.parent).ok_or(ESTALE)
    }

    fn bump(&mut self, id: u64) -> Result<(), i32> {
        let inode = self.inodes.get_mut(&id).ok_or(ESTALE)?;
        inode.nlookup = inode.nlookup.saturating_add(1);
        Ok(())
    }

    fn adopt(&mut self, node: Rc<Node>, parent: u64) -> Result<u64, i32> {
        let key = dev_ino(&node)?;
        if let Some(id) = self.by_key.get(&key).copied() {
            return Ok(id);
        }
        if self.inodes.len() >= MAX_NODES {
            return Err(28);
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(2);
        self.by_key.insert(key, id);
        self.inodes.insert(
            id,
            Inode {
                node,
                nlookup: 0,
                key,
                parent,
            },
        );
        Ok(id)
    }

    fn forget(&mut self, id: u64, n: u64) {
        if id == 1 {
            return;
        }
        let Some(inode) = self.inodes.get_mut(&id) else {
            return;
        };
        inode.nlookup = inode.nlookup.saturating_sub(n);
        if inode.nlookup == 0 {
            let key = inode.key;
            self.inodes.remove(&id);
            self.by_key.remove(&key);
        }
    }
}

fn payload_or_err(unique: u64, payload: Vec<u8>) -> Vec<u8> {
    if payload.len() == 4 {
        let errno = i32::from_le_bytes(payload.as_slice().try_into().unwrap());
        if errno > 0 {
            return err(unique, errno);
        }
    }
    ok(unique, &payload)
}

fn list_dir(fd: RawFd) -> Result<Vec<(Vec<u8>, u32)>, i32> {
    let dup = cvt(unsafe { libc::dup(fd) })?;
    let dir = unsafe { libc::fdopendir(dup) };
    if dir.is_null() {
        unsafe { libc::close(dup) };
        return Err(errno(io::Error::last_os_error()));
    }
    let mut out = Vec::new();
    let result = loop {
        unsafe {
            *__error() = 0;
        }
        let ent = unsafe { libc::readdir(dir) };
        if ent.is_null() {
            let err = unsafe { *__error() };
            if err != 0 {
                break Err(errno(io::Error::from_raw_os_error(err)));
            }
            break Ok(out);
        }
        let ent = unsafe { &*ent };
        let namelen = ent.d_namlen as usize;
        if namelen > MAX_NAME {
            continue;
        }
        let name = unsafe { std::slice::from_raw_parts(ent.d_name.as_ptr().cast::<u8>(), namelen) };
        if name == b"." || name == b".." {
            continue;
        }
        let kind = match ent.d_type {
            4 | 8 | 10 => ent.d_type as u32,
            _ => 0,
        };
        out.push((name.to_vec(), kind));
    };
    unsafe { libc::closedir(dir) };
    result
}

fn encode_dir(entries: &[DirEnt], offset: usize, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (index, entry) in entries.iter().enumerate().skip(offset) {
        let raw = 24 + entry.name.len();
        let padded = (raw + 7) & !7;
        if out.len() + padded > size {
            break;
        }
        let start = out.len();
        out.resize(start + padded, 0);
        put_u64(&mut out, start, entry.nodeid);
        put_u64(&mut out, start + 8, (index + 1) as u64);
        put_u32(&mut out, start + 16, entry.name.len() as u32);
        put_u32(&mut out, start + 20, entry.kind);
        out[start + 24..start + 24 + entry.name.len()].copy_from_slice(&entry.name);
    }
    out
}

fn attr_bytes(id: u64, meta: &Metadata) -> Vec<u8> {
    let mut out = vec![0u8; ATTR_LEN];
    put_u64(&mut out, 0, id);
    put_u64(&mut out, 8, meta.size());
    put_u64(&mut out, 16, meta.size().div_ceil(512));
    put_u64(&mut out, 24, meta.atime() as u64);
    put_u64(&mut out, 32, meta.mtime() as u64);
    put_u64(&mut out, 40, meta.ctime() as u64);
    put_u32(&mut out, 48, meta.atime_nsec() as u32);
    put_u32(&mut out, 52, meta.mtime_nsec() as u32);
    put_u32(&mut out, 56, meta.ctime_nsec() as u32);
    put_u32(&mut out, 60, meta.mode());
    put_u32(&mut out, 64, meta.nlink() as u32);
    put_u32(&mut out, 68, meta.uid());
    put_u32(&mut out, 72, meta.gid());
    put_u32(&mut out, 80, 4096);
    out
}

fn dev_ino(node: &Node) -> Result<(u64, u64), i32> {
    let meta = node.meta()?;
    Ok((meta.dev(), meta.ino()))
}

fn component(name: &[u8]) -> Result<CString, i32> {
    if name.is_empty()
        || name == b"."
        || name == b".."
        || name.contains(&b'/')
        || name.len() > MAX_NAME
    {
        return Err(EINVAL);
    }
    CString::new(name).map_err(|_| EINVAL)
}

fn c_name(body: &[u8]) -> Result<&[u8], i32> {
    let end = body.iter().position(|b| *b == 0).ok_or(EINVAL)?;
    let name = &body[..end];
    if name.is_empty() || name.contains(&b'/') || name.len() > MAX_NAME {
        return Err(EINVAL);
    }
    Ok(name)
}

fn ok(unique: u64, body: &[u8]) -> Vec<u8> {
    reply(unique, 0, body)
}

fn err(unique: u64, errno: i32) -> Vec<u8> {
    reply(unique, -errno, &[])
}

fn reply(unique: u64, error: i32, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(OUT_HEADER + body.len());
    put_u32_vec(&mut out, (OUT_HEADER + body.len()) as u32);
    put_u32_vec(&mut out, error as u32);
    out.extend_from_slice(&unique.to_le_bytes());
    out.extend_from_slice(body);
    out
}

fn put_u64(buf: &mut [u8], at: usize, value: u64) {
    buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32_vec(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_le_bytes());
}

fn errno(error: io::Error) -> i32 {
    match error.raw_os_error().unwrap_or(libc::EIO) {
        libc::ENOENT => 2,
        libc::EIO => 5,
        libc::EBADF => 9,
        libc::EACCES => 13,
        libc::ENOTDIR => 20,
        libc::EISDIR => 21,
        libc::EINVAL => 22,
        libc::EMFILE => 24,
        libc::ENOSPC => 28,
        libc::EROFS => 30,
        libc::ENAMETOOLONG => 36,
        libc::ELOOP => 40,
        libc::ENOTSUP => 95,
        libc::ESTALE => 116,
        other => other,
    }
}

fn cvt(n: i32) -> Result<i32, i32> {
    if n < 0 {
        Err(errno(io::Error::last_os_error()))
    } else {
        Ok(n)
    }
}

fn __error() -> *mut i32 {
    unsafe { libc::__error() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(opcode: u32, nodeid: u64, unique: u64, body: &[u8]) -> Vec<u8> {
        let len = (HEADER + body.len()) as u32;
        let mut out = vec![0u8; HEADER + body.len()];
        put_u32(&mut out, 0, len);
        put_u32(&mut out, 4, opcode);
        put_u64(&mut out, 8, unique);
        put_u64(&mut out, 16, nodeid);
        out[HEADER..].copy_from_slice(body);
        out
    }

    fn call(server: &mut Server, opcode: u32, nodeid: u64, body: &[u8]) -> (i32, Vec<u8>) {
        let reply = server
            .request(&message(opcode, nodeid, 7, body))
            .unwrap()
            .expect("reply");
        let error = i32::from_le_bytes(reply[4..8].try_into().unwrap());
        (error, reply[OUT_HEADER..].to_vec())
    }

    fn init(server: &mut Server) {
        let mut body = vec![0u8; 16];
        put_u32(&mut body, 0, 7);
        put_u32(&mut body, 4, 41);
        put_u32(&mut body, 8, 128 * 1024);
        put_u32(&mut body, 12, u32::MAX);
        let (error, out) = call(server, INIT, 0, &body);
        assert_eq!(error, 0);
        assert_eq!(u32::from_le_bytes(out[0..4].try_into().unwrap()), 7);
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()), 31);
        let flags = u32::from_le_bytes(out[12..16].try_into().unwrap());
        assert_eq!(flags & (1 << 20), 0, "POSIX_ACL must stay off");
        assert_eq!(u32::from_le_bytes(out[32..36].try_into().unwrap()), 0);
        assert_ne!(flags & FUSE_MAX_PAGES, 0);
    }

    #[test]
    fn read_only_lookup_read_readdir_and_forget() {
        let dir = crate::test_support::TestDir::new();
        dir.file("hello.txt", b"hello");
        std::fs::create_dir(dir.path("sub")).unwrap();
        let mut server = Server::new(dir.root(), true).unwrap();
        init(&mut server);
        let mut name = b"hello.txt".to_vec();
        name.push(0);
        let (error, entry) = call(&mut server, LOOKUP, 1, &name);
        assert_eq!(error, 0);
        let id = u64::from_le_bytes(entry[0..8].try_into().unwrap());
        assert_ne!(id, 1);
        assert_eq!(u64::from_le_bytes(entry[48..56].try_into().unwrap()), 5);
        let (error, _) = call(&mut server, OPEN, id, &[1, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(error, -EROFS);
        let (error, opened) = call(&mut server, OPEN, id, &[0; 8]);
        assert_eq!(error, 0);
        let fh = u64::from_le_bytes(opened[0..8].try_into().unwrap());
        let mut read = vec![0u8; 24];
        put_u64(&mut read, 0, fh);
        put_u32(&mut read, 16, 64);
        let (error, data) = call(&mut server, READ, id, &read);
        assert_eq!(error, 0);
        assert_eq!(data, b"hello");
        let (error, listing) = {
            let (error, opened) = call(&mut server, OPENDIR, 1, &[0; 8]);
            assert_eq!(error, 0);
            let fh = u64::from_le_bytes(opened[0..8].try_into().unwrap());
            let mut body = vec![0u8; 24];
            put_u64(&mut body, 0, fh);
            put_u32(&mut body, 16, 4096);
            call(&mut server, READDIR, 1, &body)
        };
        assert_eq!(error, 0);
        assert!(listing.windows(9).any(|w| w == b"hello.txt"));
        assert!(listing.windows(3).any(|w| w == b"sub"));
        let (error, _) = call(&mut server, MKDIR, 1, &[0; 8]);
        assert_eq!(error, -EROFS);
        let (error, _) = call(&mut server, GETXATTR, id, &[0; 8]);
        assert_eq!(error, -ENOSYS);
        assert!(server.request(&[0, 0, 0, 0]).is_err());
        server.forget(id, 1);
        let (error, _) = call(&mut server, GETATTR, id, &[0; 16]);
        assert_eq!(error, -ESTALE);
        let (error, dotdot) = call(&mut server, LOOKUP, 1, b"..\0");
        assert_eq!(error, 0);
        assert_eq!(u64::from_le_bytes(dotdot[0..8].try_into().unwrap()), 1);
        assert_eq!(call(&mut server, LOOKUP, 1, b"../hello.txt\0").0, -EINVAL);
        assert_eq!(call(&mut server, FSYNC, 1, &[0; 8]).0, 0);
        let (error, statfs) = call(&mut server, STATFS, 1, &[]);
        assert_eq!(error, 0);
        assert_eq!(statfs.len(), 80);
        assert_eq!(call(&mut server, UNLINK, 1, b"hello.txt\0").0, -EROFS);
        assert!(
            server
                .request(&message(INTERRUPT, 0, 9, &1u64.to_le_bytes()))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn symlink_is_not_followed() {
        let dir = crate::test_support::TestDir::new();
        std::os::unix::fs::symlink("/etc/passwd", dir.path("link")).unwrap();
        let mut server = Server::new(dir.root(), true).unwrap();
        init(&mut server);
        let (error, entry) = call(&mut server, LOOKUP, 1, b"link\0");
        assert_eq!(error, 0);
        let id = u64::from_le_bytes(entry[0..8].try_into().unwrap());
        let mode = u32::from_le_bytes(entry[40 + 60..40 + 64].try_into().unwrap());
        assert_eq!(mode & 0o170000, 0o120000);
        let (error, text) = call(&mut server, READLINK, id, &[]);
        assert_eq!(error, 0);
        assert_eq!(text, b"/etc/passwd");
        let (error, _) = call(&mut server, OPEN, id, &[0; 8]);
        assert_eq!(error, -ELOOP);
    }

    #[test]
    fn writable_export_is_refused() {
        let dir = crate::test_support::TestDir::new();
        assert!(Server::new(dir.root(), false).is_err());
    }
}
