//! 9P2000.L plus FreeBSD 14.4's legacy Tcreate/open-flag conventions.
//! Paths are single components relative to retained directory descriptors.
//! Symlink targets are returned to the guest and never followed by host I/O.
//! Exported directories are capabilities: an already-walked host directory
//! remains accessible if the host moves it, until the session is reset.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
use std::rc::Rc;

pub const MAX_MESSAGE: usize = 256 * 1024;
const TVERSION: u8 = 100;
const TATTACH: u8 = 104;
const TFLUSH: u8 = 108;
const TWALK: u8 = 110;
const TLOPEN: u8 = 12;
const TOPEN: u8 = 112;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;
const TGETATTR: u8 = 24;
const TSTATFS: u8 = 8;
const TREADDIR: u8 = 40;
const TLCREATE: u8 = 14;
const TCREATE: u8 = 114;
const TMKDIR: u8 = 72;
const TSETATTR: u8 = 26;
const TRENAMEAT: u8 = 74;
const TLINK: u8 = 70;
const TUNLINKAT: u8 = 76;
const TREMOVE: u8 = 122;
const TSYMLINK: u8 = 16;
const TREADLINK: u8 = 22;
const TFSYNC: u8 = 50;
const MAX_FIDS: usize = 4096;
type Result<T> = std::result::Result<T, u32>;
const EINVAL: u32 = 22;
const ENOTSUP: u32 = 95;
fn errno(e: io::Error) -> u32 {
    match e.raw_os_error().unwrap_or(libc::EIO) {
        libc::EPERM => 1,
        libc::ENOENT => 2,
        libc::EINTR => 4,
        libc::EIO => 5,
        libc::EBADF => 9,
        libc::EAGAIN => 11,
        libc::ENOMEM => 12,
        libc::EACCES => 13,
        libc::EBUSY => 16,
        libc::EEXIST => 17,
        libc::EXDEV => 18,
        libc::ENODEV => 19,
        libc::ENOTDIR => 20,
        libc::EISDIR => 21,
        libc::EINVAL => 22,
        libc::ENFILE => 23,
        libc::EMFILE => 24,
        libc::EFBIG => 27,
        libc::ENOSPC => 28,
        libc::EROFS => 30,
        libc::EMLINK => 31,
        libc::EPIPE => 32,
        libc::ENAMETOOLONG => 36,
        libc::ENOTEMPTY => 39,
        libc::ELOOP => 40,
        libc::ENOTSUP => 95,
        libc::ESTALE => 116,
        _ => 5,
    }
}
fn cvt(n: i32) -> Result<i32> {
    if n < 0 {
        Err(errno(io::Error::last_os_error()))
    } else {
        Ok(n)
    }
}
fn name(s: &[u8]) -> Result<CString> {
    if s.is_empty() || s == b"." || s == b".." || s.contains(&b'/') {
        return Err(EINVAL);
    }
    CString::new(s).map_err(|_| EINVAL)
}
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&v| v <= self.data.len())
            .ok_or(EINVAL)?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<Vec<u8>> {
        let n = self.u16()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn end(&self) -> Result<()> {
        if self.pos == self.data.len() {
            Ok(())
        } else {
            Err(EINVAL)
        }
    }
}
fn u16v(b: &mut Vec<u8>, n: u16) {
    b.extend(n.to_le_bytes());
}
fn u32v(b: &mut Vec<u8>, n: u32) {
    b.extend(n.to_le_bytes());
}
fn u64v(b: &mut Vec<u8>, n: u64) {
    b.extend(n.to_le_bytes());
}
fn string(b: &mut Vec<u8>, s: &[u8]) {
    u16v(b, s.len() as u16);
    b.extend(s);
}

