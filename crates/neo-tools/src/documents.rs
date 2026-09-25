use base64::{engine::general_purpose::STANDARD, Engine};
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

pub const MAX_FILES: usize = 8;
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_TEXT_CHARS: usize = 80_000;
const MAX_ZIP_ENTRIES: usize = 4096;
const MAX_UNPACKED_BYTES: u64 = 128 * 1024 * 1024;
const MAX_XML_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_EVENTS: usize = 2_000_000;
const MAX_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_IMAGE_SIDE: u32 = 8192;
const VISION_IMAGE_SIDE: u32 = 1536;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub text: String,
    pub image_url: Option<String>,
    pub warning: Option<String>,
}

pub fn load(path: &Path) -> Result<Attachment, String> {
    let file = File::open(path).map_err(|e| format!("无法打开附件：{e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("无法读取附件信息：{e}"))?;
    if !metadata.is_file() {
        return Err("附件必须是普通文件".into());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err("附件超过 32 MiB 上限".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取附件失败：{e}"))?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    parse(&name, &extension, &bytes)
}

fn parse(name: &str, extension: &str, data: &[u8]) -> Result<Attachment, String> {
    if data.len() as u64 > MAX_FILE_BYTES {
        return Err("附件超过 32 MiB 上限".into());
    }
    if data.is_empty() {
        return Err("附件内容为空".into());
    }
    let mut attachment = Attachment {
        name: name.to_owned(),
        kind: String::new(),
        bytes: data.len() as u64,
        text: String::new(),
        image_url: None,
        warning: None,
    };
    let mut text = Text::default();
    match extension {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" => {
            return parse_image(attachment, data);
        }
        "docx" => {
            attachment.kind = "document".into();
            let mut package = Package::new(data)?;
            let xml = package.xml("word/document.xml")?;
            extract_xml_text(&xml, &mut text)?;
            attachment.warning =
                Some("仅提取正文和表格文字；未识别嵌入图片、页眉页脚及批注。".into());
        }
        "pptx" => {
            attachment.kind = "presentation".into();
            extract_pptx(data, &mut text)?;
            attachment.warning =
                Some("按幻灯片顺序提取文本和表格；不含图片、图表渲染及演讲者备注。".into());
        }
        "doc" => {
            attachment.kind = "document".into();
            extract_doc(data, &mut text)?;
            attachment.warning =
                Some("旧版 Word：仅提取正文文字，不含图片、页眉页脚、脚注及排版。".into());
        }
        "ppt" => {
            attachment.kind = "presentation".into();
            extract_ppt(data, &mut text)?;
            attachment.warning = Some("旧版 PowerPoint：按文件记录顺序提取文字，可能包含历史保存记录及备注，顺序不一定等同放映顺序；需要精确内容请另存为 PPTX。".into());
        }
        "txt" | "md" | "csv" | "json" | "log" | "tsv" => {
            attachment.kind = "text".into();
            let (decoded, warning) = decode_text(data)?;
            text.push(&decoded);
            attachment.warning = warning;
        }
        _ => {
            return Err(format!(
                "暂不支持 .{extension} 附件；支持图片、DOC/DOCX、PPT/PPTX 和文本文件"
            ))
        }
    }
    if text.value.trim().is_empty() {
        return Err("未找到可提取文字；扫描件、纯图片文档或加密文件请另存为图片/可编辑文档".into());
    }
    if text.truncated {
        append_warning(
            &mut attachment.warning,
            "文字超过 80,000 字符，已截断，不是全文；total_chars 仅指已提取部分，分页结束不代表原文件结束。请拆分文件以读取剩余内容。",
        );
    }
    attachment.text = text.value.trim().to_owned();
    Ok(attachment)
}

fn append_warning(warning: &mut Option<String>, message: &str) {
    match warning {
        Some(existing) => {
            existing.push(' ');
            existing.push_str(message);
        }
        None => *warning = Some(message.to_owned()),
    }
}

#[derive(Default)]
struct Text {
    value: String,
    chars: usize,
    truncated: bool,
}

impl Text {
    fn push(&mut self, value: &str) {
        let remaining = MAX_TEXT_CHARS - self.chars;
        let mut count = 0;
        for ch in value.chars() {
            if count == remaining {
                self.truncated = true;
                break;
            }
            self.value.push(ch);
            count += 1;
        }
        self.chars += count;
    }

    fn newline(&mut self) {
        if !self.value.ends_with('\n') && !self.value.is_empty() {
            self.push("\n");
        }
    }
}

fn parse_image(mut attachment: Attachment, data: &[u8]) -> Result<Attachment, String> {
    let format = image::guess_format(data).map_err(|e| format!("无法识别图片：{e}"))?;
    if !matches!(
        format,
        image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif
            | image::ImageFormat::Bmp
    ) {
        return Err("图片内容不是支持的 PNG/JPEG/WebP/GIF/BMP 格式".into());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(128 * 1024 * 1024);
    let mut dimensions_reader = image::ImageReader::with_format(Cursor::new(data), format);
    dimensions_reader.limits(limits.clone());
    let (width, height) = dimensions_reader
        .into_dimensions()
        .map_err(|e| format!("图片头损坏：{e}"))?;
    check_image_dimensions(width, height)?;
    let mut reader = image::ImageReader::with_format(Cursor::new(data), format);
    reader.limits(limits);
    let mut image = reader
        .decode()
        .map_err(|e| format!("图片解码失败或超过资源上限：{e}"))?;
    if width > VISION_IMAGE_SIDE || height > VISION_IMAGE_SIDE {
        image = image.thumbnail(VISION_IMAGE_SIDE, VISION_IMAGE_SIDE);
        append_warning(
            &mut attachment.warning,
            "图片已等比缩小至最长边 1536 像素，原文件不变。",
        );
    }
    if matches!(
        format,
        image::ImageFormat::Gif | image::ImageFormat::WebP | image::ImageFormat::Png
    ) {
        append_warning(&mut attachment.warning, "如为动图，仅发送首帧。");
    }
    let mut encoded = Cursor::new(Vec::new());
    let mime = if format == image::ImageFormat::Jpeg {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 85)
            .encode_image(&image.to_rgb8())
            .map_err(|e| format!("图片转换失败：{e}"))?;
        "image/jpeg"
    } else {
        image
            .write_to(&mut encoded, image::ImageFormat::Png)
            .map_err(|e| format!("图片转换失败：{e}"))?;
        "image/png"
    };
    if encoded.get_ref().len() > 4 * 1024 * 1024 {
        return Err("图片转换结果过大，请先缩小图片".into());
    }
    attachment.kind = "image".into();
    attachment.text = format!("图片：{width} × {height} 像素（通过视觉附件发送，不包含 OCR 文本）");
    attachment.image_url = Some(format!(
        "data:{mime};base64,{}",
        STANDARD.encode(encoded.get_ref())
    ));
    Ok(attachment)
}

fn check_image_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_IMAGE_SIDE
        || height > MAX_IMAGE_SIDE
        || u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS
    {
        return Err("图片超过上限：单边 8192 像素、总计 1600 万像素".into());
    }
    Ok(())
}

fn decode_text(data: &[u8]) -> Result<(String, Option<String>), String> {
    let decoded = if let Some(data) = data.strip_prefix(&[0xFF, 0xFE]) {
        (utf16(data, true)?, None)
    } else if let Some(data) = data.strip_prefix(&[0xFE, 0xFF]) {
        (utf16(data, false)?, None)
    } else {
        let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
        match std::str::from_utf8(data) {
            Ok(text) => (text.to_owned(), None),
            Err(_) => {
                let (text, errors) = encoding_rs::GBK.decode_without_bom_handling(data);
                if errors {
                    return Err("文本编码无法识别，请另存为 UTF-8 或带 BOM 的 UTF-16".into());
                }
                (
                    text.into_owned(),
                    Some("文本不是 UTF-8，已按 GBK 解码；若出现乱码请另存为 UTF-8。".into()),
                )
            }
        }
    };
    if decoded
        .0
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t' | '\u{c}'))
    {
        return Err("文件包含二进制控制字符，不是支持的纯文本".into());
    }
    Ok(decoded)
}

fn utf16(data: &[u8], little_endian: bool) -> Result<String, String> {
    if !data.len().is_multiple_of(2) {
        return Err("UTF-16 数据长度损坏".into());
    }
    let units = data.chunks_exact(2).map(|pair| {
        if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        }
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|_| "UTF-16 文本包含损坏的代理项".into())
}

fn validate_zip_directory(data: &[u8]) -> Result<(), String> {
    let start = data.len().saturating_sub(65_557);
    let end = data[start..]
        .windows(4)
        .rposition(|signature| signature == b"PK\x05\x06")
        .map(|offset| start + offset)
        .ok_or("Office ZIP 缺少中央目录")?;
    let footer = range(data, end, 22)?;
    if end >= 20 && range(data, end - 20, 4)? == b"PK\x06\x07" {
        return Err("不支持 ZIP64 Office 附件".into());
    }
    if end + 22 + u16_at(footer, 20)? as usize != data.len() {
        return Err("Office ZIP 末尾记录损坏".into());
    }
    let count = u16_at(footer, 10)? as usize;
    if u16_at(footer, 4)? != 0 || u16_at(footer, 6)? != 0 || u16_at(footer, 8)? as usize != count {
        return Err("不支持分卷 Office ZIP".into());
    }
    if count > MAX_ZIP_ENTRIES {
        return Err("Office ZIP 条目超过 4096 上限".into());
    }
    let size = u32_at(footer, 12)? as usize;
    let offset = u32_at(footer, 16)? as usize;
    if size > data.len() || size < count * 46 || offset.checked_add(size) != Some(end) {
        return Err("Office ZIP 中央目录长度或偏移无效（不支持 ZIP64）".into());
    }
    Ok(())
}

struct Package<'a> {
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    read_bytes: u64,
}

impl<'a> Package<'a> {
    fn new(data: &'a [u8]) -> Result<Self, String> {
        if data.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]) {
            return Err(
                "文档可能已加密，或文件扩展名与实际格式不符；请另存为未加密的 DOCX/PPTX".into(),
            );
        }
        validate_zip_directory(data)?;
        let mut zip = zip::ZipArchive::new(Cursor::new(data))
            .map_err(|e| format!("Office ZIP 容器损坏：{e}"))?;
        if zip.len() > MAX_ZIP_ENTRIES {
            return Err("Office ZIP 条目超过 4096 上限".into());
        }
        let mut total = 0u64;
        let mut names = HashSet::new();
        for index in 0..zip.len() {
            let entry = zip
                .by_index(index)
                .map_err(|e| format!("ZIP 条目损坏或加密：{e}"))?;
            let name = entry.name();
            if name.len() > 512
                || name.contains('\\')
                || name.starts_with('/')
                || name.split('/').any(|part| part == ".." || part == ".")
                || !names.insert(name.to_owned())
            {
                return Err("Office ZIP 含无效或重复的条目路径".into());
            }
            if !matches!(
                entry.compression(),
                zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
            ) {
                return Err("Office ZIP 使用不支持的压缩算法".into());
            }
            total = total.checked_add(entry.size()).ok_or("ZIP 解压大小溢出")?;
            if total > MAX_UNPACKED_BYTES {
                return Err("Office ZIP 解压总量超过 128 MiB 上限".into());
            }
        }
        if !names.contains("[Content_Types].xml") {
            return Err("文件不是有效的 Office Open XML 文档".into());
        }
        Ok(Self { zip, read_bytes: 0 })
    }

    fn xml(&mut self, name: &str) -> Result<Vec<u8>, String> {
        let mut entry = self
            .zip
            .by_name(name)
            .map_err(|e| format!("缺少或无法读取 {name}：{e}"))?;
        if entry.size() > MAX_XML_BYTES {
            return Err(format!("{name} 超过单个 XML 16 MiB 上限"));
        }
        let remaining = MAX_UNPACKED_BYTES
            .saturating_sub(self.read_bytes)
            .min(MAX_XML_BYTES);
        let mut data = Vec::new();
        (&mut entry)
            .take(remaining + 1)
            .read_to_end(&mut data)
            .map_err(|e| format!("解压或校验 {name} 失败：{e}"))?;
        if data.len() as u64 > remaining {
            return Err("Office ZIP 实际解压量超过资源上限".into());
        }
        if data.len() as u64 != entry.size() {
            return Err(format!("{name} 实际长度与 ZIP 记录不符"));
        }
        self.read_bytes += data.len() as u64;
        Ok(data)
    }
}

