//! `cap-std` 沙箱的**行为契约回归测试**（原为阶段 0 的 spike，2026-09-30 正名为契约测试）。
//!
//! 它验证的是 `cap-std` **自身**的行为、不依赖本仓工具层，所以放在集成测试里：
//! 一旦 `cap-std` 升级破坏这些前提，本文件先报警，而不是等到 filesystem 工具出现诡异行为。
//!
//! 对应 `docs/planned-agent/filesystem-tools-rewrite.md` §11 阶段 0 与 §12 R1。
//!
//! 四个待验证能力：
//! 1. `Dir::open_ambient_dir` 能否打开本机目录句柄；
//! 2. 相对句柄上的 read / write / create_dir_all / read_dir / rename / metadata 是否可用；
//! 3. `..` 逃逸是否被拒；
//! 4. symlink 逃逸是否被拒（本机无 symlink 权限时跳过）。

use cap_std::ambient_authority;
use cap_std::fs::Dir;

#[test]
fn cap_std_open_read_write_list_rename_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = Dir::open_ambient_dir(tmp.path(), ambient_authority())
        .expect("open_ambient_dir 在本机应可用");

    // write + read_to_string
    dir.write("a.txt", b"hello").expect("write");
    assert_eq!(dir.read_to_string("a.txt").expect("read_to_string"), "hello");

    // create_dir_all（多级）+ 嵌套写
    dir.create_dir_all("sub/deep").expect("create_dir_all");
    dir.write("sub/deep/b.txt", b"x").expect("write nested");
    assert!(dir.read_to_string("sub/deep/b.txt").is_ok());

    // read_dir（相对句柄上的枚举）
    let names: Vec<String> = dir
        .read_dir(".")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().any(|n| n == "a.txt"), "read_dir 应看到 a.txt: {names:?}");

    // rename（同一句柄内）
    dir.rename("a.txt", &dir, "renamed.txt").expect("rename");
    assert!(dir.read_to_string("renamed.txt").is_ok());
    assert!(dir.read_to_string("a.txt").is_err());

    // metadata
    let meta = dir.metadata("renamed.txt").expect("metadata");
    assert!(meta.is_file());

    // remove（unzip/清理场景要用）
    dir.remove_file("renamed.txt").expect("remove_file");
    assert!(dir.read_to_string("renamed.txt").is_err());
}

#[test]
fn cap_std_rename_overwrite_behaviour() {
    // 探明 cap-std 的 rename 能否覆盖已存在目标 —— 决定 `builtin_write_file` 的原子策略。
    let tmp = tempfile::tempdir().unwrap();
    let dir = Dir::open_ambient_dir(tmp.path(), ambient_authority()).unwrap();

    dir.write("target.txt", b"old").unwrap();
    dir.write("source.txt", b"new").unwrap();

    let result = dir.rename("source.txt", &dir, "target.txt");
    eprintln!("cap-std rename onto existing target => {result:?}");

    match result {
        Ok(()) => assert_eq!(dir.read_to_string("target.txt").unwrap(), "new"),
        // 不支持覆盖：目标保持原值
        Err(_) => assert_eq!(dir.read_to_string("target.txt").unwrap(), "old"),
    }
}

#[test]
fn cap_std_denies_parent_dir_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    // 在 root 外放一个文件，试图用 .. 读它
    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, b"secret").unwrap();

    let dir = Dir::open_ambient_dir(&root, ambient_authority()).unwrap();
    let escaped = dir.read_to_string("../outside.txt");
    assert!(
        escaped.is_err(),
        "cap-std 必须拒绝 `..` 逃逸，但读到了：{escaped:?}"
    );
}

#[test]
fn cap_std_denies_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root");
    std::fs::create_dir_all(&root).unwrap();

    let outside = tmp.path().join("outside.txt");
    std::fs::write(&outside, b"secret").unwrap();

    let link = root.join("link.txt");
    let created = {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(&outside, &link).is_ok()
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, &link).is_ok()
        }
    };

    if !created {
        eprintln!("跳过 symlink 逃逸用例：本机无创建 symlink 权限（Windows 需开发者模式或管理员）");
        return;
    }

    let dir = Dir::open_ambient_dir(&root, ambient_authority()).unwrap();
    let leaked = dir.read_to_string("link.txt");
    assert!(
        leaked.is_err(),
        "cap-std 必须拒绝 symlink 逃逸，但读到了：{leaked:?}"
    );
}
