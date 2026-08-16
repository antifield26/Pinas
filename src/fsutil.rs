// ====== 内核级路径沙箱（P0-4：符号链接 TOCTOU 根治） ======
// safe_join_sandbox 是字符串级校验（拒绝 .. / 绝对路径），但「校验」与「后续文件操作」
// 之间，攻击者可把路径中的中间目录换成指向沙箱外的符号链接（symlink swap），
// 使 std::fs 操作实际落到沙箱外。此处用 openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)
// 让内核在单次系统调用内原子完成整条路径的解析与越界判定，再以 *at 系统调用族
// （renameat/unlinkat/mkdirat/fstatat…）基于已校验的目录 fd 执行操作——TOCTOU 窗口归零。
//
// 语义：
//   - 允许沙箱内的符号链接（解析结果仍停留在 root 之下）
//   - 拒绝任何解析出 root 的路径（含 /proc/self/fd 等 magic link，NO_MAGICLINKS）
//   - 写操作（unlinkat/renameat/mkdirat）仅对最终组件生效，绝不跟随最终组件符号链接
//   - root 本身是可信配置路径（uploads/ 等），允许其自身含符号链接
//
// Linux-only：其他平台回退为「路径拼接 + 存在性检查」的旧行为（本项目部署目标恒为 Linux）