fn walk_xml(
    data: &[u8],
    mut visit: impl FnMut(Event<'_>) -> Result<(), String>,
) -> Result<(), String> {
    if data.len() as u64 > MAX_XML_BYTES {
        return Err("XML 超过资源上限".into());
    }
    let mut reader = Reader::from_reader(data);
    reader.config_mut().check_end_names = true;
    let mut depth = 0usize;
    let mut roots = 0;
    for _ in 0..MAX_EVENTS {
        let event = reader.read_event().map_err(|e| format!("XML 损坏：{e}"))?;
        if let Event::Start(tag) | Event::Empty(tag) = &event {
            for (index, attribute) in tag.attributes().enumerate() {
                if index >= 128 {
                    return Err("XML 单个元素的属性过多".into());
                }
                attribute
                    .map_err(|e| format!("XML 属性损坏：{e}"))?
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map_err(|e| format!("XML 属性编码或实体错误：{e}"))?;
            }
        }
        match &event {
            Event::Start(_) => {
                if depth == 0 {
                    roots += 1;
                }
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err("XML 嵌套超过 64 层上限".into());
                }
            }
            Event::Empty(_) if depth == 0 => roots += 1,
            Event::End(_) => depth = depth.checked_sub(1).ok_or("XML 结束标签不匹配")?,
            Event::DocType(_) => return Err("不允许包含 DTD 或外部实体的 Office XML".into()),
            Event::Text(value) if depth == 0 => {
                if !value
                    .decode()
                    .map_err(|e| format!("XML 编码错误：{e}"))?
                    .trim()
                    .is_empty()
                {
                    return Err("XML 根元素外存在文本".into());
                }
            }
            Event::CData(_) | Event::GeneralRef(_) if depth == 0 => {
                return Err("XML 根元素外存在内容".into())
            }
            Event::Eof => {
                return if depth == 0 && roots == 1 {
                    Ok(())
                } else {
                    Err("XML 根元素缺失或未闭合".into())
                };
            }
            _ => {}
        }
        if roots > 1 {
            return Err("XML 含多个根元素".into());
        }
        visit(event)?;
    }
    Err("XML 事件数量超过资源上限".into())
}

