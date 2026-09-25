use std::{
    io::{Cursor, Write},
    path::PathBuf,
};

use neo_tools::{
    dispatch, documents, find, to_model_message, Args, Decision, ErrorKind, Policy, Scope,
};
use serde_json::{json, Value};

fn workspace(tag: &str) -> (PathBuf, Scope) {
    let dir = std::env::temp_dir().join(format!("neo-document-it-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let scope = Scope::new(&dir);
    (dir, scope)
}

fn zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in parts {
        writer.start_file(*name, options).unwrap();
        writer.write_all(content).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn docx(xml: &[u8]) -> Vec<u8> {
    zip(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("word/document.xml", xml),
    ])
}

fn cfb(streams: &[(&str, &[u8])]) -> Vec<u8> {
    let mut file = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
    for (name, data) in streams {
        file.create_stream(name).unwrap().write_all(data).unwrap();
    }
    file.into_inner().into_inner()
}

fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn doc(text: &str) -> Vec<u8> {
    let body = wide(text);
    let chars = (body.len() / 2) as u32;
    let mut word = vec![0; 512];
    word.extend_from_slice(&body);
    word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
    word[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
    word[32..34].copy_from_slice(&14u16.to_le_bytes());
    word[62..64].copy_from_slice(&22u16.to_le_bytes());
    word[76..80].copy_from_slice(&chars.to_le_bytes());
    word[152..154].copy_from_slice(&34u16.to_le_bytes());
    let mut plc = Vec::new();
    for cp in [0u32, chars] {
        plc.extend_from_slice(&cp.to_le_bytes());
    }
    plc.extend_from_slice(&[0; 2]);
    plc.extend_from_slice(&512u32.to_le_bytes());
    plc.extend_from_slice(&[0; 2]);
    let mut clx = vec![2];
    clx.extend_from_slice(&(plc.len() as u32).to_le_bytes());
    clx.extend_from_slice(&plc);
    let pair = 154 + 33 * 8;
    word[pair + 4..pair + 8].copy_from_slice(&(clx.len() as u32).to_le_bytes());
    cfb(&[("/WordDocument", &word), ("/0Table", &clx)])
}

fn ppt(text: &str) -> Vec<u8> {
    let body = wide(text);
    let mut record = Vec::new();
    record.extend_from_slice(&0u16.to_le_bytes());
    record.extend_from_slice(&4000u16.to_le_bytes());
    record.extend_from_slice(&(body.len() as u32).to_le_bytes());
    record.extend_from_slice(&body);
    cfb(&[("/PowerPoint Document", &record)])
}

fn collect_pages(scope: &Scope, path: &str, limit: usize) -> String {
    let mut content = String::new();
    let mut offset = 0;
    loop {
        let out = dispatch(
            scope,
            "read_document",
            &json!({"path": path, "offset": offset, "limit": limit}),
        );
        assert!(out.is_ok(), "{:?}", out.error);
        assert!(out.images.is_empty());
        let encoded = to_model_message(&out);
        assert!(encoded.chars().count() <= neo_tools::tools::limits::MODEL_JSON_CHARS);
        assert!(encoded.len() <= 32 * 1024);
        let page: Value = serde_json::from_str(&encoded).unwrap();
        assert!(
            page.get("truncated").is_none(),
            "结构化分页不能被通用截断吞掉"
        );
        assert_eq!(page["data"], out.data);
        let data = &page["data"];
        assert_eq!(data["offset"], offset);
        let part = data["content"].as_str().unwrap();
        let count = part.chars().count();
        assert!(count <= limit);
        content.push_str(part);
        offset += count;
        if data["has_more"] == false {
            assert!(data["next_offset"].is_null());
            assert_eq!(data["total_chars"], offset);
            break;
        }
        assert!(count > 0, "续页必须前进");
        assert_eq!(data["next_offset"], offset);
    }
    content
}

#[test]
fn dispatch_docx_reads_file_and_paginates_unicode() {
    let (dir, scope) = workspace("docx");
    let text = "中文𠀀 & 正文\n第二段";
    let xml = "<w:document xmlns:w='urn:w'><w:body><w:p><w:r><w:t>中文𠀀 &amp; 正文</w:t></w:r></w:p><w:p><w:r><w:t>第二段</w:t></w:r></w:p></w:body></w:document>";
    std::fs::write(dir.join("lesson.DOCX"), docx(xml.as_bytes())).unwrap();
    assert_eq!(collect_pages(&scope, "lesson.DOCX", 3), text);
    let absolute = dispatch(
        &scope,
        "read_document",
        &json!({"path": dir.join("lesson.DOCX").to_string_lossy()}),
    );
    assert!(absolute.is_ok(), "{:?}", absolute.error);
    assert_eq!(absolute.data["content"], text);
    let out = dispatch(&scope, "read_document", &json!({"path": "lesson.DOCX"}));
    assert_eq!(out.data["name"], "lesson.DOCX");
    assert_eq!(out.data["type"], "document");
    assert!(out.data["warning"].as_str().unwrap().contains("图片"));
    assert_eq!(
        documents::load(&dir.join("lesson.DOCX")).unwrap().text,
        text
    );
}

#[test]
fn dispatch_pptx_keeps_slide_order_and_pagination() {
    let (dir, scope) = workspace("pptx");
    let data = zip(&[
        ("[Content_Types].xml", b"<Types/>"),
        ("ppt/presentation.xml", b"<p:presentation xmlns:p='p' xmlns:r='r'><p:sldIdLst><p:sldId r:id='second'/><p:sldId r:id='first'/></p:sldIdLst></p:presentation>"),
        ("ppt/_rels/presentation.xml.rels", b"<Relationships><Relationship Id='first' Type='urn:office/slide' Target='slides/slide1.xml'/><Relationship Id='second' Type='urn:office/slide' Target='slides/slide2.xml'/></Relationships>"),
        ("ppt/slides/slide1.xml", "<s><p><t>第二页表格</t></p></s>".as_bytes()),
        ("ppt/slides/slide2.xml", "<s><p><t>第一页正文</t></p></s>".as_bytes()),
    ]);
    std::fs::write(dir.join("lesson.pptx"), data).unwrap();
    assert_eq!(
        collect_pages(&scope, "lesson.pptx", 7),
        "--- 幻灯片 1 ---\n第一页正文\n\n--- 幻灯片 2 ---\n第二页表格"
    );
    let out = dispatch(&scope, "read_document", &json!({"path": "lesson.pptx"}));
    assert_eq!(out.data["type"], "presentation");
    assert!(out.data["warning"].as_str().unwrap().contains("备注"));
}

#[test]
fn dispatch_legacy_doc_and_ppt_read_real_cfb_files() {
    let (dir, scope) = workspace("legacy");
    let text = "旧版文档𠀀正文\r第二段";
    for (name, data, kind) in [
        ("old.doc", doc(text), "document"),
        ("old.ppt", ppt(text), "presentation"),
    ] {
        std::fs::write(dir.join(name), data).unwrap();
        assert_eq!(collect_pages(&scope, name, 4), text.replace('\r', "\n"));
        let out = dispatch(&scope, "read_document", &json!({"path": name}));
        assert_eq!(out.data["type"], kind);
        assert!(out.data["warning"].as_str().unwrap().contains("旧版"));
    }
}

#[test]
fn dispatch_text_defaults_encodings_and_end_offsets() {
    let (dir, scope) = workspace("text");
    std::fs::write(dir.join("long.txt"), "中".repeat(5001)).unwrap();
    let first = dispatch(&scope, "read_document", &json!({"path": "long.txt"}));
    assert_eq!(
        first.data["content"].as_str().unwrap().chars().count(),
        4000
    );
    assert_eq!(first.data["next_offset"], 4000);
    assert_eq!(first.data["type"], "text");
    assert_eq!(first.data["warning"], Value::Null);
    let end = dispatch(
        &scope,
        "read_document",
        &json!({"path": "long.txt", "offset": 6000}),
    );
    assert_eq!(end.data["offset"], 5001);
    assert_eq!(end.data["content"], "");
    assert_eq!(end.data["has_more"], false);
    assert!(end.data["next_offset"].is_null());
    let mut utf16 = vec![0xFF, 0xFE];
    utf16.extend(wide("中文𠀀正文"));
    std::fs::write(dir.join("utf16.txt"), utf16).unwrap();
    assert_eq!(collect_pages(&scope, "utf16.txt", 2), "中文𠀀正文");
    std::fs::write(dir.join("gbk.txt"), encoding_rs::GBK.encode("中文正文").0).unwrap();
    assert_eq!(collect_pages(&scope, "gbk.txt", 2), "中文正文");
}

#[test]
fn model_json_budget_preserves_chinese_escape_sequences_and_continuations() {
    let (dir, scope) = workspace("budget");
    for (name, text) in [
        ("chinese.txt", "中文正文".repeat(4000)),
        ("wide.txt", "𠀀".repeat(16_000)),
        ("escaped.txt", format!("始{}终", "\u{c}".repeat(16_000))),
        ("quoted.txt", "\\\"\t\n".repeat(4000)),
    ] {
        std::fs::write(dir.join(name), &text).unwrap();
        assert_eq!(collect_pages(&scope, name, 8000), text.trim());
    }
}

#[test]
fn model_json_shrinks_a_page_before_losing_pagination_fields() {
    let (dir, scope) = workspace("budget-shrink");
    let name = format!("{}.docx", "文".repeat(82));
    let text = "𠀀".repeat(16_000);
    std::fs::write(
        dir.join(&name),
        docx(format!("<x><t>{text}</t></x>").as_bytes()),
    )
    .unwrap();
    let first = dispatch(
        &scope,
        "read_document",
        &json!({"path": name, "limit": 8000}),
    );
    assert!(first.is_ok(), "{:?}", first.error);
    let returned = first.data["content"].as_str().unwrap().chars().count();
    assert!(
        returned > 0 && returned < 8000,
        "文件名及四字节文字应触发预算缩页"
    );
    assert_eq!(first.data["next_offset"], returned);
    assert_eq!(collect_pages(&scope, &name, 8000), text);
}

#[test]
fn extraction_truncation_never_claims_to_be_full_document() {
    let (dir, scope) = workspace("truncated");
    std::fs::write(dir.join("truncated.txt"), "中".repeat(80_001)).unwrap();
    let last = dispatch(
        &scope,
        "read_document",
        &json!({"path": "truncated.txt", "offset": 79_999}),
    );
    assert!(last.is_ok());
    assert_eq!(last.data["content"], "中");
    assert_eq!(last.data["total_chars"], 80_000);
    assert_eq!(last.data["has_more"], false);
    let warning = last.data["warning"].as_str().unwrap();
    assert!(warning.contains("不是全文") && warning.contains("分页结束不代表原文件结束"));
}

#[test]
fn document_schema_preview_policy_and_bad_arguments_are_consistent() {
    let (_dir, scope) = workspace("args");
    let tool = find("read_document").unwrap();
    assert_eq!(tool.risk, neo_tools::Risk::Read);
    let schema = tool.schema();
    assert_eq!(schema["required"], json!(["path"]));
    assert_eq!(schema["properties"]["offset"]["default"], 0);
    assert_eq!(schema["properties"]["limit"]["default"], 4000);
    assert_eq!(schema["properties"]["limit"]["maximum"], 8000);
    assert_eq!(schema["additionalProperties"], false);
    let value = json!({"path": "lesson.pptx", "offset": 4000, "limit": 3000});
    let preview = (tool.preview)(&Args::new(tool, &value));
    assert!(
        preview.contains("lesson.pptx") && preview.contains("4000") && preview.contains("3000")
    );
    assert_eq!(Policy::read_only().decide(tool, &value), Decision::Allow);
    for args in [
        json!({}),
        json!({"path": "a.txt", "offset": -1}),
        json!({"path": "a.txt", "offset": 80_001}),
        json!({"path": "a.txt", "limit": 0}),
        json!({"path": "a.txt", "limit": 8001}),
        json!({"path": "a.txt", "limit": "2"}),
        json!({"path": "a.txt", "unknown": true}),
    ] {
        assert_eq!(
            dispatch(&scope, "read_document", &args).error.unwrap().kind,
            ErrorKind::BadArguments
        );
    }
}

#[test]
fn document_failure_json_is_bounded_and_keeps_structured_error() {
    let (_, scope) = workspace("failure-budget");
    for path in [
        "中".repeat(50_000),
        "\u{c}".repeat(50_000),
        format!("../../{}", "x".repeat(50_000)),
    ] {
        let out = dispatch(&scope, "read_document", &json!({"path": path}));
        assert!(!out.is_ok());
        let encoded = to_model_message(&out);
        assert!(encoded.len() <= 32 * 1024);
        assert!(encoded.chars().count() <= neo_tools::tools::limits::MODEL_JSON_CHARS);
        let page: Value = serde_json::from_str(&encoded).unwrap();
        assert!(page.get("truncated").is_none());
        assert!(page["error"]["kind"].is_string());
        assert!(page["error"]["message"].is_string());
    }
}

#[test]
fn document_file_errors_and_image_guidance_are_specific() {
    let (dir, scope) = workspace("errors");
    assert_eq!(
        dispatch(&scope, "read_document", &json!({"path": "missing.docx"}))
            .error
            .unwrap()
            .kind,
        ErrorKind::NotFound
    );
    assert_eq!(
        dispatch(&scope, "read_document", &json!({"path": "."}))
            .error
            .unwrap()
            .kind,
        ErrorKind::Unsupported
    );
    for (name, data) in [
        ("empty.txt", b"".as_slice()),
        ("binary.txt", b"a\0b"),
        ("bad.docx", b"bad"),
        ("bad.pptx", b"bad"),
        ("bad.doc", b"bad"),
        ("bad.ppt", b"bad"),
        ("bad.exe", b"MZ"),
    ] {
        std::fs::write(dir.join(name), data).unwrap();
        assert_eq!(
            dispatch(&scope, "read_document", &json!({"path": name}))
                .error
                .unwrap()
                .kind,
            ErrorKind::Unsupported,
            "{name}"
        );
    }
    std::fs::write(dir.join("image.png"), b"not decoded").unwrap();
    let image = dispatch(&scope, "read_document", &json!({"path": "image.png"}));
    assert_eq!(image.error.as_ref().unwrap().kind, ErrorKind::Unsupported);
    assert!(image.error.unwrap().hint.unwrap().contains("view_image"));
    let binary = dispatch(&scope, "read_file", &json!({"path": "bad.docx"}));
    assert!(binary
        .error
        .unwrap()
        .hint
        .unwrap()
        .contains("read_document"));
}

#[test]
fn document_rejects_outside_paths_and_oversize_file() {
    let (dir, scope) = workspace("scope");
    for path in [
        "../secret.docx".to_owned(),
        std::env::temp_dir()
            .join("outside.docx")
            .to_string_lossy()
            .into_owned(),
    ] {
        assert_eq!(
            dispatch(&scope, "read_document", &json!({"path": path}))
                .error
                .unwrap()
                .kind,
            ErrorKind::NotAllowed
        );
    }
    let large = std::fs::File::create(dir.join("large.docx")).unwrap();
    large.set_len(documents::MAX_FILE_BYTES + 1).unwrap();
    let result = dispatch(&scope, "read_document", &json!({"path": "large.docx"}));
    assert_eq!(result.error.unwrap().kind, ErrorKind::TooLarge);
}

#[test]
fn document_rejects_unsafe_xml_packages_and_large_expansion() {
    let (dir, scope) = workspace("packages");
    for (name, bytes) in [
        (
            "dtd.docx",
            docx(b"<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///secret'>]><x><t>&a;</t></x>"),
        ),
        ("broken.docx", docx(b"<x><t>bad</x>")),
        (
            "traversal.docx",
            zip(&[("[Content_Types].xml", b"<Types/>"), ("../escape", b"bad")]),
        ),
    ] {
        std::fs::write(dir.join(name), bytes).unwrap();
        assert!(!dispatch(&scope, "read_document", &json!({"path": name})).is_ok());
    }
    std::fs::write(
        dir.join("huge.docx"),
        docx(&vec![b' '; 16 * 1024 * 1024 + 1]),
    )
    .unwrap();
    assert_eq!(
        dispatch(&scope, "read_document", &json!({"path": "huge.docx"}))
            .error
            .unwrap()
            .kind,
        ErrorKind::TooLarge
    );
    let mut damaged = docx(b"<x><t>text</t></x>");
    damaged.truncate(damaged.len() - 10);
    std::fs::write(dir.join("damaged.docx"), damaged).unwrap();
    assert!(!dispatch(&scope, "read_document", &json!({"path": "damaged.docx"})).is_ok());
}

#[test]
fn document_rejects_links_to_outside_workspace() {
    let (dir, scope) = workspace("link");
    let (outside, _outside_scope) = workspace("link-outside");
    std::fs::write(outside.join("secret.txt"), "不能读取").unwrap();
    let link = dir.join("linked");
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "创建测试目录联接失败：{:?}",
            output
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let out = dispatch(
        &scope,
        "read_document",
        &json!({"path": "linked/secret.txt"}),
    );
    assert_eq!(out.error.unwrap().kind, ErrorKind::NotAllowed);
    #[cfg(windows)]
    std::fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    std::fs::remove_file(&link).unwrap();
}