struct Node {
    file: File,
    location: RefCell<(Option<Rc<Node>>, Vec<u8>)>,
}
impl Node {
    fn depth(&self) -> Result<usize> {
        let mut depth = 0;
        let mut parent = self.location.borrow().0.clone();
        while let Some(node) = parent {
            depth += 1;
            if depth > 256 {
                return Err(36);
            }
            parent = node.location.borrow().0.clone();
        }
        Ok(depth)
    }
    fn meta(&self) -> Result<Metadata> {
        self.file.metadata().map_err(errno)
    }
    fn child(self: &Rc<Self>, s: &[u8]) -> Result<Rc<Self>> {
        if s == b"." {
            return Ok(self.clone());
        }
        if s == b".." {
            return Ok(self
                .location
                .borrow()
                .0
                .clone()
                .unwrap_or_else(|| self.clone()));
        }
        if !self.meta()?.is_dir() {
            return Err(20);
        }
        if self.depth()? >= 256 {
            return Err(36);
        }
        let c = name(s)?;
        // O_SYMLINK opens the link itself on Darwin, never its host target.
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
            location: RefCell::new((Some(self.clone()), s.to_vec())),
        });
        let m = node.meta()?;
        if !m.is_file() && !m.is_dir() && !m.file_type().is_symlink() {
            return Err(ENOTSUP);
        }
        Ok(node)
    }
    fn reopen(&self, write: bool) -> Result<File> {
        let m = self.meta()?;
        if m.file_type().is_symlink() {
            return Err(40);
        }
        if !write {
            return self.file.try_clone().map_err(errno);
        }
        if !m.is_file() {
            return Err(21);
        }
        let (parent, n) = self.location.borrow().clone();
        let parent = parent.ok_or(13u32)?;
        let c = name(&n)?;
        let fd = cvt(unsafe {
            libc::openat(
                parent.file.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDWR | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })?;
        let file = unsafe { File::from_raw_fd(fd) };
        let current = file.metadata().map_err(errno)?;
        if (m.dev(), m.ino()) != (current.dev(), current.ino()) {
            return Err(116);
        }
        Ok(file)
    }
}
struct Fid {
    node: Rc<Node>,
    open: Option<File>,
    writable: bool,
    readable: bool,
    append: bool,
}
impl Fid {
    fn new(node: Rc<Node>) -> Self {
        Self {
            node,
            open: None,
            writable: false,
            readable: false,
            append: false,
        }
    }
}

struct Qid {
    id: u64,
    version: u32,
    metadata: (u64, i64, i64, i64, i64),
}