fn extract_xml_text(data: &[u8], text: &mut Text) -> Result<(), String> {
    let mut in_text = false;
    walk_xml(data, |event| {
        match event {
            Event::Start(tag) => {
                if tag.local_name().as_ref() == b"t" {
                    in_text = true;
                }
            }
            Event::End(tag) => match tag.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" | b"tr" => text.newline(),
                b"tc" => text.push("\t"),
                _ => {}
            },
            Event::Empty(tag) => match tag.local_name().as_ref() {
                b"tab" => text.push("\t"),
                b"br" | b"cr" => text.newline(),
                _ => {}
            },
            Event::Text(value) if in_text => {
                text.push(
                    &value
                        .decode()
                        .map_err(|e| format!("XML 文本编码错误：{e}"))?,
                );
            }
            Event::CData(value) if in_text => {
                text.push(
                    &value
                        .decode()
                        .map_err(|e| format!("XML CDATA 编码错误：{e}"))?,
                );
            }
            Event::GeneralRef(value) => {
                let reference = value
                    .decode()
                    .map_err(|e| format!("XML 实体编码错误：{e}"))?;
                let escaped = format!("&{reference};");
                let decoded = quick_xml::escape::unescape(&escaped)
                    .map_err(|e| format!("XML 实体错误：{e}"))?;
                if in_text {
                    text.push(&decoded);
                }
            }
            _ => {}
        }
        Ok(())
    })
}

