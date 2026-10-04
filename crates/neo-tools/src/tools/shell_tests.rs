
    use super::*;

    // 仅重启本测试程序：不运行桌面、录音、网络工具，也不依赖本机 shell 安装。
    fn helper(mode: &str, dir: &Path) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "tools::shell::tests::lifecycle_child",
            "--ignored",
            "--nocapture",
        ])
        .env("NEO_LIFECYCLE_CHILD", mode)
        .env("NEO_LIFECYCLE_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        cmd
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "neo-shell-lifecycle-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[ignore = "仅由隔离生命周期回归作为短进程启动"]
    fn lifecycle_child() {
        let Ok(mode) = std::env::var("NEO_LIFECYCLE_CHILD") else {
            return;
        };
        let dir = PathBuf::from(std::env::var_os("NEO_LIFECYCLE_DIR").unwrap());
        match mode.as_str() {
            "output" => {
                println!("captured-stdout");
                eprintln!("captured-stderr");
                print!("{}", "x".repeat(limits::OUTPUT_BYTES * 2));
            }
            "nested" => {
                let captured = run_foreground(
                    &mut helper("output", &dir),
                    &Scope::new(&dir),
                    Duration::from_secs(5),
                )
                .unwrap();
                assert!(captured.status.success());
                println!("nested-job-ok");
            }
            "leaf" => {
                std::fs::write(dir.join("ready"), b"ready").unwrap();
                std::thread::sleep(Duration::from_millis(1800));
                let _ = std::fs::write(dir.join("escaped"), b"must not survive foreground");
            }
            "released-leaf" => {
                std::fs::write(dir.join("ready"), b"ready").unwrap();
                // 即使父测试失败，也自行退出，不留下后台进程。
                let deadline = Instant::now() + Duration::from_secs(10);
                while !dir.join("release").exists() && Instant::now() < deadline {
                    std::thread::sleep(POLL_INTERVAL);
                }
                if dir.join("release").exists() {
                    std::fs::write(dir.join("escaped"), b"survived shell exit").unwrap();
                }
            }
            "tree" | "parent-exits" | "parent-exits-no-pipes" => {
                let no_pipes = mode == "parent-exits-no-pipes";
                let child = helper("leaf", &dir)
                    .stdout(if no_pipes { Stdio::null() } else { Stdio::inherit() })
                    .stderr(if no_pipes { Stdio::null() } else { Stdio::inherit() })
                    .spawn()
                    .unwrap();
                drop(child);
                if no_pipes {
                    let deadline = Instant::now() + Duration::from_secs(3);
                    while !dir.join("ready").exists() && Instant::now() < deadline {
                        std::thread::sleep(POLL_INTERVAL);
                    }
                    assert!(dir.join("ready").exists());
                }
                if mode == "tree" {
                    std::thread::sleep(Duration::from_secs(4));
                }
            }
            _ => panic!("unknown helper mode"),
        }
    }

    #[test]
    fn lifecycle_collects_both_streams_and_caps_output() {
        let dir = TempDir::new();
        let captured = run_foreground(
            &mut helper("output", &dir.0),
            &Scope::new(&dir.0),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(captured.status.success());
        assert_eq!(captured.stdout.len(), limits::OUTPUT_BYTES);
        assert!(String::from_utf8_lossy(&captured.stdout).contains("captured-stdout"));
        assert!(String::from_utf8_lossy(&captured.stderr).contains("captured-stderr"));
    }

    #[test]
    fn lifecycle_timeout_kills_descendants_with_inherited_pipes() {
        let dir = TempDir::new();
        let started = Instant::now();
        let result = run_foreground(
            &mut helper("tree", &dir.0),
            &Scope::new(&dir.0),
            Duration::from_millis(700),
        );
        assert_eq!(result.err().unwrap().kind, ErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(dir.0.join("ready").exists(), "后代必须确实启动");
        std::thread::sleep(Duration::from_secs(2));
        assert!(!dir.0.join("escaped").exists());
    }

    #[test]
    fn lifecycle_cancellation_kills_running_tree() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let dir = TempDir::new();
        let token = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(&dir.0).with_cancel(token.clone());
        let path = dir.0.clone();
        let cancel = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !path.join("ready").exists() && Instant::now() < deadline {
                std::thread::sleep(POLL_INTERVAL);
            }
            token.store(true, Ordering::Release);
        });
        let result = run_foreground(&mut helper("tree", &dir.0), &scope, Duration::from_secs(5));
        cancel.join().unwrap();
        let error = result.err().unwrap();
        assert_eq!(error.kind, ErrorKind::NotAllowed);
        assert!(error.message.contains("取消"));
        assert!(dir.0.join("ready").exists());
        std::thread::sleep(Duration::from_secs(2));
        assert!(!dir.0.join("escaped").exists());
    }

    #[test]
    fn lifecycle_parent_exit_does_not_wait_for_pipe_holding_descendant() {
        let dir = TempDir::new();
        let started = Instant::now();
        let result = run_foreground(
            &mut helper("parent-exits", &dir.0),
            &Scope::new(&dir.0),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.status.success());
        assert!(started.elapsed() < Duration::from_millis(1500));
        std::thread::sleep(Duration::from_secs(2));
        assert!(!dir.0.join("escaped").exists());
    }

    #[test]
    fn lifecycle_foreground_cleans_gui_like_child_even_without_inherited_pipes() {
        let dir = TempDir::new();
        let result = run_foreground(
            &mut helper("parent-exits-no-pipes", &dir.0),
            &Scope::new(&dir.0),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(result.status.success());
        assert!(dir.0.join("ready").exists(), "子进程必须实际运行过");
        std::thread::sleep(Duration::from_secs(2));
        assert!(!dir.0.join("escaped").exists());
    }

    #[test]
    fn lifecycle_precancel_and_spawn_failure_do_not_execute() {
        let dir = TempDir::new();
        let scope = Scope::new(&dir.0).with_cancel(std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(true),
        ));
        assert_eq!(
            run_foreground(&mut helper("leaf", &dir.0), &scope, Duration::from_secs(5))
                .err()
                .unwrap()
                .kind,
            ErrorKind::NotAllowed
        );
        assert!(!dir.0.join("ready").exists());
        let mut missing = Command::new(dir.0.join("nonexistent-shell"));
        assert_eq!(
            run_foreground(&mut missing, &Scope::new(&dir.0), Duration::from_secs(5))
                .err()
                .unwrap()
                .kind,
            ErrorKind::Io
        );
    }

    #[cfg(windows)]
    #[test]
    fn lifecycle_job_assignment_failure_never_runs_user_command() {
        let dir = TempDir::new();
        let started = Instant::now();
        assert!(process_tree::spawn_with_failing_assignment(&mut helper("leaf", &dir.0)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(100));
        assert!(!dir.0.join("ready").exists());
    }

    #[cfg(windows)]
    #[test]
    fn lifecycle_nested_job_is_compatible_with_external_job() {
        let dir = TempDir::new();
        let captured = run_foreground(
            &mut helper("nested", &dir.0),
            &Scope::new(&dir.0),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(
            captured.status.success(),
            "{}",
            String::from_utf8_lossy(&captured.stderr)
        );
        assert!(String::from_utf8_lossy(&captured.stdout).contains("nested-job-ok"));
    }

    #[test]
    fn lifecycle_timeout_does_not_kill_unrelated_process() {
        let unrelated = TempDir::new();
        let mut child = helper("leaf", &unrelated.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let dir = TempDir::new();
        let result = run_foreground(
            &mut helper("tree", &dir.0),
            &Scope::new(&dir.0),
            Duration::from_millis(100),
        );
        assert_eq!(result.err().unwrap().kind, ErrorKind::Timeout);
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        let status = child.try_wait().unwrap();
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(status.is_some_and(|status| status.success()));
        assert!(unrelated.0.join("escaped").exists());
    }

    #[cfg(windows)]
    #[test]
    fn lifecycle_powershell_dispatch_captures_output_and_times_out() {
        let dir = TempDir::new();
        let scope = Scope::new(&dir.0);
        let outcome = crate::dispatch(
            &scope,
            "powershell",
            &json!({
                "command": "[Console]::WriteLine('foreground-ok'); [Console]::Error.WriteLine('stderr-ok')",
                "timeout_ms": 5000
            }),
        );
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.data["exit_code"], 0, "{}", outcome.data);
        assert!(outcome.data["stdout"]
            .as_str()
            .unwrap()
            .contains("foreground-ok"));
        assert!(outcome.data["stderr"]
            .as_str()
            .unwrap()
            .contains("stderr-ok"));
        let started = Instant::now();
        let outcome = crate::dispatch(
            &scope,
            "powershell",
            &json!({
                "command": "Start-Sleep -Seconds 4", "timeout_ms": 300
            }),
        );
        assert_eq!(outcome.error.unwrap().kind, ErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[cfg(windows)]
    #[test]
    fn lifecycle_explicit_background_survives_token_cancellation() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let dir = TempDir::new();
        let token = Arc::new(AtomicBool::new(false));
        let scope = Scope::new(&dir.0).with_cancel(token.clone());
        let outcome = crate::dispatch(
            &scope,
            "powershell",
            &json!({
                "command": "Start-Sleep -Milliseconds 400; [IO.File]::WriteAllText((Join-Path (Get-Location) 'background.txt'), 'done')",
                "background": true
            }),
        );
        assert!(outcome.error.is_none(), "{:?}", outcome.error);
        assert_eq!(outcome.data["background"], true);
        token.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.0.join("background.txt").exists() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        assert!(dir.0.join("background.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn lifecycle_background_child_survives_powershell_exit() {
        let dir = TempDir::new();
        let host = resolve(ShellKind::PowerShell).unwrap();
        let quote = |path: &Path| path.to_string_lossy().replace('\'', "''");
        // 只启动本测试的有期限 fixture，不启动真实 GUI；不修改测试进程的环境。
        let command = format!(
            "$env:NEO_LIFECYCLE_CHILD = 'released-leaf'; \
             $env:NEO_LIFECYCLE_DIR = '{}'; \
             Start-Process -FilePath '{}' -NoNewWindow \
             -ArgumentList '--exact tools::shell::tests::lifecycle_child --ignored --nocapture'",
            quote(&dir.0),
            quote(&std::env::current_exe().unwrap()),
        );
        // 与 exec(background=true) 共用 spawn；保留 Child 仅为确认 shell 确实退出，
        // 而不是把“工具已返回”误当成“shell 已退出”。
        let mut child = spawn(ShellKind::PowerShell, &host, &dir.0, &command, false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        let status = child.try_wait().unwrap();
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(status.is_some_and(|status| status.success()));
        drop(child);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.0.join("ready").exists() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        assert!(dir.0.join("ready").exists());
        assert!(!dir.0.join("escaped").exists());
        std::fs::write(dir.0.join("release"), b"shell has exited").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !dir.0.join("escaped").exists() && Instant::now() < deadline {
            std::thread::sleep(POLL_INTERVAL);
        }
        assert!(dir.0.join("escaped").exists(), "子进程必须在 shell 退出后仍能执行");
    }

    #[test]
    fn powershell_wrap_fixes_upstream_problems() {
        let w = wrap_powershell("Get-ChildItem");
        assert!(w.contains("Get-ChildItem"));
        // 编码：否则中文输出按 GBK 写出、我们按 UTF-8 解码就是乱码
        assert!(w.contains("[Console]::OutputEncoding"));
        // 退出码：否则 cargo test 失败也会报 0
        assert!(w.contains("$LASTEXITCODE"));
        // 错误即终止
        assert!(w.contains("$ErrorActionPreference = 'Stop'"));
    }

    #[test]
    fn clip_marks_truncation() {
        let (text, cut) = clip(&[b'a'; 10]);
        assert_eq!(text, "aaaaaaaaaa");
        assert!(!cut);

        let (text, cut) = clip(&vec![b'a'; limits::OUTPUT_BYTES + 5]);
        assert!(cut);
        assert!(text.contains("已截断"));
    }

    /// WSL 的应用执行别名必须被排除 —— 跑它等于去启动一个 Linux 发行版。
    #[test]
    fn wsl_stub_is_rejected() {
        assert!(is_wsl_stub(Path::new(
            r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\bash.exe"
        )));
        assert!(!is_wsl_stub(Path::new(
            r"C:\Program Files\Git\bin\bash.exe"
        )));
    }

    /// 从 `git.exe` 反推 bash：两种安装布局都要覆盖。
    #[test]
    fn bash_is_derived_from_git_layouts() {
        // 标准安装：<root>\cmd\git.exe
        let v = derive_from_git(Path::new(r"C:\Program Files\Git\cmd\git.exe"));
        assert!(
            v.iter()
                .any(|p| p.ends_with(Path::new("Git").join("bin").join("bash.exe"))),
            "标准安装应推出 <root>\\bin\\bash.exe：{v:?}"
        );
        // PortableGit：<root>\mingw64\bin\git.exe
        let v = derive_from_git(Path::new(r"D:\pg\mingw64\bin\git.exe"));
        assert!(
            v.iter()
                .any(|p| p.ends_with(Path::new("mingw64").join("bin").join("bash.exe"))),
            "PortableGit 应推出同层的 bash.exe：{v:?}"
        );
        assert!(
            v.iter()
                .any(|p| p.ends_with(Path::new("usr").join("bin").join("bash.exe"))),
            "同时应推出 usr\\bin\\bash.exe：{v:?}"
        );
    }

    #[test]
    fn release_paths_gitbash_release_precedes_cache_and_keeps_sh_fallback() {
        let base = PathBuf::from("package");
        let candidates = bundled_bash_candidates(runtime_roots_from(None, Some(base.clone()), None));
        let release = base.join("runtime/gitbash");
        let cache = base.join(".cache/runtime/gitbash");
        assert_eq!(
            &candidates[..6],
            &[
                release.join("bin/bash.exe"),
                release.join("usr/bin/bash.exe"),
                release.join("usr/bin/sh.exe"),
                cache.join("bin/bash.exe"),
                cache.join("usr/bin/bash.exe"),
                cache.join("usr/bin/sh.exe"),
            ]
        );
        for (available, expected) in [
            (
                vec![candidates[0].clone(), candidates[3].clone()],
                candidates[0].clone(),
            ),
            (
                vec![candidates[2].clone(), candidates[3].clone()],
                candidates[2].clone(),
            ),
            (vec![candidates[3].clone()], candidates[3].clone()),
            (vec![candidates[5].clone()], candidates[5].clone()),
        ] {
            assert_eq!(
                candidates.iter().find(|p| available.contains(p)),
                Some(&expected)
            );
        }
    }

    #[test]
    fn release_paths_gitbash_runtime_override_stays_first() {
        let roots = runtime_roots_from(
            Some("  custom/gitbash  "),
            Some(PathBuf::from("package")),
            None,
        );
        assert_eq!(roots[0], Path::new("custom/gitbash"));
        assert_eq!(roots[1], Path::new("package/runtime/gitbash"));
        assert_eq!(roots[2], Path::new("package/.cache/runtime/gitbash"));
        let roots = runtime_roots_from(Some("  "), Some(PathBuf::from("package")), None);
        assert_eq!(roots[0], Path::new("package/runtime/gitbash"));
    }

    #[test]
    fn release_paths_gitbash_searches_only_cache_in_exe_ancestors_and_cwd() {
        let exe_dir = PathBuf::from("repo/target/debug");
        let cwd = PathBuf::from("working/nested");
        let roots = runtime_roots_from(None, Some(exe_dir.clone()), Some(cwd.clone()));
        assert_eq!(roots[0], exe_dir.join("runtime/gitbash"));
        assert_eq!(roots[1], exe_dir.join(".cache/runtime/gitbash"));
        let repo_cache = roots
            .iter()
            .position(|p| p == Path::new("repo/.cache/runtime/gitbash"))
            .unwrap();

        let cwd_cache = roots
            .iter()
            .position(|p| p == &cwd.join(".cache/runtime/gitbash"))
            .unwrap();
        assert!(repo_cache < cwd_cache);
        assert!(roots.contains(&PathBuf::from("working/.cache/runtime/gitbash")));
        for base in exe_dir.ancestors().skip(1).chain(cwd.ancestors()) {
            assert!(
                !roots.contains(&base.join("runtime/gitbash")),
                "旧路径不应被发现：{base:?}"
            );
        }
    }

    #[test]
    fn release_paths_gitbash_cwd_does_not_replace_missing_exe_dir() {
        let cwd = PathBuf::from("repo");
        let roots = runtime_roots_from(None, None, Some(cwd.clone()));
        assert_eq!(roots[0], cwd.join(".cache/runtime/gitbash"));
        assert!(!roots.contains(&cwd.join("runtime/gitbash")));
        let candidates = bundled_bash_candidates(roots);
        let old_bash = cwd.join("runtime/gitbash/usr/bin/bash.exe");
        assert!(!candidates.contains(&old_bash));
        assert!(runtime_roots_from(None, None, None).is_empty());
    }

    /// 候选表里**不该有** WSL 别名；随包运行时排在 PATH 之前。
    #[test]
    fn candidates_put_bundled_runtime_first_and_drop_wsl() {
        let c = bash_candidates();
        assert!(!c.is_empty());
        assert!(
            !c.iter().any(|p| is_wsl_stub(p)),
            "候选里混进了 WSL 别名：{c:?}"
        );
        assert!(
            c[0].to_string_lossy().contains("runtime"),
            "随包运行时必须是第一个候选（优先级最高）：{c:?}"
        );
        assert!(
            c[0].ends_with("runtime\\gitbash\\bin\\bash.exe")
                || c[0].ends_with("runtime/gitbash/bin/bash.exe"),
            "随包 bash 的位置不对：{}",
            c[0].display()
        );
    }

    #[test]
    fn resolution_never_panics() {
        if let Ok(host) = resolve(ShellKind::PowerShell) {
            assert!(!host.executable.as_os_str().is_empty());
            assert!(matches!(host.kind, "pwsh" | "powershell"));
        }
        // bash 在有些机器上确实没有 —— 这时必须是**可读的**错误，而不是 panic
        if let Err(e) = resolve(ShellKind::Bash) {
            assert_eq!(e.kind, ErrorKind::Unsupported);
            assert!(e.hint.is_some(), "必须给出路");
        }
    }
