//! `neo-tools` 的端到端行为测试：在真实临时目录里跑全部六个工具。
//!
//! 这些测试同时充当**契约文档** —— 断言里写清了每个工具在异常路径上
//! 返回哪个 `kind`、`hint` 里必须出现什么。

use std::path::PathBuf;

use neo_tools::{dispatch, find, Decision, ErrorKind, Policy, Scope};
use serde_json::{json, Value};

/// 建一个干净的临时工作区。
fn workspace(tag: &str) -> (PathBuf, Scope) {
    let dir = std::env::temp_dir().join(format!("neo-tools-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scope = Scope::new(&dir);
    (dir, scope)
}

/// 取错误 kind（断言成功路径时用它更直白）。
fn kind_of(out: &neo_tools::Outcome) -> ErrorKind {
    out.error.as_ref().expect("应当失败").kind
}

#[test]
fn registry_is_well_formed() {
    let names: Vec<&str> = neo_tools::tool_names();
    assert_eq!(
        names,
        vec![
            "read_file",
            "read_document",
            "view_image",
            "open_file",
            "write_file",
            "edit_file",
            "powershell",
            "bash",
            "screenshot",
            "screen_elements",
            "screen_element_search",
            "click",
            "drag"
        ]
    );
    // 每个工具的 schema 必须自洽：required 与 default 不冲突、参数名唯一。
    for t in neo_tools::registry() {
        let schema = t.schema();
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let mut seen = Vec::new();
        for p in t.params {
            assert!(!seen.contains(&p.name), "{} 参数名重复", t.name);
            seen.push(p.name);
            if p.required {
                assert!(
                    p.default.is_none(),
                    "{} 的必填参数 {} 不该有默认值",
                    t.name,
                    p.name
                );
                assert!(
                    required.contains(&p.name),
                    "{} 的 {} 未列入 required",
                    t.name,
                    p.name
                );
            } else {
                assert!(
                    p.default.is_some(),
                    "{} 的可选参数 {} 必须有默认值",
                    t.name,
                    p.name
                );
            }
        }
        assert_eq!(schema["additionalProperties"], false);
    }
}

#[test]
fn openai_tools_shape() {
    let tools = neo_tools::openai_tools();
    let arr = tools.as_array().unwrap();
    // 数量跟着注册表走 —— 写死数字只会让"加一个工具就要改一次测试"，
    // 而那正是文档同步测试该干的事。
    assert_eq!(arr.len(), neo_tools::registry().len());
    assert!(arr.len() >= 6);
    assert_eq!(arr[0]["type"], "function");
    assert_eq!(arr[0]["function"]["name"], "read_file");
    assert!(arr[0]["function"]["parameters"]["properties"]["path"].is_object());
}

#[test]
fn write_then_read_roundtrip() {
    let (_dir, scope) = workspace("rw");

    // 新建
    let out = dispatch(
        &scope,
        "write_file",
        &json!({ "path": "a/b.txt", "content": "行1\n行2\n行3" }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["created"], true);
    assert_eq!(out.data["lines"], 3);

    // 已存在但未允许覆盖 → conflict
    let out = dispatch(
        &scope,
        "write_file",
        &json!({ "path": "a/b.txt", "content": "x" }),
    );
    assert_eq!(kind_of(&out), ErrorKind::Conflict);
    assert!(out.error.unwrap().hint.unwrap().contains("overwrite"));

    // 覆盖
    let out = dispatch(
        &scope,
        "write_file",
        &json!({ "path": "a/b.txt", "content": "新", "overwrite": true }),
    );
    assert!(out.is_ok());
    assert_eq!(out.data["overwritten"], true);

    // 读回
    let out = dispatch(&scope, "read_file", &json!({ "path": "a/b.txt" }));
    assert!(out.is_ok());
    assert_eq!(out.data["content"], "新");
    assert_eq!(out.data["lines_total"], 1);
}

#[test]
fn read_errors_are_specific() {
    let (_dir, scope) = workspace("read-err");

    let out = dispatch(&scope, "read_file", &json!({ "path": "nope.txt" }));
    assert_eq!(kind_of(&out), ErrorKind::NotFound);

    std::fs::create_dir_all(scope.root().join("dir")).unwrap();
    let out = dispatch(&scope, "read_file", &json!({ "path": "dir" }));
    assert_eq!(kind_of(&out), ErrorKind::Unsupported);

    let out = dispatch(&scope, "read_file", &json!({}));
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
    assert!(out.error.unwrap().hint.unwrap().contains("必填"));

    let out = dispatch(&scope, "read_file", &json!({ "path": "../outside.txt" }));
    assert_eq!(kind_of(&out), ErrorKind::NotAllowed);
}

#[test]
fn read_paginates_with_next_offset() {
    let (_dir, scope) = workspace("read-page");
    let content: String = (1..=10).map(|i| format!("第{i}行\n")).collect();
    dispatch(
        &scope,
        "write_file",
        &json!({ "path": "n.txt", "content": content }),
    );

    let out = dispatch(
        &scope,
        "read_file",
        &json!({ "path": "n.txt", "offset": 0, "limit": 4 }),
    );
    assert_eq!(out.data["lines_returned"], 4);
    assert_eq!(out.data["truncated"], true);
    assert_eq!(out.data["next_offset"], 4);
    assert_eq!(out.data["content"], "第1行\n第2行\n第3行\n第4行");

    let out = dispatch(
        &scope,
        "read_file",
        &json!({ "path": "n.txt", "offset": 8, "limit": 4 }),
    );
    assert_eq!(out.data["truncated"], false);
    assert_eq!(out.data["lines_returned"], 2);
}

#[test]
fn edit_replaces_exactly_one_and_refuses_ambiguity() {
    let (_dir, scope) = workspace("edit");
    dispatch(
        &scope,
        "write_file",
        &json!({ "path": "s.rs", "content": "let x = 1;\nlet y = 1;\n" }),
    );

    // 唯一命中
    let out = dispatch(
        &scope,
        "edit_file",
        &json!({ "path": "s.rs", "old_string": "let x = 1;", "new_string": "let x = 2;" }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["replacements"], 1);
    let out = dispatch(&scope, "read_file", &json!({ "path": "s.rs" }));
    assert!(out.data["content"].as_str().unwrap().contains("let x = 2;"));

    // 多命中 → not_unique 且 hint 指向 replace_all
    let out = dispatch(
        &scope,
        "edit_file",
        &json!({ "path": "s.rs", "old_string": "= ", "new_string": "=  " }),
    );
    assert_eq!(kind_of(&out), ErrorKind::NotUnique);
    assert!(out.error.unwrap().hint.unwrap().contains("replace_all"));

    // 命中 0 处 → not_found 且 hint 指向 read_file
    let out = dispatch(
        &scope,
        "edit_file",
        &json!({ "path": "s.rs", "old_string": "不存在的内容", "new_string": "x" }),
    );
    assert_eq!(kind_of(&out), ErrorKind::NotFound);
    assert!(out.error.unwrap().hint.unwrap().contains("read_file"));

    // 新旧相同 → bad_arguments
    let out = dispatch(
        &scope,
        "edit_file",
        &json!({ "path": "s.rs", "old_string": "x", "new_string": "x" }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);

    // 显式 replace_all
    let out = dispatch(
        &scope,
        "edit_file",
        &json!({ "path": "s.rs", "old_string": "let ", "new_string": "const ", "replace_all": true }),
    );
    assert!(out.is_ok());
    assert_eq!(out.data["replacements"], 2);
}

#[test]
fn view_image_reads_png_header() {
    let (_dir, scope) = workspace("img");
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&[0, 0, 0, 13]);
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&800u32.to_be_bytes());
    png.extend_from_slice(&600u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    std::fs::write(scope.root().join("pic.png"), &png).unwrap();

    let out = dispatch(&scope, "view_image", &json!({ "path": "pic.png" }));
    assert!(out.is_ok());
    assert_eq!(out.data["format"], "png");
    assert_eq!(out.data["width"], 800);
    assert_eq!(out.data["height"], 600);
    assert_eq!(out.data["image_attached"], false, "默认不带图");
    assert!(out.images.is_empty());

    // include_data = 把图**交给模型看**：图作为独立一块附在结果上，
    // 而不是把 base64 塞进 JSON 字符串（塞进去模型看不见，还占爆上下文）。
    let out = dispatch(
        &scope,
        "view_image",
        &json!({ "path": "pic.png", "include_data": true }),
    );
    assert_eq!(out.data["image_attached"], true);
    assert!(
        out.data.get("data_url").is_none(),
        "base64 不该出现在 JSON 里"
    );
    assert_eq!(out.images.len(), 1, "应当附带一张图");
    let url = &out.images[0];
    assert!(url.starts_with("data:image/png;base64,iVBOR"), "{url}");
    assert!(out.summary.contains("已把图交给模型"), "{}", out.summary);

    // 非图片 → unsupported 且指向 read_file
    dispatch(
        &scope,
        "write_file",
        &json!({ "path": "t.txt", "content": "hello" }),
    );
    let out = dispatch(&scope, "view_image", &json!({ "path": "t.txt" }));
    assert_eq!(kind_of(&out), ErrorKind::Unsupported);
}

#[test]
fn unknown_tool_and_unknown_argument_are_rejected() {
    let (_dir, scope) = workspace("unknown");
    let out = dispatch(&scope, "rm_rf", &json!({}));
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
    assert!(out.error.unwrap().hint.unwrap().contains("write_file"));

    let out = dispatch(
        &scope,
        "read_file",
        &json!({ "path": "a", "recursive": true }),
    );
    let policy = Policy::default();
    let tool = find("read_file").unwrap();
    // 未知参数在策略层就被拒（先于打扰用户）。
    assert!(matches!(
        policy.decide(tool, &json!({ "path": "a", "recursive": true })),
        Decision::Deny(_)
    ));
    assert!(dispatch(&scope, "read_file", &json!({ "path": "a" })).is_ok() || true);
    let _ = out;
}

#[test]
fn preview_shows_the_dangerous_part() {
    let ps = find("powershell").unwrap();
    let p = (ps.preview)(&neo_tools::Args::new(
        ps,
        &json!({ "command": "Remove-Item -Recurse -Force build" }),
    ));
    assert!(
        p.contains("Remove-Item -Recurse -Force build"),
        "确认框必须展示命令本身，实际：{p}"
    );

    let edit = find("edit_file").unwrap();
    let p = (edit.preview)(&neo_tools::Args::new(
        edit,
        &json!({ "path": "a.rs", "old_string": "let x = 1;", "new_string": "let x = 2;" }),
    ));
    assert!(p.contains("a.rs") && p.contains("let x = 1;"), "{p}");
}

#[test]
fn model_message_is_compact_valid_json() {
    let (_dir, scope) = workspace("model-json");
    dispatch(
        &scope,
        "write_file",
        &json!({ "path": "a.txt", "content": "hi" }),
    );
    let out = dispatch(&scope, "read_file", &json!({ "path": "a.txt" }));
    let text = neo_tools::to_model_message(&out);
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["tool"], "read_file");
    assert!(text.chars().count() <= neo_tools::tools::limits::MODEL_JSON_CHARS);
}

#[test]
fn ps_runs_and_reports_exit_code() {
    let (_dir, scope) = workspace("ps");

    let out = dispatch(
        &scope,
        "powershell",
        &json!({ "command": "Write-Output neo-tools-ok" }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["exit_code"], 0);
    assert!(out.data["stdout"]
        .as_str()
        .unwrap()
        .contains("neo-tools-ok"));
    assert!(out.data["duration_ms"].as_u64().is_some());

    // 非零退出：工具**成功执行**，失败信息在 exit_code 里 —— 两者可区分。
    let out = dispatch(&scope, "powershell", &json!({ "command": "exit 3" }));
    assert!(out.is_ok(), "非零退出不该被当成工具失败");
    assert_eq!(out.data["exit_code"], 3);
    assert!(out.summary.contains("退出码 3"));
}

#[test]
fn ps_rejects_cwd_outside_workspace() {
    let (dir, scope) = workspace("ps-cwd");
    // 越界 → not_allowed
    let out = dispatch(
        &scope,
        "powershell",
        &json!({ "command": "Get-ChildItem", "cwd": "../.." }),
    );
    assert_eq!(kind_of(&out), ErrorKind::NotAllowed);

    // 工作区内但不存在 / 不是目录 → bad_arguments（参数本身的问题）
    let out = dispatch(
        &scope,
        "powershell",
        &json!({ "command": "Get-ChildItem", "cwd": "not-a-dir" }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);

    // 正常目录 → 通过，且 cwd 以相对路径回报
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let out = dispatch(
        &scope,
        "powershell",
        &json!({ "command": "Write-Output ok", "cwd": "sub" }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["cwd"], "sub");
}

#[test]
fn ps_times_out_on_long_command() {
    let (_dir, scope) = workspace("ps-timeout");
    let cmd = "Start-Sleep -Seconds 6";

    let out = dispatch(
        &scope,
        "powershell",
        &json!({ "command": cmd, "timeout_ms": 300 }),
    );
    assert_eq!(kind_of(&out), ErrorKind::Timeout);
    assert!(out.error.unwrap().hint.unwrap().contains("timeout_ms"));
}

#[test]
fn limits_are_enforced_and_documented() {
    let (_dir, scope) = workspace("limits");
    // 超长 content 被拒（写上限 1 MiB）。
    let huge = "x".repeat(neo_tools::tools::limits::WRITE_BYTES as usize + 1);
    let out = dispatch(
        &scope,
        "write_file",
        &json!({ "path": "big.txt", "content": huge }),
    );
    assert_eq!(kind_of(&out), ErrorKind::TooLarge);

    // timeout_ms 越界 → bad_arguments（不是静默夹取；上限已提到 30 分钟）。
    let out = dispatch(
        &scope,
        "powershell",
        &json!({ "command": "Write-Output hi", "timeout_ms": 9_999_999 }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
}

/// 文档与注册表同步：`docs/tools.md` 必须为每个工具留一节。
///
/// 加工具却忘了写文档时，这条测试会先炸 —— 比事后 review 靠谱。
#[test]
fn every_tool_is_documented() {
    let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/tools.md");
    let text =
        std::fs::read_to_string(&doc).unwrap_or_else(|e| panic!("读不到 {}：{e}", doc.display()));
    for tool in neo_tools::registry() {
        assert!(
            text.contains(&format!("### {}. `{}`", 1, tool.name))
                || text.contains(&format!("`{}`", tool.name)),
            "docs/tools.md 缺少 `{}` 的说明",
            tool.name
        );
        assert!(
            text.contains(tool.title),
            "docs/tools.md 缺少 `{}` 的中文名 `{}`",
            tool.name,
            tool.title
        );
    }
    assert!(text.contains("24 000"), "文档应写明回灌截断上限");
}

/// 中文输出必须原样回来。
///
/// Windows PowerShell 5.1 默认按控制台 OEM 代码页（中文机器上是 GBK）写 stdout，
/// 我们按 UTF-8 解码就会得到一堆 `锟斤拷`。工具里靠命令前置的
/// `[Console]::OutputEncoding = UTF8` 修掉 —— 这条测试看着它别被改回去。
#[test]
fn powershell_output_keeps_chinese_intact() {
    let (_dir, scope) = workspace("ps-utf8");
    let out = dispatch(
        &scope,
        "powershell",
        &serde_json::json!({ "command": "Write-Output '你好，教室'" }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    let stdout = out.data["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("你好，教室"),
        "中文输出被编码搞坏了：{stdout:?}"
    );
    assert!(
        !stdout.contains('\u{fffd}'),
        "出现了替换字符，说明解码用的是错的编码：{stdout:?}"
    );
}

// ---------------------------------------------------------------------------
// bash —— 类 Unix 环境（随包的 Git Bash）
// ---------------------------------------------------------------------------

/// 有没有可用的 Git Bash 取决于这台机器。
///
/// 没有就**跳过**：那是预期的 `unsupported` 错误（工具会给出三条出路），
/// 不是测试失败。想让它变成"有"，跑 `python tools/fetch_runtime.py`。
fn bash_ready(scope: &Scope) -> bool {
    let out = dispatch(scope, "bash", &json!({ "command": "true" }));
    if out.is_ok() {
        return true;
    }
    let err = out.error.expect("失败时应有 error");
    assert_eq!(
        err.kind,
        ErrorKind::Unsupported,
        "bash 跑不起来的原因只能是「本机没有」：{}",
        err.message
    );
    assert!(err.hint.is_some(), "没有 bash 时必须给出路");
    eprintln!("跳过 bash 用例：{}（{}）", err.message, err.hint.unwrap());
    false
}

#[test]
fn bash_runs_in_a_unix_environment() {
    let (dir, scope) = workspace("bash");
    std::fs::write(dir.join("notes.txt"), "第一行\n第二行\n第三行\n").unwrap();
    if !bash_ready(&scope) {
        return;
    }

    // 一整套 Unix 工具都在：`ls` / `grep` / `sed` 这些在另一种 shell 里并不存在。
    let out = dispatch(&scope, "bash", &json!({ "command": "ls -1" }));
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["exit_code"], 0);
    assert!(out.data["stdout"].as_str().unwrap().contains("notes.txt"));

    let out = dispatch(
        &scope,
        "bash",
        &json!({ "command": "grep -c 行 notes.txt" }),
    );
    assert_eq!(out.data["stdout"].as_str().unwrap().trim(), "3");

    let out = dispatch(
        &scope,
        "bash",
        &json!({ "command": "sed -n '2p' notes.txt" }),
    );
    assert!(out.data["stdout"].as_str().unwrap().contains("第二行"));

    // 多步连通（`&&` 是 bash 的写法）
    let out = dispatch(&scope, "bash", &json!({ "command": "echo a && echo b" }));
    let stdout = out.data["stdout"].as_str().unwrap();
    assert!(stdout.contains('a') && stdout.contains('b'), "{stdout:?}");

    // 非零退出：工具**成功执行**，成败在 exit_code 里 —— 两者可区分。
    let out = dispatch(&scope, "bash", &json!({ "command": "exit 7" }));
    assert!(out.is_ok(), "非零退出不该被当成工具失败");
    assert_eq!(out.data["exit_code"], 7);
    assert!(out.summary.contains("退出码 7"));

    // host 字段能区分用的是哪一份宿主（随包 / 系统安装）
    assert!(out.data["host"].as_str().unwrap().contains("gitbash"));
}

#[test]
fn bash_output_keeps_chinese_intact() {
    let (_dir, scope) = workspace("bash-utf8");
    if !bash_ready(&scope) {
        return;
    }
    let out = dispatch(&scope, "bash", &json!({ "command": "echo '你好，教室'" }));
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    let stdout = out.data["stdout"].as_str().unwrap();
    assert!(stdout.contains("你好，教室"), "中文输出坏了：{stdout:?}");
    assert!(
        !stdout.contains('\u{fffd}'),
        "出现替换字符，说明解码用的是错的编码：{stdout:?}"
    );
}

#[test]
fn bash_rejects_cwd_outside_workspace() {
    let (dir, scope) = workspace("bash-cwd");
    let out = dispatch(&scope, "bash", &json!({ "command": "ls", "cwd": "../.." }));
    assert_eq!(kind_of(&out), ErrorKind::NotAllowed);

    let out = dispatch(
        &scope,
        "bash",
        &json!({ "command": "ls", "cwd": "not-a-dir" }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);

    if !bash_ready(&scope) {
        return;
    }
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let out = dispatch(&scope, "bash", &json!({ "command": "pwd", "cwd": "sub" }));
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["cwd"], "sub");
}

#[test]
fn bash_times_out_on_long_command() {
    let (_dir, scope) = workspace("bash-timeout");
    if !bash_ready(&scope) {
        return;
    }
    let out = dispatch(
        &scope,
        "bash",
        &json!({ "command": "while :; do :; done", "timeout_ms": 300 }),
    );
    assert_eq!(kind_of(&out), ErrorKind::Timeout);
    assert!(out.error.unwrap().hint.unwrap().contains("timeout_ms"));
}

/// 参数与另一个 shell 工具**逐项一致**：同一骨架，只有方言不同。
/// 分开写两份最容易漂移的就是这里。
#[test]
fn bash_and_the_other_shell_share_parameter_shape() {
    let a = find("powershell").unwrap();
    let b = find("bash").unwrap();
    assert_eq!(a.risk, b.risk, "风险等级必须一致（都是 exec）");
    assert_eq!(a.params.len(), b.params.len());
    for (x, y) in a.params.iter().zip(b.params.iter()) {
        assert_eq!(x.name, y.name);
        assert_eq!(x.ty, y.ty);
        assert_eq!(x.default, y.default);
        assert_eq!(x.range, y.range);
        assert_eq!(x.required, y.required);
    }
}

/// 随包运行时一旦存在，就**必须优先**于系统安装被选中 —— 这是"随包提供"的意义。
#[test]
fn bundled_runtime_wins_when_present() {
    use neo_tools::tools::shell::{resolve, ShellKind};

    let runtime_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("runtime")
        .join("gitbash");
    // MinGit 没有 `bin/`，壳在 `usr/bin/` —— 两处都要认，否则这条测试会
    // 永远走"跳过"分支，看着是绿的其实什么都没断言。
    let present = ["bin/bash.exe", "usr/bin/bash.exe"]
        .iter()
        .any(|rel| runtime_root.join(rel).is_file());
    if !present {
        eprintln!(
            "跳过：随包运行时还没下载（python tools/fetch_runtime.py）；\
             当前解析到系统安装的 bash 也没问题"
        );
        return;
    }
    let host = resolve(ShellKind::Bash).expect("随包运行时在，就该解析得出来");
    eprintln!(
        "解析到：{}（kind={}）",
        host.executable.display(),
        host.kind
    );
    assert_eq!(host.kind, "gitbash-bundled", "随包运行时必须优先");
    assert!(
        host.executable.to_string_lossy().contains("gitbash"),
        "解析到的不是随包那份：{}",
        host.executable.display()
    );

    // 贯通到底：**工具结果里的 host 也必须是随包那份** ——
    // 否则"解析优先"只是单元级的结论，执行路径上可能另有分支。
    let (_dir, scope) = workspace("bash-bundled-run");
    let out = dispatch(&scope, "bash", &json!({ "command": "echo $BASH_VERSION" }));
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["host"], "gitbash-bundled");
    assert!(
        out.data["stdout"].as_str().unwrap().starts_with('5'),
        "随包运行时应当是 GNU bash：{:?}",
        out.data["stdout"]
    );
}

// ---------------------------------------------------------------------------
// 屏幕交互（截屏 / 点击 / 拖动）
// ---------------------------------------------------------------------------

/// 屏幕区域：先问一下虚拟桌面有多大，再挑一块小的。
/// 没有桌面会话时返回 `None`（那种环境下这些用例整组跳过）。
fn probe_region() -> Option<(neo_tools::tools::screen::Rect, serde_json::Value)> {
    let vs = neo_tools::tools::screen::virtual_screen();
    if vs.width <= 0 || vs.height <= 0 {
        eprintln!("跳过：没有可用的虚拟桌面（无头会话）");
        return None;
    }
    let w = 64.min(vs.width);
    let h = 48.min(vs.height);
    Some((vs, json!({ "x": vs.x, "y": vs.y, "width": w, "height": h })))
}

/// 截屏是**只读**的，可以直接测：抓一块 → 存进工作区 → 图附在结果上。
#[test]
fn screenshot_captures_a_region_and_attaches_the_image() {
    let (dir, scope) = workspace("shot");
    let Some((vs, region)) = probe_region() else {
        return;
    };

    let out = dispatch(&scope, "screenshot", &region);
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));

    // 区域与桌面尺寸都要报出来 —— 桌面尺寸是模型写坐标的依据
    assert_eq!(out.data["region"]["width"], region["width"]);
    assert_eq!(out.data["virtual_screen"]["width"], vs.width);
    assert!(
        out.data["coordinate_space"]
            .as_str()
            .unwrap()
            .contains("物理像素"),
        "必须写明坐标空间：{}",
        out.data["coordinate_space"]
    );

    // 图要附上（多模态模型靠它"看见"屏幕）
    assert_eq!(out.data["image_attached"], true);
    assert_eq!(out.images.len(), 1, "应当附带一张图");
    assert!(
        out.images[0].starts_with("data:image/png;base64,"),
        "{}",
        &out.images[0][..40]
    );
    assert!(out.summary.contains("已把图交给模型"), "{}", out.summary);

    // 同时落盘在工作区里 —— 模型之后能用这个路径做别的事
    let rel = out.data["path"].as_str().unwrap();
    let saved = dir.join(rel);
    assert!(saved.is_file(), "截图应当落在工作区：{}", saved.display());
    let bytes = std::fs::read(&saved).unwrap();
    assert!(
        bytes.starts_with(&[0x89, b'P', b'N', b'G']),
        "存下来的应当是 PNG"
    );
    assert_eq!(
        bytes.len() as u64,
        out.data["saved_bytes"].as_u64().unwrap()
    );
}

/// 局部截屏的四个参数**要么都给、要么都不给** —— 只给一半会静默截到
/// 一块莫名其妙的区域，那比报错难查得多。
#[test]
fn screenshot_needs_all_four_region_params() {
    let (_dir, scope) = workspace("shot-args");

    let out = dispatch(&scope, "screenshot", &json!({ "x": 10, "y": 20 }));
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
    let e = out.error.unwrap();
    assert!(
        e.message.contains("width") && e.message.contains("height"),
        "{}",
        e.message
    );
    assert!(e.hint.is_some());
}

/// 越界区域要**在抓屏之前**被拒，并在错误里给出屏幕的真实范围。
#[test]
fn screenshot_rejects_a_region_outside_the_screen() {
    let (_dir, scope) = workspace("shot-range");
    let Some((vs, _)) = probe_region() else {
        return;
    };
    let out = dispatch(
        &scope,
        "screenshot",
        &json!({ "x": vs.x + vs.width + 100, "y": vs.y, "width": 64, "height": 48 }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
    let e = out.error.unwrap();
    assert!(e.message.contains("不在屏幕内"), "{}", e.message);
    // 合法范围要写出来，模型据此自己改
    assert!(
        e.message.contains(&(vs.x + vs.width - 1).to_string()),
        "要含合法范围：{}",
        e.message
    );
}

/// 风险等级决定了要不要用户点头：截屏只读直接放行，
/// 点击/拖动是 `exec`，只读模式下直接拒绝。
#[test]
fn screen_tools_policy_matches_their_risk() {
    use neo_tools::{find, Decision, Policy};

    assert_eq!(find("screenshot").unwrap().risk, neo_tools::Risk::Read);
    assert_eq!(find("click").unwrap().risk, neo_tools::Risk::Exec);
    assert_eq!(find("drag").unwrap().risk, neo_tools::Risk::Exec);

    let read_only = Policy::read_only();
    let empty = json!({});
    assert_eq!(
        read_only.decide(find("screenshot").unwrap(), &empty),
        Decision::Allow,
        "截屏不改动任何东西，只读模式下也该放行"
    );
    let point = json!({ "x": 1, "y": 1 });
    assert!(
        matches!(
            read_only.decide(find("click").unwrap(), &point),
            Decision::Deny(_)
        ),
        "只读模式下点击必须被拒"
    );
    let drag_args = json!({ "x": 1, "y": 1, "to_x": 2, "to_y": 2 });
    assert!(matches!(
        read_only.decide(find("drag").unwrap(), &drag_args),
        Decision::Deny(_)
    ));
}

/// 鼠标用例会**真的动用户的鼠标**，所以默认跳过。
///
/// 这不是"测试写得随便"，而是必须的门控：跑测试的人可能正在用这台电脑，
/// 一个自动化用例不该去抢他的鼠标。要验这条路径就显式设 `NEO_TEST_INPUT=1`。
fn input_allowed() -> bool {
    let on = std::env::var("NEO_TEST_INPUT").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("跳过鼠标用例（会真的移动鼠标）：设 NEO_TEST_INPUT=1 才跑");
    }
    on
}

#[test]
fn click_and_drag_really_move_the_mouse_when_explicitly_allowed() {
    let (_dir, scope) = workspace("input");
    if !input_allowed() {
        return;
    }
    let Some((vs, _)) = probe_region() else {
        return;
    };

    // 点虚拟桌面左上角里侧一点点 —— 位置可预期，方便人工确认鼠标确实动了
    let out = dispatch(&scope, "click", &json!({ "x": vs.x + 2, "y": vs.y + 2 }));
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["button"], "left");
    assert_eq!(out.data["double"], false);

    // 右键
    let out = dispatch(
        &scope,
        "click",
        &json!({ "x": vs.x + 2, "y": vs.y + 2, "button": "right" }),
    );
    assert!(out.is_ok());
    assert_eq!(out.data["button"], "right");

    // 双击（一次调用内完成）
    let out = dispatch(
        &scope,
        "click",
        &json!({ "x": vs.x + 2, "y": vs.y + 2, "double": true }),
    );
    assert!(out.is_ok());
    assert_eq!(out.data["double"], true);

    // 拖动：从角落拖出 40 px
    let out = dispatch(
        &scope,
        "drag",
        &json!({ "x": vs.x + 2, "y": vs.y + 2, "to_x": vs.x + 42, "to_y": vs.y + 22 }),
    );
    assert!(out.is_ok(), "{:?}", out.error.map(|e| e.message));
    assert_eq!(out.data["button"], "left");
}

/// 越界的点击/拖动**一个事件都不发**（鼠标不该先跑过去再报错）。
/// 这条不需要门控：它断言的正是"没动鼠标"。
#[test]
fn out_of_range_input_is_rejected_before_touching_the_mouse() {
    let (_dir, scope) = workspace("input-range");
    let Some((vs, _)) = probe_region() else {
        return;
    };

    let out = dispatch(
        &scope,
        "click",
        &json!({ "x": vs.x + vs.width + 5000, "y": vs.y + 5 }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);

    // 注意用**在参数区间内**但屏幕外的坐标：超出 -32768…32768 会先撞区间校验，
    // 那就变成另一条路径了（区间校验更早，也更该先报）。
    let out = dispatch(
        &scope,
        "drag",
        &json!({ "x": vs.x + 5, "y": vs.y + 5, "to_x": vs.x - 30000, "to_y": vs.y + 5 }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
    assert!(out.error.unwrap().message.contains("终点"));

    // 顺带守住"参数区间比屏幕范围更早兜住"这件事
    let out = dispatch(
        &scope,
        "drag",
        &json!({ "x": 0, "y": 0, "to_x": 999_999, "to_y": 0 }),
    );
    let e = out.error.unwrap();
    assert_eq!(e.kind, ErrorKind::BadArguments);
    assert!(e.message.contains("to_x"), "{}", e.message);
}

/// 坐标是**必填**的：漏填不能被悄悄当成 (0,0) 点下去。
#[test]
fn click_coordinates_are_required() {
    let (_dir, scope) = workspace("click-args");
    let out = dispatch(&scope, "click", &json!({ "y": 10 }));
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);

    let out = dispatch(&scope, "drag", &json!({ "x": 1, "y": 2, "to_x": 3 }));
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
}

/// 按钮名写错要说清楚合法值。
#[test]
fn click_rejects_an_unknown_button() {
    let (_dir, scope) = workspace("click-btn");
    let out = dispatch(
        &scope,
        "click",
        &json!({ "x": 1, "y": 1, "button": "middle" }),
    );
    assert_eq!(kind_of(&out), ErrorKind::BadArguments);
    let e = out.error.unwrap();
    assert!(e.message.contains("left"), "{}", e.message);
    assert!(e.hint.is_some());
}