fn attributes(tag: &quick_xml::events::BytesStart<'_>) -> Result<HashMap<String, String>, String> {
    tag.attributes()
        .map(|attribute| {
            let attribute = attribute.map_err(|e| format!("XML 属性损坏：{e}"))?;
            let key = std::str::from_utf8(attribute.key.as_ref())
                .map_err(|_| "XML 属性名编码错误")?
                .to_owned();
            let value = attribute
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|e| format!("XML 属性值损坏：{e}"))?
                .into_owned();
            Ok((key, value))
        })
        .collect()
}

fn extract_pptx(data: &[u8], text: &mut Text) -> Result<(), String> {
    let mut package = Package::new(data)?;
    let rels = package.xml("ppt/_rels/presentation.xml.rels")?;
    let mut slides = HashMap::new();
    walk_xml(&rels, |event| {
        if let Event::Empty(tag) | Event::Start(tag) = event {
            if tag.local_name().as_ref() == b"Relationship" {
                let attrs = attributes(&tag)?;
                if attrs
                    .get("Type")
                    .is_some_and(|kind| kind.ends_with("/slide"))
                {
                    if attrs
                        .get("TargetMode")
                        .is_some_and(|mode| mode.eq_ignore_ascii_case("External"))
                    {
                        return Err("演示文稿引用外部幻灯片；不会访问外部链接".into());
                    }
                    let id = attrs.get("Id").ok_or("幻灯片关系缺少 Id")?;
                    let target = attrs.get("Target").ok_or("幻灯片关系缺少 Target")?;
                    if slides
                        .insert(id.clone(), resolve_part("ppt", target)?)
                        .is_some()
                    {
                        return Err("幻灯片关系 Id 重复".into());
                    }
                }
            }
        }
        Ok(())
    })?;
    let presentation = package.xml("ppt/presentation.xml")?;
    let mut ordered = Vec::new();
    walk_xml(&presentation, |event| {
        if let Event::Empty(tag) | Event::Start(tag) = event {
            if tag.local_name().as_ref() == b"sldId" {
                let attrs = attributes(&tag)?;
                let id = attrs
                    .iter()
                    .find(|(name, _)| name.ends_with(":id"))
                    .map(|(_, value)| value)
                    .ok_or("幻灯片列表缺少关系引用")?;
                let target = slides.get(id).ok_or("幻灯片关系引用不存在")?;
                if ordered.len() >= 2048 {
                    return Err("幻灯片超过 2048 页上限".into());
                }
                ordered.push(target.clone());
            }
        }
        Ok(())
    })?;
    if ordered.is_empty() {
        return Err("演示文稿不含幻灯片".into());
    }
    let mut has_text = false;
    for (index, path) in ordered.iter().enumerate() {
        let xml = package.xml(path)?;
        let mut slide = Text::default();
        extract_xml_text(&xml, &mut slide)?;
        has_text |= !slide.value.trim().is_empty();
        text.push(&format!("\n--- 幻灯片 {} ---\n", index + 1));
        text.push(&slide.value);
        text.truncated |= slide.truncated;
        text.newline();
    }
    if !has_text {
        return Err("演示文稿中没有可提取文字，可能仅包含图片".into());
    }
    Ok(())
}

fn resolve_part(base: &str, target: &str) -> Result<String, String> {
    if target.is_empty()
        || target.len() > 512
        || target.contains(['\\', ':', '?', '#', '%'])
        || target.starts_with("//")
    {
        return Err("Office 关系目标不是受支持的本地部件路径".into());
    }
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop().ok_or("Office 关系路径越出文档容器")?;
            }
            _ => parts.push(part),
        }
    }
    if parts.is_empty() {
        return Err("Office 关系目标路径为空".into());
    }
    Ok(parts.join("/"))
}

fn range(data: &[u8], offset: usize, len: usize) -> Result<&[u8], String> {
    let end = offset.checked_add(len).ok_or("文档偏移溢出")?;
    data.get(offset..end)
        .ok_or_else(|| "文档记录越界或被截断".into())
}