pub struct Server {
    root: Rc<Node>,
    ro: bool,
    msize: usize,
    negotiated: bool,
    fids: HashMap<u32, Fid>,
    qids: HashMap<(u64, u64), Qid>,
    next_qid: u64,
}
impl Server {
    pub fn new(path: &Path, ro: bool) -> io::Result<Self> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            root: Rc::new(Node {
                file: unsafe { File::from_raw_fd(fd) },
                location: RefCell::new((None, vec![])),
            }),
            ro,
            msize: MAX_MESSAGE,
            negotiated: false,
            fids: HashMap::new(),
            qids: HashMap::new(),
            next_qid: 1,
        })
    }
    pub fn reset(&mut self) {
        self.fids.clear();
        self.qids.clear();
        self.next_qid = 1;
        self.negotiated = false;
        self.msize = MAX_MESSAGE;
    }
    fn fid(&self, id: u32) -> Result<&Fid> {
        self.fids.get(&id).ok_or(9)
    }
    fn node(&self, id: u32) -> Result<Rc<Node>> {
        Ok(self.fid(id)?.node.clone())
    }
    fn writable(&self) -> Result<()> {
        if self.ro { Err(30) } else { Ok(()) }
    }
    fn vacant(&self, id: u32) -> Result<()> {
        if id == u32::MAX || self.fids.contains_key(&id) {
            Err(EINVAL)
        } else if self.fids.len() >= MAX_FIDS {
            Err(24)
        } else {
            Ok(())
        }
    }
    fn qid(&mut self, b: &mut Vec<u8>, m: &Metadata) -> Result<()> {
        let key = (m.dev(), m.ino());
        if !self.qids.contains_key(&key) {
            if self.qids.len() >= 65536 {
                return Err(28);
            }
            self.qids.insert(
                key,
                Qid {
                    id: self.next_qid,
                    version: 1,
                    metadata: (
                        m.size(),
                        m.mtime(),
                        m.mtime_nsec(),
                        m.ctime(),
                        m.ctime_nsec(),
                    ),
                },
            );
            self.next_qid += 1;
        }
        b.push(if m.is_dir() {
            0x80
        } else if m.file_type().is_symlink() {
            2
        } else {
            0
        });
        let q = self.qids.get_mut(&key).unwrap();
        let metadata = (
            m.size(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        );
        if metadata != q.metadata {
            q.version = q.version.wrapping_add(1).max(1);
            q.metadata = metadata;
        }
        u32v(b, q.version);
        u64v(b, q.id);
        Ok(())
    }
    fn check_relocation(&self, key: (u64, u64), parent: &Node) -> Result<()> {
        let depth = parent.depth()? + 1;
        if depth > 256 {
            return Err(36);
        }
        for fid in self.fids.values() {
            let mut next = Some(fid.node.clone());
            let mut distance = 0;
            while let Some(node) = next {
                let m = node.meta()?;
                if (m.dev(), m.ino()) == key {
                    if depth + distance > 256 {
                        return Err(36);
                    }
                    break;
                }
                distance += 1;
                if distance > 256 {
                    return Err(36);
                }
                next = node.location.borrow().0.clone();
            }
        }
        Ok(())
    }
    fn relocate(&mut self, key: (u64, u64), parent: Rc<Node>, name: &[u8]) {
        let mut seen = std::collections::HashSet::new();
        for fid in self.fids.values() {
            let mut next = Some(fid.node.clone());
            while let Some(node) = next {
                if !seen.insert(Rc::as_ptr(&node) as usize) {
                    break;
                }
                next = node.location.borrow().0.clone();
                if node.meta().is_ok_and(|m| (m.dev(), m.ino()) == key) {
                    *node.location.borrow_mut() = (Some(parent.clone()), name.to_vec());
                }
            }
        }
    }
    pub fn request(&mut self, bytes: &[u8]) -> Vec<u8> {
        let tag = bytes
            .get(5..7)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0xffff);
        let kind = bytes.get(4).copied().unwrap_or(0);
        let result = if bytes.len() < 7
            || bytes.len() > self.msize
            || u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize != bytes.len()
        {
            Err(EINVAL)
        } else {
            self.dispatch(
                kind,
                &mut Reader {
                    data: &bytes[7..],
                    pos: 0,
                },
            )
        };
        let (ty, body) = match result {
            Ok(b) if b.len() + 7 <= self.msize => (kind.wrapping_add(1), b),
            Ok(_) => (7, EINVAL.to_le_bytes().to_vec()),
            Err(e) => (7, e.to_le_bytes().to_vec()),
        };
        let mut out = Vec::with_capacity(body.len() + 7);
        u32v(&mut out, (body.len() + 7) as u32);
        out.push(ty);
        u16v(&mut out, tag);
        out.extend(body);
        out
    }
    fn dispatch(&mut self, ty: u8, r: &mut Reader<'_>) -> Result<Vec<u8>> {
        let mut b = Vec::new();
        if ty != TVERSION && !self.negotiated {
            return Err(EINVAL);
        }
        match ty {
            TVERSION => {
                let msize = r.u32()? as usize;
                let version = r.string()?;
                r.end()?;
                if msize < 256 {
                    return Err(EINVAL);
                }
                self.reset();
                self.msize = msize.min(MAX_MESSAGE);
                self.negotiated = version == b"9P2000.L";
                u32v(&mut b, self.msize as u32);
                string(
                    &mut b,
                    if self.negotiated {
                        b"9P2000.L"
                    } else {
                        b"unknown"
                    },
                );
            }
            TATTACH => {
                let id = r.u32()?;
                let afid = r.u32()?;
                let _user = r.string()?;
                let _aname = r.string()?;
                let _uid = r.u32()?;
                r.end()?;
                self.vacant(id)?;
                if afid != u32::MAX {
                    return Err(ENOTSUP);
                }
                let root = self.root.clone();
                self.qid(&mut b, &root.meta()?)?;
                self.fids.insert(id, Fid::new(root));
            }
            TFLUSH => {
                let _oldtag = r.u16()?;
                r.end()?;
            } // Serialized service: preceding request has completed.
            TWALK => {
                let id = r.u32()?;
                let new = r.u32()?;
                let count = r.u16()?;
                if count > 16 {
                    return Err(EINVAL);
                }
                let mut names = Vec::new();
                for _ in 0..count {
                    names.push(r.string()?);
                }
                r.end()?;
                if new != id {
                    self.vacant(new)?;
                }
                if self.fid(id)?.open.is_some() {
                    return Err(EINVAL);
                }
                let mut node = self.node(id)?;
                let mut q = Vec::new();
                let mut walked = 0;
                for n in names {
                    match node.child(&n) {
                        Ok(next) => {
                            node = next;
                            self.qid(&mut q, &node.meta()?)?;
                            walked += 1;
                        }
                        Err(e) if walked == 0 => return Err(e),
                        Err(_) => break,
                    }
                }
                u16v(&mut b, walked);
                b.extend(q);
                if walked == count {
                    self.fids.insert(new, Fid::new(node));
                }
            }
            TLOPEN | TOPEN => {
                let id = r.u32()?;
                let flags = if ty == TLOPEN {
                    r.u32()?
                } else {
                    legacy_flags(r.u8()?)?
                };
                r.end()?;
                let (read, write, trunc, append) = open_flags(flags)?;
                if write || trunc {
                    self.writable()?;
                }
                if append && !write {
                    return Err(EINVAL);
                }
                if self.fid(id)?.open.is_some() {
                    return Err(EINVAL);
                }
                let node = self.node(id)?;
                let file = node.reopen(write)?;
                if trunc {
                    file.set_len(0).map_err(errno)?;
                }
                if append {
                    cvt(unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, libc::O_APPEND) })?;
                }
                self.qid(&mut b, &node.meta()?)?;
                u32v(&mut b, (self.msize - 24) as u32);
                let f = self.fids.get_mut(&id).unwrap();
                f.open = Some(file);
                f.readable = read;
                f.writable = write;
                f.append = append;
            }
            TREAD => {
                let id = r.u32()?;
                let off = r.u64()?;
                let count = r.u32()? as usize;
                r.end()?;
                let f = self.fid(id)?;
                if !f.readable {
                    return Err(9);
                }
                let file = f.open.as_ref().ok_or(9u32)?;
                if !file.metadata().map_err(errno)?.is_file() {
                    return Err(21);
                }
                if off > i64::MAX as u64 {
                    return Err(EINVAL);
                }
                let mut data = vec![0; count.min(self.msize - 11)];
                let n = file.read_at(&mut data, off).map_err(errno)?;
                u32v(&mut b, n as u32);
                b.extend(&data[..n]);
            }
            TWRITE => {
                let id = r.u32()?;
                let off = r.u64()?;
                let count = r.u32()? as usize;
                let data = r.take(count)?;
                r.end()?;
                self.writable()?;
                let f = self.fid(id)?;
                if !f.writable {
                    return Err(9);
                }
                if off
                    .checked_add(count as u64)
                    .is_none_or(|n| n > i64::MAX as u64)
                {
                    return Err(EINVAL);
                }
                let file = f.open.as_ref().ok_or(9u32)?;
                let n = if f.append {
                    let n =
                        unsafe { libc::write(file.as_raw_fd(), data.as_ptr().cast(), data.len()) };
                    if n < 0 {
                        return Err(errno(io::Error::last_os_error()));
                    }
                    n as usize
                } else {
                    file.write_at(data, off).map_err(errno)?
                };
                // FreeBSD 14.4 p9fs does not issue Tfsync. Persist data before
                // acknowledging writes; directory metadata has normal host FS semantics.
                file.sync_all().map_err(errno)?;
                u32v(&mut b, n as u32);
            }
            TCLUNK => {
                let id = r.u32()?;
                r.end()?;
                self.fids.remove(&id).ok_or(9u32)?;
            }
            TGETATTR => {
                let id = r.u32()?;
                let _mask = r.u64()?;
                r.end()?;
                let m = self.node(id)?.meta()?;
                u64v(&mut b, 0x7ff);
                self.qid(&mut b, &m)?;
                u32v(&mut b, m.mode() & !0o6000);
                u32v(&mut b, 0);
                u32v(&mut b, 0);
                for v in [
                    m.nlink(),
                    0,
                    m.size(),
                    m.blksize(),
                    m.blocks(),
                    m.atime() as u64,
                    m.atime_nsec() as u64,
                    m.mtime() as u64,
                    m.mtime_nsec() as u64,
                    m.ctime() as u64,
                    m.ctime_nsec() as u64,
                    0,
                    0,
                    0,
                    0,
                ] {
                    u64v(&mut b, v);
                }
            }
            TSTATFS => {
                let id = r.u32()?;
                r.end()?;
                let node = self.node(id)?;
                let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
                cvt(unsafe { libc::fstatvfs(node.file.as_raw_fd(), &mut st) })?;
                u32v(&mut b, 0x01021997);
                u32v(&mut b, st.f_frsize as u32);
                for v in [
                    st.f_blocks as u64,
                    st.f_bfree as u64,
                    st.f_bavail as u64,
                    st.f_files as u64,
                    st.f_ffree as u64,
                    0,
                ] {
                    u64v(&mut b, v);
                }
                u32v(&mut b, st.f_namemax as u32);
            }
            TREADDIR => {
                let id = r.u32()?;
                let off = r.u64()?;
                let count = r.u32()? as usize;
                r.end()?;
                let f = self.fid(id)?;
                if f.open.is_none() || !f.readable {
                    return Err(9);
                }
                let node = f.node.clone();
                if !node.meta()?.is_dir() {
                    return Err(20);
                }
                let dot = c".";
                let fd = cvt(unsafe {
                    libc::openat(
                        node.file.as_raw_fd(),
                        dot.as_ptr(),
                        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                    )
                })?;
                let dir = unsafe { libc::fdopendir(fd) };
                if dir.is_null() {
                    unsafe {
                        libc::close(fd);
                    }
                    return Err(errno(io::Error::last_os_error()));
                }
                struct Dir(*mut libc::DIR);
                impl Drop for Dir {
                    fn drop(&mut self) {
                        unsafe {
                            libc::closedir(self.0);
                        }
                    }
                }
                let _guard = Dir(dir);
                let mut data = Vec::new();
                let mut index = 0u64;
                // Cookies are enumeration offsets, valid while the directory is unchanged.
                loop {
                    let ent = unsafe { libc::readdir(dir) };
                    if ent.is_null() {
                        break;
                    }
                    index += 1;
                    if index <= off {
                        continue;
                    }
                    let n = unsafe { std::ffi::CStr::from_ptr((*ent).d_name.as_ptr()).to_bytes() };
                    if data.len() + 24 + n.len() > count.min(self.msize - 11) {
                        break;
                    }
                    let child = match node.child(n) {
                        Ok(v) => v,
                        Err(2) => continue,
                        Err(e) => return Err(e),
                    };
                    let m = child.meta()?;
                    self.qid(&mut data, &m)?;
                    u64v(&mut data, index);
                    data.push(if m.is_dir() {
                        4
                    } else if m.file_type().is_symlink() {
                        10
                    } else {
                        8
                    });
                    string(&mut data, n);
                }
                u32v(&mut b, data.len() as u32);
                b.extend(data);
            }
            TLCREATE | TCREATE | TMKDIR => {
                let id = r.u32()?;
                let n = r.string()?;
                let (flags, mode, directory) = if ty == TCREATE {
                    let perm = r.u32()?;
                    let flags = legacy_flags(r.u8()?)?;
                    let _extension = r.string()?;
                    if perm & !(0x80000000 | 0x10000 | 0o777) != 0 {
                        return Err(ENOTSUP);
                    }
                    (
                        flags,
                        (perm & 0o777) | ((perm & 0x10000) >> 6),
                        perm & 0x80000000 != 0,
                    )
                } else if ty == TMKDIR {
                    let mode = r.u32()?;
                    let _gid = r.u32()?;
                    (0, mode, true)
                } else {
                    let flags = r.u32()?;
                    let mode = r.u32()?;
                    let _gid = r.u32()?;
                    (flags, mode, false)
                };
                r.end()?;
                self.writable()?;
                let c = name(&n)?;
                let parent = self.node(id)?;
                if parent.depth()? >= 256 {
                    return Err(36);
                }
                if self.fid(id)?.open.is_some() || !parent.meta()?.is_dir() {
                    return Err(EINVAL);
                }
                let (read, write, _, append) = open_flags(if ty == TLCREATE {
                    flags & !(0x40 | 0x80)
                } else {
                    flags
                })?;
                if append && !write {
                    return Err(EINVAL);
                }
                let mut created = None;
                if directory {
                    cvt(unsafe {
                        libc::mkdirat(
                            parent.file.as_raw_fd(),
                            c.as_ptr(),
                            (mode & 0o1777) as libc::mode_t,
                        )
                    })?;
                } else {
                    let fd = cvt(unsafe {
                        libc::openat(
                            parent.file.as_raw_fd(),
                            c.as_ptr(),
                            libc::O_CREAT
                                | libc::O_EXCL
                                | libc::O_RDWR
                                | libc::O_NOFOLLOW
                                | libc::O_CLOEXEC,
                            (mode & 0o1777) as libc::c_uint,
                        )
                    })?;
                    created = Some(unsafe { File::from_raw_fd(fd) });
                }
                let node = if let Some(file) = created {
                    Rc::new(Node {
                        file,
                        location: RefCell::new((Some(parent.clone()), n)),
                    })
                } else {
                    parent.child(&n)?
                };
                self.qid(&mut b, &node.meta()?)?;
                if ty != TMKDIR {
                    u32v(&mut b, (self.msize - 24) as u32);
                    let file = node.file.try_clone().map_err(errno)?;
                    if append {
                        cvt(unsafe {
                            libc::fcntl(file.as_raw_fd(), libc::F_SETFL, libc::O_APPEND)
                        })?;
                    }
                    self.fids.insert(
                        id,
                        Fid {
                            node,
                            open: Some(file),
                            writable: write && !directory,
                            readable: read,
                            append,
                        },
                    );
                }
            }
            TSETATTR => {
                let id = r.u32()?;
                let valid = r.u32()?;
                let mode = r.u32()?;
                let uid = r.u32()?;
                let gid = r.u32()?;
                let size = r.u64()?;
                let at = r.u64()?;
                let an = r.u64()?;
                let mt = r.u64()?;
                let mn = r.u64()?;
                r.end()?;
                self.writable()?;
                if valid & !0x1ff != 0
                    || (valid & 2 != 0 && uid != 0)
                    || (valid & 4 != 0 && gid != 0)
                {
                    return Err(ENOTSUP);
                }
                if size > i64::MAX as u64 || an >= 1_000_000_000 || mn >= 1_000_000_000 {
                    return Err(EINVAL);
                }
                let node = self.node(id)?;
                if node.meta()?.file_type().is_symlink() {
                    return Err(ENOTSUP);
                }
                let file = node.reopen(valid & 8 != 0)?;
                if valid & 8 != 0 {
                    file.set_len(size).map_err(errno)?;
                }
                if valid & 1 != 0 {
                    cvt(unsafe {
                        libc::fchmod(file.as_raw_fd(), (mode & 0o1777) as libc::mode_t)
                    })?;
                }
                let ts = |flag, set, sec, ns| libc::timespec {
                    tv_sec: sec as libc::time_t,
                    tv_nsec: if valid & flag == 0 {
                        libc::UTIME_OMIT
                    } else if valid & set == 0 {
                        libc::UTIME_NOW
                    } else {
                        ns as _
                    },
                };
                if valid & 0x30 != 0 {
                    let times = [ts(16, 128, at, an), ts(32, 256, mt, mn)];
                    cvt(unsafe { libc::futimens(file.as_raw_fd(), times.as_ptr()) })?;
                }
            }
            TRENAMEAT => {
                let old = self.node(r.u32()?)?;
                let oldname = name(&r.string()?)?;
                let new = self.node(r.u32()?)?;
                let newname = name(&r.string()?)?;
                r.end()?;
                self.writable()?;
                let moved = old.child(oldname.as_bytes())?.meta()?;
                if moved.is_dir() {
                    self.check_relocation((moved.dev(), moved.ino()), &new)?;
                }
                cvt(unsafe {
                    libc::renameat(
                        old.file.as_raw_fd(),
                        oldname.as_ptr(),
                        new.file.as_raw_fd(),
                        newname.as_ptr(),
                    )
                })?;
                self.relocate((moved.dev(), moved.ino()), new, newname.as_bytes());
            }
            TLINK => {
                let directory = self.node(r.u32()?)?;
                let source = self.node(r.u32()?)?;
                let dest = name(&r.string()?)?;
                r.end()?;
                self.writable()?;
                if !source.meta()?.is_file() {
                    return Err(ENOTSUP);
                }
                let (parent, n) = source.location.borrow().clone();
                let parent = parent.ok_or(13u32)?;
                let old = name(&n)?;
                let current = parent.child(&n)?.meta()?;
                let original = source.meta()?;
                if (current.dev(), current.ino()) != (original.dev(), original.ino()) {
                    return Err(116);
                }
                cvt(unsafe {
                    libc::linkat(
                        parent.file.as_raw_fd(),
                        old.as_ptr(),
                        directory.file.as_raw_fd(),
                        dest.as_ptr(),
                        0,
                    )
                })?;
            }
            TUNLINKAT => {
                let node = self.node(r.u32()?)?;
                let n = name(&r.string()?)?;
                let flags = r.u32()?;
                r.end()?;
                self.writable()?;
                if flags & !0x200 != 0 {
                    return Err(EINVAL);
                }
                cvt(unsafe {
                    libc::unlinkat(
                        node.file.as_raw_fd(),
                        n.as_ptr(),
                        if flags == 0x200 {
                            libc::AT_REMOVEDIR
                        } else {
                            0
                        },
                    )
                })?;
            }
            TREMOVE => {
                let id = r.u32()?;
                r.end()?;
                self.writable()?;
                let f = self.fids.remove(&id).ok_or(9u32)?;
                let (parent, raw) = f.node.location.borrow().clone();
                let parent = parent.ok_or(13u32)?;
                let n = name(&raw)?;
                let current = parent.child(&raw)?;
                let a = current.meta()?;
                let m = f.node.meta()?;
                if (a.dev(), a.ino()) != (m.dev(), m.ino()) {
                    return Err(116);
                }
                cvt(unsafe {
                    libc::unlinkat(
                        parent.file.as_raw_fd(),
                        n.as_ptr(),
                        if m.is_dir() { libc::AT_REMOVEDIR } else { 0 },
                    )
                })?;
            }
            TSYMLINK => {
                let parent = self.node(r.u32()?)?;
                let n = r.string()?;
                let target = r.string()?;
                let _gid = r.u32()?;
                r.end()?;
                self.writable()?;
                let c = name(&n)?;
                let target = CString::new(target).map_err(|_| EINVAL)?;
                cvt(unsafe {
                    libc::symlinkat(target.as_ptr(), parent.file.as_raw_fd(), c.as_ptr())
                })?;
                self.qid(&mut b, &parent.child(&n)?.meta()?)?;
            }
            TREADLINK => {
                let node = self.node(r.u32()?)?;
                r.end()?;
                if !node.meta()?.file_type().is_symlink() {
                    return Err(EINVAL);
                }
                let mut data = vec![0u8; 4096];
                let (parent, raw) = node.location.borrow().clone();
                let parent = parent.ok_or(EINVAL)?;
                let c = name(&raw)?;
                let n = unsafe {
                    libc::readlinkat(
                        parent.file.as_raw_fd(),
                        c.as_ptr(),
                        data.as_mut_ptr().cast(),
                        data.len(),
                    )
                };
                if n < 0 {
                    return Err(errno(io::Error::last_os_error()));
                }
                if n as usize == data.len() {
                    return Err(36);
                }
                string(&mut b, &data[..n as usize]);
            }
            TFSYNC => {
                let id = r.u32()?;
                let _datasync = r.u32()?;
                r.end()?;
                self.fid(id)?
                    .open
                    .as_ref()
                    .ok_or(9u32)?
                    .sync_all()
                    .map_err(errno)?;
            }
            _ => return Err(ENOTSUP),
        }
        Ok(b)
    }
}
fn legacy_flags(mode: u8) -> Result<u32> {
    if mode & !(3 | 0x10 | 0x80) != 0 {
        return Err(ENOTSUP);
    }
    Ok((if mode & 3 == 3 { 0 } else { mode as u32 & 3 })
        | if mode & 0x10 != 0 { 0x200 } else { 0 }
        | if mode & 0x80 != 0 { 0x400 } else { 0 })
}
fn open_flags(flags: u32) -> Result<(bool, bool, bool, bool)> {
    // Ignore client-local O_LARGEFILE/O_CLOEXEC; reject flags we cannot honor.
    if flags & !(3 | 0x80 | 0x200 | 0x400 | 0x8000 | 0x10000 | 0x20000 | 0x80000) != 0
        || flags & 3 == 3
    {
        return Err(ENOTSUP);
    }
    let write = flags & 3 != 0;
    let trunc = flags & 0x200 != 0;
    if trunc && !write {
        return Err(EINVAL);
    }
    Ok((flags & 3 != 1, write, trunc, flags & (0x80 | 0x400) != 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        dir: std::path::PathBuf,
        server: Server,
    }
    impl Fixture {
        fn new(ro: bool) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "vmm-p9-unit-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("file"), b"hello").unwrap();
            let mut s = Self {
                server: Server::new(&dir, ro).unwrap(),
                dir,
            };
            let mut b = Vec::new();
            u32v(&mut b, MAX_MESSAGE as u32);
            string(&mut b, b"9P2000.L");
            s.ok(100, b);
            let mut b = Vec::new();
            u32v(&mut b, 1);
            u32v(&mut b, u32::MAX);
            string(&mut b, b"root");
            string(&mut b, b"");
            u32v(&mut b, 0);
            s.ok(104, b);
            s
        }
        fn call(&mut self, ty: u8, b: Vec<u8>) -> Vec<u8> {
            let mut p = Vec::new();
            u32v(&mut p, (b.len() + 7) as u32);
            p.push(ty);
            u16v(&mut p, 42);
            p.extend(b);
            self.server.request(&p)
        }
        fn ok(&mut self, ty: u8, b: Vec<u8>) -> Vec<u8> {
            let r = self.call(ty, b);
            assert_eq!(r[4], ty + 1, "request {ty}, response {r:?}");
            r
        }
        fn walk(&mut self, path: &[u8], fid: u32) -> Vec<u8> {
            let mut b = Vec::new();
            u32v(&mut b, 1);
            u32v(&mut b, fid);
            u16v(&mut b, 1);
            string(&mut b, path);
            self.call(110, b)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn read_and_write_and_readonly_enforcement() {
        for ro in [true, false] {
            let mut f = Fixture::new(ro);
            assert_eq!(f.walk(b"file", 2)[4], 111);
            let mut b = Vec::new();
            u32v(&mut b, 2);
            u32v(&mut b, 2);
            let r = f.call(12, b);
            if ro {
                assert_eq!(&r[7..], &30u32.to_le_bytes());
                continue;
            }
            assert_eq!(r[4], 13);
            let mut b = Vec::new();
            u32v(&mut b, 2);
            u64v(&mut b, 1);
            u32v(&mut b, 2);
            b.extend(b"XX");
            f.ok(118, b);
            assert_eq!(std::fs::read(f.dir.join("file")).unwrap(), b"hXXlo");
            let mut b = Vec::new();
            u32v(&mut b, 2);
            u64v(&mut b, 0);
            u32v(&mut b, 50);
            assert_eq!(&f.ok(116, b)[11..], b"hXXlo");
        }
    }
    #[test]
    fn malformed_and_traversal_never_escape() {
        let mut f = Fixture::new(false);
        for p in [b"../escape".as_slice(), b"/etc/passwd", b"file\0suffix"] {
            assert_eq!(f.walk(p, 2)[4], 7);
        }
        for n in 0..7 {
            assert_eq!(f.server.request(&vec![0; n])[4], 7);
        }
        std::os::unix::fs::symlink("/etc/passwd", f.dir.join("link")).unwrap();
        assert_eq!(f.walk(b"link", 2)[4], 111);
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u32v(&mut b, 0);
        assert_eq!(f.call(12, b)[4], 7);
        let mut b = Vec::new();
        u32v(&mut b, 2);
        let r = f.ok(22, b);
        assert_eq!(&r[9..], b"/etc/passwd");
    }
    #[test]
    fn create_freebsd_extension_and_reset() {
        let mut f = Fixture::new(false);
        let mut b = Vec::new();
        u32v(&mut b, 1);
        string(&mut b, b"created");
        u32v(&mut b, 0o644);
        b.push(2);
        string(&mut b, b"");
        f.ok(114, b);
        assert!(f.dir.join("created").is_file());
        f.server.reset();
        let mut b = Vec::new();
        u32v(&mut b, 1);
        assert_eq!(f.call(120, b)[4], 7);
    }
    #[test]
    fn replaced_path_cannot_redirect_an_existing_fid_write() {
        let mut f = Fixture::new(false);
        assert_eq!(f.walk(b"file", 2)[4], 111);
        std::fs::rename(f.dir.join("file"), f.dir.join("original")).unwrap();
        std::fs::write(f.dir.join("file"), b"replacement").unwrap();
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u32v(&mut b, 2);
        let r = f.call(12, b);
        assert_eq!(&r[7..], &116u32.to_le_bytes());
        assert_eq!(std::fs::read(f.dir.join("file")).unwrap(), b"replacement");
    }
    #[test]
    fn rename_updates_cached_fid_before_append_and_fsync() {
        let mut f = Fixture::new(false);
        assert_eq!(f.walk(b"file", 2)[4], 111);
        let mut b = Vec::new();
        u32v(&mut b, 1);
        string(&mut b, b"file");
        u32v(&mut b, 1);
        string(&mut b, b"renamed");
        f.ok(TRENAMEAT, b);
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u32v(&mut b, 0x81);
        f.ok(TLOPEN, b);
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u64v(&mut b, 0);
        u32v(&mut b, 1);
        b.push(b'!');
        f.ok(TWRITE, b);
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u32v(&mut b, 0);
        f.ok(TFSYNC, b);
        assert_eq!(std::fs::read(f.dir.join("renamed")).unwrap(), b"hello!");
    }
    #[test]
    fn readonly_rejects_create_remove_and_metadata_mutations() {
        let mut f = Fixture::new(true);
        assert_eq!(f.walk(b"file", 2)[4], 111);
        let mut create = Vec::new();
        u32v(&mut create, 1);
        string(&mut create, b"new");
        u32v(&mut create, 0o644);
        create.push(2);
        string(&mut create, b"");
        let mut setattr = Vec::new();
        u32v(&mut setattr, 2);
        u32v(&mut setattr, 8);
        setattr.resize(60, 0);
        for (ty, b) in [
            (TCREATE, create),
            (TREMOVE, 2u32.to_le_bytes().to_vec()),
            (TSETATTR, setattr),
        ] {
            let reply = f.call(ty, b);
            assert_eq!(&reply[7..], &30u32.to_le_bytes());
        }
        assert_eq!(std::fs::read(f.dir.join("file")).unwrap(), b"hello");
        assert!(!f.dir.join("new").exists());
    }
    #[test]
    fn rejected_open_does_not_truncate_and_metadata_version_changes() {
        let mut f = Fixture::new(false);
        assert_eq!(f.walk(b"file", 2)[4], 111);
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u32v(&mut b, 0x200 | 1 | 0x40000000);
        assert_eq!(f.call(TLOPEN, b)[4], 7);
        assert_eq!(std::fs::read(f.dir.join("file")).unwrap(), b"hello");
        let mut b = Vec::new();
        u32v(&mut b, 2);
        u64v(&mut b, u64::MAX);
        let before = f.ok(TGETATTR, b.clone());
        std::fs::write(f.dir.join("file"), b"changed contents").unwrap();
        let after = f.ok(TGETATTR, b);
        assert_ne!(&before[16..20], &after[16..20]);
        assert_eq!(&before[20..28], &after[20..28]);
    }
}