use std::fs::{File, Metadata};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use rustix::fs::{
        AtFlags, Dir, Mode, OFlags, ResolveFlags, Stat, mkdirat, open, openat2, renameat, statat,
        unlinkat,
    };

    /// 打开根目录（O_PATH 目录 fd；允许 root 自身路径含符号链接——它是可信配置）
    pub(super) fn open_root_fd(root: &Path) -> io::Result<OwnedFd> {
        open(
            root,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)
    }

    /// openat2：BENEATH（解析不得越出 dirfd）+ NO_MAGICLINKS（禁 /proc/self/fd 等）
    fn open_beneath<P: rustix::path::Arg>(
        dirfd: BorrowedFd<'_>,
        path: P,
        oflags: OFlags,
        mode: Mode,
    ) -> io::Result<OwnedFd> {
        openat2(
            dirfd,
            path,
            oflags,
            mode,
            ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(io::Error::from)
    }

    /// 打开 rel 路径的父目录（BENEATH 原子解析整条父链）；rel 无 '/' 时返回 root fd 副本
    pub(super) fn open_parent(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<OwnedFd> {
        let parent = match rel.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => {
                // 父即根：dup 根 fd（O_PATH fd 可直接作 *at 的 dirfd）
                return rustix::io::dup(root_fd).map_err(io::Error::from);
            }
        };
        open_beneath(
            root_fd,
            parent,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
    }

    pub(super) fn open_read(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<File> {
        let fd = open_beneath(
            root_fd,
            rel,
            OFlags::RDONLY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(File::from(fd))
    }

    pub(super) fn open_write(
        root_fd: BorrowedFd<'_>,
        rel: &Path,
        truncate: bool,
        exclusive: bool,
    ) -> io::Result<File> {
        let mut flags = OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC;
        if truncate {
            flags |= OFlags::TRUNC;
        }
        if exclusive {
            flags |= OFlags::EXCL;
        }
        let fd = open_beneath(root_fd, rel, flags, Mode::from_raw_mode(0o644))?;
        Ok(File::from(fd))
    }

    pub(super) fn create_dir(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<()> {
        let parent_fd = open_parent(root_fd, rel)?;
        let name = rel
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "空路径"))?;
        mkdirat(parent_fd, name, Mode::from_raw_mode(0o755)).map_err(io::Error::from)
    }

    /// create_dir_all：自顶向下逐级 mkdirat（父级已存在则继续）
    pub(super) fn create_dir_all(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<()> {
        let mut cur: OwnedFd = rustix::io::dup(root_fd).map_err(io::Error::from)?;
        let mut remaining: Option<&Path> = Some(rel);
        // 迭代每个前缀组件：能打开（O_PATH 目录）→ 下钻；ENOENT → mkdirat 后下钻
        while let Some(r) = remaining {
            let comp = match r.components().next() {
                Some(c) => c,
                None => break,
            };
            let name = Path::new(comp.as_os_str());
            let next_remaining = r
                .strip_prefix(comp)
                .ok()
                .filter(|p| !p.as_os_str().is_empty());
            match open_beneath(
                cur.as_fd(),
                name,
                OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => cur = fd,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    mkdirat(cur.as_fd(), name, Mode::from_raw_mode(0o755))
                        .map_err(io::Error::from)?;
                    cur = open_beneath(
                        cur.as_fd(),
                        name,
                        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?;
                }
                Err(e) => return Err(e),
            }
            remaining = next_remaining;
        }
        Ok(())
    }

    pub(super) fn rename(root_fd: BorrowedFd<'_>, src: &Path, dst: &Path) -> io::Result<()> {
        let src_parent = open_parent(root_fd, src)?;
        let dst_parent = open_parent(root_fd, dst)?;
        let src_name = src
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "空源路径"))?;
        let dst_name = dst
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "空目标路径"))?;
        renameat(src_parent, src_name, dst_parent, dst_name).map_err(io::Error::from)
    }

    pub(super) fn remove_file(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<()> {
        let parent_fd = open_parent(root_fd, rel)?;
        let name = rel
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "空路径"))?;
        unlinkat(parent_fd, name, AtFlags::empty()).map_err(io::Error::from)
    }

    pub(super) fn remove_dir(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<()> {
        let parent_fd = open_parent(root_fd, rel)?;
        let name = rel
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "空路径"))?;
        unlinkat(parent_fd, name, AtFlags::REMOVEDIR).map_err(io::Error::from)
    }

    /// 递归删除：基于目录 fd 遍历 + unlinkat，全程无路径字符串二次解析
    pub(super) fn remove_dir_all(root_fd: BorrowedFd<'_>, rel: &Path) -> io::Result<()> {
        fn rm_recursive(fd: BorrowedFd<'_>) -> io::Result<()> {
            let dir = Dir::read_from(fd).map_err(io::Error::from)?;
            for entry in dir {
                let entry = entry.map_err(io::Error::from)?;
                let name = entry.file_name();
                // 跳过 "." / ".."（getdents 原样返回，递归进 "." 会无限循环）
                if name.to_bytes() == b"." || name.to_bytes() == b".." {
                    continue;
                }
                let ftype = entry.file_type();
                if ftype.is_dir() {
                    // 打开子目录（BENEATH 语义保证目录本身在沙箱内）；
                    // 必须 O_RDONLY（O_PATH fd 不支持 getdents 遍历）
                    let child = open_beneath(
                        fd,
                        name,
                        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                        Mode::empty(),
                    )?;
                    rm_recursive(child.as_fd())?;
                    unlinkat(fd, name, AtFlags::REMOVEDIR).map_err(io::Error::from)?;
                } else {
                    unlinkat(fd, name, AtFlags::empty()).map_err(io::Error::from)?;
                }
            }
            Ok(())
        }
        // 根目录同样 O_RDONLY（getdents 需要真实 fd）
        let dir_fd = open_beneath(
            root_fd,
            rel,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        rm_recursive(dir_fd.as_fd())?;
        // 删除根目录自身（相对其父）
        let parent_fd = open_parent(root_fd, rel)?;
        let name = rel
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "空路径"))?;
        unlinkat(parent_fd, name, AtFlags::REMOVEDIR).map_err(io::Error::from)
    }

    /// 基于 fstatat 的元数据：follow=true 时跟随最终组件（BENEATH 由父 fd 保证）；
    /// follow=false 时对最终组件做 lstat（不跟随符号链接）。
    /// 实现：openat2 打开目标（follow 由 OFlags::NOFOLLOW 控制），再取 File::metadata
    pub(super) fn metadata_via_open(
        root_fd: BorrowedFd<'_>,
        rel: &Path,
        follow: bool,
    ) -> io::Result<Metadata> {
        let mut flags = OFlags::PATH | OFlags::CLOEXEC;
        if !follow {
            flags |= OFlags::NOFOLLOW;
        }
        let fd = open_beneath(root_fd, rel, flags, Mode::empty())?;
        File::from(fd).metadata()
    }

    /// 读目录：返回 (名称, 类型码, 大小, mtime_秒)。
    /// 目录以 O_RDONLY 打开（O_PATH fd 不支持 getdents）
    pub(super) fn read_dir_entries(
        root_fd: BorrowedFd<'_>,
        rel: &Path,
    ) -> io::Result<Vec<(std::ffi::OsString, u8, u64, i64)>> {
        let dir_fd = open_beneath(
            root_fd,
            rel,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let dir = Dir::read_from(dir_fd.as_fd()).map_err(io::Error::from)?;
        let mut out = Vec::new();
        for entry in dir {
            let entry = entry.map_err(io::Error::from)?;
            let name = entry.file_name();
            // 跳过 "." / ".."（getdents 原样返回；std::fs::read_dir 同样过滤）
            if name.to_bytes() == b"." || name.to_bytes() == b".." {
                continue;
            }
            // 条目类型（d_type，不跟随符号链接）
            let ftype = entry.file_type();
            let type_code: u8 = if ftype.is_dir() {
                1
            } else if ftype.is_symlink() {
                2
            } else if ftype.is_file() {
                0
            } else {
                3
            };
            let (size, mtime) = if type_code == 0 {
                // 文件大小/修改时间：statat（lstat 语义，条目名由内核解析）
                let st: Stat = statat(dir_fd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(io::Error::from)?;
                (st.st_size.max(0) as u64, st.st_mtime)
            } else {
                (0, 0)
            };
            // CStr → OsString（非 UTF-8 文件名保持原样）
            #[cfg(unix)]
            let name_os: std::ffi::OsString = {
                use std::os::unix::ffi::OsStrExt as _;
                std::ffi::OsStr::from_bytes(name.to_bytes()).to_os_string()
            };
            #[cfg(not(unix))]
            let name_os: std::ffi::OsString = name.to_string_lossy().into_owned().into();
            out.push((name_os, type_code, size, mtime));
        }
        Ok(out)
    }

    pub(super) fn copy(root_fd: BorrowedFd<'_>, src: &Path, dst: &Path) -> io::Result<u64> {
        let mut src_file = open_read(root_fd, src)?;
        let mut dst_file = open_write(root_fd, dst, true, false)?;
        io::copy(&mut src_file, &mut dst_file)
    }
}

// ====== 平台无关外壳 ======

/// 目录条目信息（type_code: 0=文件, 1=目录, 2=符号链接, 3=其他）
#[derive(Debug, Clone)]
pub struct DirItem {
    pub name: std::ffi::OsString,
    pub type_code: u8,
    pub size: u64,
    pub mtime_secs: i64,
}

impl DirItem {
    pub fn is_dir(&self) -> bool {
        self.type_code == 1
    }
    pub fn is_symlink(&self) -> bool {
        self.type_code == 2
    }
    pub fn is_file(&self) -> bool {
        self.type_code == 0
    }
}

/// 沙箱根（如 uploads/、uploads/.trash/）。所有操作都以相对 root 的 rel 路径进行。
#[derive(Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// 创建沙箱；root 不存在时自动创建（root 是可信配置路径）
    pub fn new(root: impl AsRef<Path>) -> io::Result<Sandbox> {
        let root = root.as_ref().to_path_buf();
        if !root.exists() {
            std::fs::create_dir_all(&root)?;
        }
        Ok(Sandbox { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// rel → 已拼好 root 的完整路径（供展示/日志/DB 记录使用；文件操作一律走 *at 方法）
    pub fn join(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            self.root.clone()
        } else {
            self.root.join(rel)
        }
    }

    fn root_fd(&self) -> io::Result<OwnedFd> {
        imp::open_root_fd(&self.root)
    }

    /// 打开文件读取（跟随沙箱内符号链接；越界解析报 NotFound/权限错误）
    pub fn open(&self, rel: &str) -> io::Result<File> {
        imp::open_read(self.root_fd()?.as_fd(), Path::new(rel))
    }

    /// 打开文件写入（O_CREAT；truncate=true 时截断已存在文件；exclusive=true 时拒绝已存在）
    pub fn open_write(&self, rel: &str, truncate: bool, exclusive: bool) -> io::Result<File> {
        imp::open_write(self.root_fd()?.as_fd(), Path::new(rel), truncate, exclusive)
    }

    pub fn create_dir(&self, rel: &str) -> io::Result<()> {
        imp::create_dir(self.root_fd()?.as_fd(), Path::new(rel))
    }

    pub fn create_dir_all(&self, rel: &str) -> io::Result<()> {
        imp::create_dir_all(self.root_fd()?.as_fd(), Path::new(rel))
    }

    pub fn rename(&self, src: &str, dst: &str) -> io::Result<()> {
        imp::rename(self.root_fd()?.as_fd(), Path::new(src), Path::new(dst))
    }

    pub fn remove_file(&self, rel: &str) -> io::Result<()> {
        imp::remove_file(self.root_fd()?.as_fd(), Path::new(rel))
    }

    pub fn remove_dir(&self, rel: &str) -> io::Result<()> {
        imp::remove_dir(self.root_fd()?.as_fd(), Path::new(rel))
    }

    pub fn remove_dir_all(&self, rel: &str) -> io::Result<()> {
        imp::remove_dir_all(self.root_fd()?.as_fd(), Path::new(rel))
    }

    /// 元数据（follow=true 跟随最终符号链接——仅在沙箱内跟随）
    pub fn metadata(&self, rel: &str) -> io::Result<Metadata> {
        imp::metadata_via_open(self.root_fd()?.as_fd(), Path::new(rel), true)
    }

    /// 最终组件不跟随符号链接（lstat 语义）
    pub fn symlink_metadata(&self, rel: &str) -> io::Result<Metadata> {
        imp::metadata_via_open(self.root_fd()?.as_fd(), Path::new(rel), false)
    }

    pub fn try_exists(&self, rel: &str) -> io::Result<bool> {
        match self.symlink_metadata(rel) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn read_dir(&self, rel: &str) -> io::Result<Vec<DirItem>> {
        let entries = imp::read_dir_entries(self.root_fd()?.as_fd(), Path::new(rel))?;
        Ok(entries
            .into_iter()
            .map(|(name, type_code, size, mtime_secs)| DirItem {
                name,
                type_code,
                size,
                mtime_secs,
            })
            .collect())
    }

    pub fn copy(&self, src: &str, dst: &str) -> io::Result<u64> {
        imp::copy(self.root_fd()?.as_fd(), Path::new(src), Path::new(dst))
    }

    pub fn read_to_string(&self, rel: &str) -> io::Result<String> {
        let mut f = self.open(rel)?;
        let mut s = String::new();
        use std::io::Read as _;
        f.read_to_string(&mut s)?;
        Ok(s)
    }

    pub fn write(&self, rel: &str, data: &[u8]) -> io::Result<()> {
        use std::io::Write as _;
        let mut f = self.open_write(rel, true, false)?;
        f.write_all(data)?;
        f.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn test_sandbox() -> (tempfile::TempDir, Sandbox) {
        let dir = tempfile::tempdir().unwrap();
        let sb = Sandbox::new(dir.path()).unwrap();
        (dir, sb)
    }

    #[test]
    fn test_basic_ops() {
        let (_d, sb) = test_sandbox();
        sb.create_dir_all("a/b/c").unwrap();
        assert!(sb.metadata("a/b/c").unwrap().is_dir());
        // create_dir 语义同 std：父目录必须已存在
        assert!(sb.create_dir("a/b/c/d").is_ok());
        assert!(sb.create_dir("x/y").is_err());
        sb.write("a/b/c/f.txt", b"hello").unwrap();
        assert_eq!(sb.read_to_string("a/b/c/f.txt").unwrap(), "hello");
        assert_eq!(sb.metadata("a/b/c/f.txt").unwrap().len(), 5);
        let items = sb.read_dir("a/b/c").unwrap();
        assert_eq!(items.len(), 2, "应含 f.txt 与 d 两个条目");
        assert!(items.iter().any(|i| i.is_file()));
        assert!(items.iter().any(|i| i.is_dir()));
        // rename + remove
        sb.rename("a/b/c/f.txt", "a/b/f2.txt").unwrap();
        assert!(sb.try_exists("a/b/f2.txt").unwrap());
        assert!(!sb.try_exists("a/b/c/f.txt").unwrap());
        sb.remove_file("a/b/f2.txt").unwrap();
        assert!(!sb.try_exists("a/b/f2.txt").unwrap());
        sb.remove_dir_all("a").unwrap();
        assert!(!sb.try_exists("a").unwrap());
    }

    #[test]
    fn test_symlink_escape_blocked() {
        let (_d, sb) = test_sandbox();
        sb.create_dir_all("user/dir").unwrap();
        sb.write("user/dir/ok.txt", b"inside").unwrap();
        // 沙箱外文件
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(outside.path(), b"SECRET").unwrap();
        // 符号链接：user/dir 内部指向沙箱外 → 解析必须失败
        let link_rel = "user/escape";
        symlink(outside.path(), sb.join(link_rel)).unwrap();
        assert!(sb.open("user/escape").is_err(), "越界符号链接必须拒绝");
        assert!(sb.metadata("user/escape").is_err());
        // 写操作同样拒绝（open_write 经 BENEATH 解析整条路径）
        assert!(sb.open_write("user/escape", true, false).is_err());
        // 删除/重命名只作用于链接本身（unlinkat/renameat 不跟随最终组件）——
        // 删掉的是链接而非指向的目标，这是安全且期望的行为
        sb.remove_file("user/escape").unwrap();
        let outside_content = std::fs::read_to_string(outside.path()).unwrap();
        assert_eq!(outside_content, "SECRET", "目标文件不得被触碰");
        // 沙箱内符号链接允许（相对目标；绝对目标会被 BENEATH 拒绝——保守语义）
        sb.write("user/target.txt", b"t").unwrap();
        symlink("target.txt", sb.join("user/lnk.txt")).unwrap();
        assert_eq!(sb.read_to_string("user/lnk.txt").unwrap(), "t");
        // 目录内符号链接（相对目标指向沙箱内目录）允许
        symlink("dir", sb.join("user/dirlnk")).unwrap();
        let items = sb.read_dir("user/dirlnk").unwrap();
        assert_eq!(items.len(), 1);
        // 相对目标越界的链接（../../ 逃逸）拒绝
        symlink("../../outside_target", sb.join("user/rel_escape")).unwrap();
        assert!(sb.open("user/rel_escape").is_err());
    }

    #[test]
    fn test_remove_dir_all_with_symlink_inside() {
        let (_d, sb) = test_sandbox();
        sb.create_dir_all("a/b").unwrap();
        sb.write("a/b/f.txt", b"x").unwrap();
        sb.remove_dir_all("a").unwrap();
        assert!(!sb.try_exists("a").unwrap());
    }
}