fn u16_at(data: &[u8], offset: usize) -> Result<u16, String> {
    let bytes = range(data, offset, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = range(data, offset, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn validate_cfb_allocation(data: &[u8]) -> Result<(), String> {
    if range(data, 0, 8)? != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return Err("不是 OLE/CFB 复合文件".into());
    }
    let shift = u16_at(data, 30)?;
    if !matches!(shift, 9 | 12) {
        return Err("CFB 扇区大小不受支持".into());
    }
    let sector_size = 1usize << shift;
    if data.len() < sector_size || !data.len().is_multiple_of(sector_size) {
        return Err("CFB 扇区数据被截断".into());
    }
    let sectors = data.len() / sector_size - 1;
    let entries = sector_size / 4;
    let max_fat = sectors.div_ceil(entries);
    let declared = u32_at(data, 44)? as usize;
    let difat_count = u32_at(data, 72)? as usize;
    if declared > max_fat || difat_count > max_fat.div_ceil(entries - 1) {
        return Err("CFB FAT/DIFAT 声明超过实际文件资源上限".into());
    }
    // 在交给 CFB 库前，阻止重复 FAT 扇区将小文件放大为巨型内存分配。
    let mut seen_fat = HashSet::new();
    let mut insert_fat = |sector: u32| -> Result<(), String> {
        if sector == 0xFFFF_FFFF {
            return Ok(());
        }
        if sector as usize >= sectors || !seen_fat.insert(sector) || seen_fat.len() > declared {
            return Err("CFB FAT 扇区重复、越界或数量不符".into());
        }
        Ok(())
    };
    for index in 0..109 {
        insert_fat(u32_at(data, 76 + index * 4)?)?;
    }
    let mut next = u32_at(data, 68)?;
    let mut seen_difat = HashSet::new();
    while next != 0xFFFF_FFFE {
        if next as usize >= sectors || !seen_difat.insert(next) || seen_difat.len() > difat_count {
            return Err("CFB DIFAT 链循环、越界或数量不符".into());
        }
        let at = (next as usize + 1) * sector_size;
        for index in 0..entries - 1 {
            insert_fat(u32_at(data, at + index * 4)?)?;
        }
        next = u32_at(data, at + sector_size - 4)?;
    }
    if seen_fat.len() != declared || seen_difat.len() != difat_count {
        return Err("CFB FAT/DIFAT 实际数量与声明不符".into());
    }
    Ok(())
}

fn compound(data: &[u8]) -> Result<cfb::CompoundFile<Cursor<&[u8]>>, String> {
    validate_cfb_allocation(data)?;
    let compound = cfb::CompoundFile::open(Cursor::new(data))
        .map_err(|e| format!("旧版 Office 复合文件损坏：{e}"))?;
    if compound.exists("/EncryptedPackage") || compound.exists("/EncryptionInfo") {
        return Err("不支持加密的 Office 附件，请先另存为未加密文件".into());
    }
    Ok(compound)
}

fn stream(compound: &mut cfb::CompoundFile<Cursor<&[u8]>>, name: &str) -> Result<Vec<u8>, String> {
    let entry = compound
        .entry(name)
        .map_err(|e| format!("缺少 {name} 数据流：{e}"))?;
    if entry.len() > MAX_FILE_BYTES {
        return Err("Office 数据流超过 32 MiB 上限".into());
    }
    let source = compound
        .open_stream(name)
        .map_err(|e| format!("打开 {name} 失败：{e}"))?;
    let mut bytes = Vec::new();
    source
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("读取 {name} 失败：{e}"))?;
    if bytes.len() as u64 > MAX_FILE_BYTES || bytes.len() as u64 != entry.len() {
        return Err("Office 数据流长度损坏或超过上限".into());
    }
    Ok(bytes)
}

fn extract_doc(data: &[u8], text: &mut Text) -> Result<(), String> {
    let mut compound = compound(data)?;
    let word = stream(&mut compound, "/WordDocument")?;
    if u16_at(&word, 0)? != 0xA5EC {
        return Err("不是有效的 Word 二进制文档".into());
    }
    if !matches!(
        u16_at(&word, 2)?,
        0x00C1 | 0x00D9 | 0x0101 | 0x010C | 0x0112
    ) {
        return Err("仅支持 Word 97–2007 二进制 DOC，请另存为 DOCX".into());
    }
    let flags = u16_at(&word, 10)?;
    if flags & (0x0100 | 0x8000) != 0 {
        return Err("DOC 已加密或混淆，请另存为未加密 DOCX".into());
    }
    let table = stream(
        &mut compound,
        if flags & 0x0200 != 0 {
            "/1Table"
        } else {
            "/0Table"
        },
    )?;
    let csw = u16_at(&word, 32)? as usize;
    let lw_count_at = 34 + csw * 2;
    let cslw = u16_at(&word, lw_count_at)? as usize;
    if cslw < 4 {
        return Err("DOC 的 FIB 长整数表损坏".into());
    }
    let lw_at = lw_count_at + 2;
    let body_chars = u32_at(&word, lw_at + 12)? as usize;
    let pairs_count_at = lw_at + cslw * 4;
    let pair_count = u16_at(&word, pairs_count_at)? as usize;
    if pair_count <= 33 {
        return Err("DOC 缺少 CLX 文字片段表".into());
    }
    let clx_pair = pairs_count_at + 2 + 33 * 8;
    let offset = u32_at(&word, clx_pair)? as usize;
    let len = u32_at(&word, clx_pair + 4)? as usize;
    if len == 0 {
        return Err("DOC 不含可读取的 piece table，请另存为 DOCX".into());
    }
    extract_doc_pieces(&word, range(&table, offset, len)?, body_chars, text)
}

fn extract_doc_pieces(
    word: &[u8],
    clx: &[u8],
    body_chars: usize,
    text: &mut Text,
) -> Result<(), String> {
    if body_chars > MAX_FILE_BYTES as usize {
        return Err("DOC 正文字符数超过资源上限".into());
    }
    let mut offset = 0;
    while clx.get(offset) == Some(&1) {
        let len = u16_at(clx, offset + 1)? as usize;
        range(clx, offset, len + 3)?;
        offset += len + 3;
    }
    if clx.get(offset) != Some(&2) {
        return Err("DOC CLX 不含有效 Pcdt 记录".into());
    }
    let size = u32_at(clx, offset + 1)? as usize;
    let plc = range(clx, offset + 5, size)?;
    if size < 4 || !(size - 4).is_multiple_of(12) {
        return Err("DOC piece table 长度无效".into());
    }
    let count = (size - 4) / 12;
    if count > 100_000 {
        return Err("DOC 文字片段超过资源上限".into());
    }
    if u32_at(plc, 0)? != 0 || (u32_at(plc, count * 4)? as usize) < body_chars {
        return Err("DOC 字符位置表与正文长度不一致".into());
    }
    let records_at = (count + 1) * 4;
    let mut fields = Vec::new();
    let mut decoded_bytes = 0u64;
    for index in 0..count {
        let start = u32_at(plc, index * 4)? as usize;
        let end = u32_at(plc, (index + 1) * 4)? as usize;
        let len = end.checked_sub(start).ok_or("DOC 字符位置表不是递增序列")?;
        let fc = u32_at(plc, records_at + index * 8 + 2)?;
        let compressed = fc & 0x4000_0000 != 0;
        let raw_offset = (fc & 0x3FFF_FFFF) as usize;
        let width = if compressed { 1 } else { 2 };
        let bytes = range(
            word,
            if compressed {
                raw_offset / 2
            } else {
                raw_offset
            },
            len.checked_mul(width).ok_or("DOC 文字长度溢出")?,
        )?;
        let used = end.min(body_chars).saturating_sub(start);
        if used == 0 {
            continue;
        }
        let bytes = &bytes[..used * width];
        decoded_bytes += bytes.len() as u64;
        if decoded_bytes > MAX_UNPACKED_BYTES {
            return Err("DOC 文字解码量超过资源上限".into());
        }
        let decoded = if compressed {
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(bytes)
                .0
                .into_owned()
        } else {
            utf16(bytes, true)?
        };
        for ch in decoded.chars() {
            match ch {
                '\u{13}' => {
                    if fields.len() >= MAX_DEPTH {
                        return Err("DOC 域嵌套超过上限".into());
                    }
                    fields.push(false);
                }
                '\u{14}' => {
                    if let Some(result) = fields.last_mut() {
                        *result = true;
                    }
                }
                '\u{15}' => {
                    fields.pop();
                }
                _ if fields.iter().any(|result| !result) => {}
                '\r' | '\u{b}' | '\u{c}' => text.newline(),
                '\u{7}' | '\t' => text.push("\t"),
                _ if ch.is_control() => {}
                _ => text.push(ch.encode_utf8(&mut [0; 4])),
            }
        }
    }
    Ok(())
}

fn extract_ppt(data: &[u8], text: &mut Text) -> Result<(), String> {
    let mut compound = compound(data)?;
    if compound.exists("/Current User") {
        let current = stream(&mut compound, "/Current User")?;
        if u32_at(&current, 12)? == 0xF3D1_C4DF {
            return Err("PPT 已加密，请另存为未加密 PPTX".into());
        }
    }
    let document = stream(&mut compound, "/PowerPoint Document")?;
    let mut records = 0;
    ppt_records(&document, 0, &mut records, text)
}

fn ppt_records(
    data: &[u8],
    depth: usize,
    count: &mut usize,
    text: &mut Text,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("PPT 记录嵌套超过 64 层上限".into());
    }
    let mut offset = 0;
    while offset < data.len() {
        *count += 1;
        if *count > MAX_EVENTS {
            return Err("PPT 记录数量超过上限".into());
        }
        let version = u16_at(data, offset)? & 0x000F;
        let record_type = u16_at(data, offset + 2)?;
        let len = u32_at(data, offset + 4)? as usize;
        let record = range(data, offset + 8, len)?;
        if record_type == 0x2F14 {
            return Err("PPT 包含加密记录，请先移除密码".into());
        }
        if version == 0x000F {
            ppt_records(record, depth + 1, count, text)?;
        } else if record_type == 4000 || record_type == 4008 {
            let decoded = if record_type == 4000 {
                utf16(record, true)?
            } else {
                // TextBytesAtom 是 UTF-16 低字节序列，不是系统 ANSI 代码页。
                record.iter().map(|&byte| char::from(byte)).collect()
            };
            for ch in decoded.chars() {
                match ch {
                    '\r' | '\u{b}' | '\u{c}' => text.newline(),
                    '\t' => text.push("\t"),
                    _ if ch.is_control() => {}
                    _ => text.push(ch.encode_utf8(&mut [0; 4])),
                }
            }
            text.newline();
        }
        offset += 8 + len;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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

    fn cfb(streams: &[(&str, &[u8])]) -> Vec<u8> {
        let mut file = cfb::CompoundFile::create(Cursor::new(Vec::new())).unwrap();
        for (name, data) in streams {
            file.create_stream(name).unwrap().write_all(data).unwrap();
        }
        file.into_inner().into_inner()
    }

    fn record(kind: u16, container: bool, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(if container { 15u16 } else { 0 }).to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn docx_extracts_runs_entities_and_table() {
        let data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", "<w:document xmlns:w='urn:w'><w:body><w:p><w:r><w:t>中文 &amp; </w:t></w:r><w:r><w:t>&#65;</w:t><w:tab/><w:t>末尾</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>表格</w:t></w:r></w:p></w:tc></w:tr></w:tbl></w:body></w:document>".as_bytes()),
        ]);
        let attachment = parse("test.docx", "docx", &data).unwrap();
        assert_eq!(attachment.kind, "document");
        assert!(attachment.text.contains("中文 & A\t末尾\n表格"));
        assert!(attachment.image_url.is_none());
    }

    #[test]
    fn pptx_uses_relationship_order_not_slide_filename() {
        let data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("ppt/presentation.xml", b"<p:presentation xmlns:p='p' xmlns:r='r'><p:sldIdLst><p:sldId id='256' r:id='second'/><p:sldId id='257' r:id='first'/></p:sldIdLst></p:presentation>"),
            ("ppt/_rels/presentation.xml.rels", b"<Relationships><Relationship Id='first' Type='urn:office/slide' Target='slides/slide1.xml'/><Relationship Id='second' Type='urn:office/slide' Target='slides/slide2.xml'/><Relationship Id='web' Type='urn:office/hyperlink' TargetMode='External' Target='https://invalid.example'/></Relationships>"),
            ("ppt/slides/slide1.xml", b"<s><p><t>AAA</t></p></s>"),
            ("ppt/slides/slide2.xml", b"<s><p><t>BBB</t></p></s>"),
        ]);
        let attachment = parse("test.pptx", "pptx", &data).unwrap();
        assert!(attachment.text.find("BBB").unwrap() < attachment.text.find("AAA").unwrap());
    }

    #[test]
    fn xml_rejects_dtd_bad_nesting_and_depth() {
        for xml in [
            b"<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///secret'>]><x/>".as_slice(),
            b"<x><t>x</x>",
            b"<x>",
            b"<x/><x/>",
            b"<x><t>&missing;</t></x>",
        ] {
            assert!(extract_xml_text(xml, &mut Text::default()).is_err());
        }
        let xml = format!(
            "{}{}",
            "<x>".repeat(MAX_DEPTH + 1),
            "</x>".repeat(MAX_DEPTH + 1)
        );
        assert!(extract_xml_text(xml.as_bytes(), &mut Text::default()).is_err());
    }

    #[test]
    fn paths_cannot_escape_package_or_access_network() {
        for path in [
            "../../secret.xml",
            "https://example.test/file",
            "//server/file",
            "slides\\file",
            "../%2e%2e/file",
        ] {
            assert!(resolve_part("ppt", path).is_err());
        }
        assert_eq!(
            resolve_part("ppt", "slides/slide1.xml").unwrap(),
            "ppt/slides/slide1.xml"
        );
        assert_eq!(
            resolve_part("ppt", "/ppt/slides/slide1.xml").unwrap(),
            "ppt/slides/slide1.xml"
        );
    }

    #[test]
    fn zip_rejects_invalid_paths_and_large_xml() {
        let data = zip(&[("[Content_Types].xml", b"<Types/>"), ("../escape", b"bad")]);
        assert!(Package::new(&data).is_err());
        let oversized = vec![b' '; MAX_XML_BYTES as usize + 1];
        let data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", &oversized),
        ]);
        assert!(parse("large.docx", "docx", &data)
            .unwrap_err()
            .contains("16 MiB"));
    }

    #[test]
    fn zip_budget_and_corrupt_payload_are_rejected() {
        let mut data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", b"<document><t>body</t></document>"),
        ]);
        let central = data
            .windows(4)
            .position(|signature| signature == b"PK\x01\x02")
            .unwrap();
        data[central + 24..central + 28]
            .copy_from_slice(&((MAX_UNPACKED_BYTES + 1) as u32).to_le_bytes());
        assert!(Package::new(&data).err().unwrap().contains("128 MiB"));
        let mut data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("word/document.xml", b"<document><t>body</t></document>"),
        ]);
        let local = data
            .windows(4)
            .enumerate()
            .filter(|(_, signature)| *signature == b"PK\x03\x04")
            .nth(1)
            .unwrap()
            .0;
        let name_len = u16_at(&data, local + 26).unwrap() as usize;
        let extra_len = u16_at(&data, local + 28).unwrap() as usize;
        data[local + 30 + name_len + extra_len] ^= 0xFF;
        assert!(parse("bad.docx", "docx", &data).is_err());
    }

    #[test]
    fn image_is_validated_and_sent_separately() {
        let image = image::DynamicImage::new_rgb8(2, 3);
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let attachment = parse("image.png", "png", png.get_ref()).unwrap();
        assert!(attachment
            .image_url
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(!attachment.text.contains("base64"));
        assert!(parse("broken.png", "png", b"not image").is_err());
        assert!(check_image_dimensions(8193, 1).is_err());
        assert!(check_image_dimensions(5000, 5000).is_err());
    }

    #[test]
    fn supported_image_formats_are_normalized() {
        for (extension, format) in [
            ("jpg", image::ImageFormat::Jpeg),
            ("webp", image::ImageFormat::WebP),
            ("gif", image::ImageFormat::Gif),
            ("bmp", image::ImageFormat::Bmp),
        ] {
            let image = image::DynamicImage::new_rgb8(3, 2);
            let mut encoded = Cursor::new(Vec::new());
            image.write_to(&mut encoded, format).unwrap();
            let attachment = parse("image", extension, encoded.get_ref()).unwrap();
            let data_url = attachment.image_url.unwrap();
            assert!(data_url.starts_with(if extension == "jpg" {
                "data:image/jpeg;base64,"
            } else {
                "data:image/png;base64,"
            }));
        }
    }

    #[test]
    fn zip_and_cfb_preflight_reject_forged_allocations() {
        let mut package = zip(&[("[Content_Types].xml", b"<Types/>")]);
        let footer = package.len() - 22;
        package[footer + 8..footer + 10].copy_from_slice(&5000u16.to_le_bytes());
        package[footer + 10..footer + 12].copy_from_slice(&5000u16.to_le_bytes());
        assert!(validate_zip_directory(&package)
            .unwrap_err()
            .contains("4096"));
        let mut compound = cfb(&[("/Example", b"text")]);
        compound[44..48].copy_from_slice(&100_000u32.to_le_bytes());
        assert!(validate_cfb_allocation(&compound).is_err());
    }

    #[test]
    fn external_slide_and_empty_presentation_are_rejected() {
        let data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("ppt/_rels/presentation.xml.rels", b"<Relationships><Relationship Id='bad' Type='urn:office/slide' TargetMode='External' Target='https://invalid.example'/></Relationships>"),
        ]);
        assert!(parse("external.pptx", "pptx", &data)
            .unwrap_err()
            .contains("外部"));
        let data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("ppt/_rels/presentation.xml.rels", b"<Relationships/>"),
            ("ppt/presentation.xml", b"<presentation/>"),
        ]);
        assert!(parse("empty.pptx", "pptx", &data)
            .unwrap_err()
            .contains("不含幻灯片"));
    }

    #[test]
    fn text_handles_utf16_gbk_and_char_limit() {
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(wide("中文\n正文"));
        assert_eq!(parse("text.txt", "txt", &utf16).unwrap().text, "中文\n正文");
        let gbk = encoding_rs::GBK.encode("中文").0;
        assert_eq!(parse("text.txt", "txt", &gbk).unwrap().text, "中文");
        let long = "中".repeat(MAX_TEXT_CHARS + 1);
        let attachment = parse("long.txt", "txt", long.as_bytes()).unwrap();
        assert_eq!(attachment.text.chars().count(), MAX_TEXT_CHARS);
        assert!(attachment.warning.unwrap().contains("截断"));
        assert!(parse("binary.txt", "txt", b"a\0b").is_err());
    }

    #[test]
    fn doc_piece_table_reads_mixed_unicode_and_compressed_text() {
        let mut word = vec![0; 1024];
        word[0..2].copy_from_slice(&0xA5ECu16.to_le_bytes());
        word[2..4].copy_from_slice(&0x00C1u16.to_le_bytes());
        word[32..34].copy_from_slice(&14u16.to_le_bytes());
        word[62..64].copy_from_slice(&22u16.to_le_bytes());
        word[76..80].copy_from_slice(&5u32.to_le_bytes());
        word[152..154].copy_from_slice(&34u16.to_le_bytes());
        word[512..518].copy_from_slice(b"ABC\rZZ");
        word[600..604].copy_from_slice(&wide("中文"));
        let mut plc = Vec::new();
        for cp in [0u32, 3, 5] {
            plc.extend_from_slice(&cp.to_le_bytes());
        }
        for fc in [0x4000_0000u32 | 1024, 600] {
            plc.extend_from_slice(&[0; 2]);
            plc.extend_from_slice(&fc.to_le_bytes());
            plc.extend_from_slice(&[0; 2]);
        }
        let mut clx = vec![2];
        clx.extend_from_slice(&(plc.len() as u32).to_le_bytes());
        clx.extend_from_slice(&plc);
        let pair = 154 + 33 * 8;
        word[pair + 4..pair + 8].copy_from_slice(&(clx.len() as u32).to_le_bytes());
        let data = cfb(&[("/WordDocument", &word), ("/0Table", &clx)]);
        assert_eq!(parse("old.doc", "doc", &data).unwrap().text, "ABC中文");
        let mut damaged = clx.clone();
        damaged[9..13].copy_from_slice(&100u32.to_le_bytes());
        assert!(extract_doc_pieces(&word, &damaged, 5, &mut Text::default()).is_err());
        word[10..12].copy_from_slice(&0x0100u16.to_le_bytes());
        let encrypted = cfb(&[("/WordDocument", &word), ("/0Table", &clx)]);
        assert!(parse("locked.doc", "doc", &encrypted)
            .unwrap_err()
            .contains("加密"));
    }

    #[test]
    fn ppt_extracts_unicode_and_byte_atoms_from_cfb() {
        let mut atoms = record(4000, false, &wide("幻灯片\r正文"));
        atoms.extend(record(4008, false, b"Latin"));
        let records = record(1000, true, &atoms);
        let data = cfb(&[("/PowerPoint Document", &records)]);
        let attachment = parse("old.ppt", "ppt", &data).unwrap();
        assert_eq!(attachment.text, "幻灯片\n正文\nLatin");
        assert!(attachment.warning.unwrap().contains("历史"));
        assert!(ppt_records(
            &records[..records.len() - 1],
            0,
            &mut 0,
            &mut Text::default()
        )
        .is_err());
        let mut nested = record(4008, false, b"x");
        for _ in 0..MAX_DEPTH + 2 {
            nested = record(1000, true, &nested);
        }
        assert!(ppt_records(&nested, 0, &mut 0, &mut Text::default()).is_err());
    }

    #[test]
    fn corrupt_empty_unsupported_and_oversize_are_errors() {
        for extension in ["doc", "docx", "ppt", "pptx"] {
            assert!(parse("broken", extension, b"broken").is_err());
        }
        assert!(parse("empty.txt", "txt", b"").is_err());
        assert!(parse("unknown.exe", "exe", b"MZ").is_err());
        let data = vec![0; MAX_FILE_BYTES as usize + 1];
        assert!(parse("huge.txt", "txt", &data)
            .unwrap_err()
            .contains("32 MiB"));
    }
}
