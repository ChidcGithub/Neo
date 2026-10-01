
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
    fn xml_break_forms_and_paired_controls_preserve_separators() {
        for br in ["<a:br/>", "<a:br></a:br>", "<a:br><a:rPr lang='zh-CN'/></a:br>"] {
            let xml = format!(
                "<a:p xmlns:a='urn:a'><a:r><a:t>before</a:t></a:r>{br}<a:r><a:t>after</a:t><a:tab></a:tab><a:t>tab</a:t><a:cr></a:cr><a:t>end</a:t></a:r></a:p>"
            );
            let mut text = Text::default();
            extract_xml_text(xml.as_bytes(), &mut text).unwrap();
            assert_eq!(text.value, "before\nafter\ttab\nend\n", "{br}");
        }
    }

    #[test]
    fn pptx_keeps_styled_break_between_runs() {
        let data = zip(&[
            ("[Content_Types].xml", b"<Types/>"),
            ("ppt/presentation.xml", b"<p:presentation xmlns:p='p' xmlns:r='r'><p:sldIdLst><p:sldId id='256' r:id='first'/></p:sldIdLst></p:presentation>"),
            ("ppt/_rels/presentation.xml.rels", b"<Relationships><Relationship Id='first' Type='urn:office/slide' Target='slides/slide1.xml'/></Relationships>"),
            ("ppt/slides/slide1.xml", b"<p:sld xmlns:p='p' xmlns:a='a'><a:p><a:r><a:t>before</a:t></a:r><a:br><a:rPr lang='en-US'/></a:br><a:r><a:t>after</a:t></a:r></a:p></p:sld>"),
        ]);
        let attachment = parse("break.pptx", "pptx", &data).unwrap();
        assert_eq!(attachment.text, "--- 幻灯片 1 ---\nbefore\nafter");
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
