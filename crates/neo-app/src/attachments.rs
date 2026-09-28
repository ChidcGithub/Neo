pub use neo_tools::documents::{load, Attachment, MAX_FILES};

/// 工具图片不静默缩放：截图的坐标必须继续对应原始物理像素。
pub fn prepare_tool_images(outcome: &mut neo_tools::Outcome) {
    use base64::Engine;
    use neo_tools::tools::view_image::{model_image_with_sizes, MAX_SOURCE_IMAGE_BYTES};
    let had_images = !outcome.images.is_empty();
    let mut images = Vec::new();
    let mut warnings = Vec::new();
    if !had_images && (outcome.data["image_attached"] == true || outcome.data["screenshot_id"].is_string()) {
        warnings.push("工具声称附图或提供 screenshot_id，但没有可发送的图片数据".to_owned());
    }
    for url in std::mem::take(&mut outcome.images) {
        let prepared = (|| {
            let (header, data) = url.split_once(',').ok_or("图片不是 data URL")?;
            if !matches!(header, "data:image/png;base64" | "data:image/jpeg;base64"
                | "data:image/gif;base64" | "data:image/bmp;base64" | "data:image/webp;base64")
                || data.len() > (MAX_SOURCE_IMAGE_BYTES + 2) / 3 * 4 {
                return Err("图片格式或编码体积超限".to_owned());
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(data)
                .map_err(|_| "图片 base64 损坏".to_owned())?;
            let image = model_image_with_sizes(&bytes, false)?;
            let (width, height) = image.sent_size;
            for (field, w, h) in [
                ("sent_size", "width", "height"),
                ("image_space", "width", "height"),
            ] {
                if let Some(size) = outcome.data.get(field).filter(|v| !v.is_null()) {
                    if size[w].as_u64() != Some(u64::from(width)) || size[h].as_u64() != Some(u64::from(height)) {
                        return Err(format!("{field} 与实际附图尺寸不一致"));
                    }
                }
            }
            if outcome.tool == "screenshot" {
                for (key, expected) in [("sent_width", width), ("sent_height", height)] {
                    if let Some(value) = outcome.data.get(key).filter(|v| !v.is_null()) {
                        if value.as_u64() != Some(u64::from(expected)) {
                            return Err(format!("{key} 与实际附图尺寸不一致"));
                        }
                    }
                }
                if let Some(mapping) = outcome.data.get("image_to_desktop") {
                    let region = &outcome.data["region"];
                    if mapping["scale_x"] != 1 || mapping["scale_y"] != 1
                        || mapping["dpi_conversion"] != false
                        || mapping["offset_x"] != region["x"] || mapping["offset_y"] != region["y"]
                        || region["width"].as_u64() != Some(u64::from(width))
                        || region["height"].as_u64() != Some(u64::from(height)) {
                        return Err("image_to_desktop 与原尺寸附图或 region 不一致".to_owned());
                    }
                }
            }
            neo_llm::validate_image(&image.url)?;
            Ok(image.url)
        })();
        match prepared {
            Ok(image) => images.push(image),
            Err(error) => warnings.push(error),
        }
    }
    outcome.images = images;
    if had_images && !outcome.data.is_object() {
        outcome.data = serde_json::json!({"result": outcome.data});
    }
    if outcome.tool == "screenshot" && outcome.images.len() == 1 {
        let (header, encoded) = outcome.images[0].split_once(',').unwrap();
        let format = if header == "data:image/png;base64" { "png" } else { "jpeg" };
        outcome.data["sent_format"] = serde_json::json!(format);
        outcome.data["lossless"] = serde_json::json!(format == "png");
        outcome.data["sent_bytes"] = serde_json::json!(encoded.len() / 4 * 3 - encoded.bytes().rev().take_while(|b| *b == b'=').count());
    }
    if had_images || !warnings.is_empty() || outcome.data.get("image_attached").is_some() {
        if !outcome.data.is_object() {
            outcome.data = serde_json::json!({"result": outcome.data});
        }
        outcome.data["image_attached"] = serde_json::json!(!outcome.images.is_empty());
    }
    if !warnings.is_empty() {
        let status = if outcome.images.is_empty() { "图片未发送" } else { "部分图片未附送" };
        outcome.data["image_warning"] = serde_json::json!(format!(
            "{status}：{}。原文件不变；请重新截取更小的区域（x/y/width/height），不要用未发送图片定位。", warnings.join("；")
        ));
        outcome.data["next"] = serde_json::json!("重新截取更小的区域（x/y/width/height）以获取可发送图片及新的截图引用；不得按未发送图片或 view_image 缩略图操作桌面。");
        if outcome.images.is_empty() {
            if let Some(id) = outcome.data["screenshot_id"].as_str() {
                neo_tools::tools::screenshot_space::revoke(id);
            }
            if let Some(data) = outcome.data.as_object_mut() {
                for key in ["sent_size", "sent_width", "sent_height", "sent_format", "lossless", "screenshot_id"] {
                    if data.contains_key(key) { data.insert(key.into(), serde_json::Value::Null); }
                }
                if data.contains_key("sent_bytes") { data.insert("sent_bytes".into(), serde_json::json!(0)); }
                for key in ["source_to_sent_scale", "image_to_desktop", "image_space", "reference_ttl_seconds"] {
                    data.remove(key);
                }
            }
        }
        outcome.summary = if outcome.tool == "screenshot" && outcome.is_ok() {
            format!("截图已保存到 {}，{status}；请重新截取更小的区域，见 image_warning。",
                outcome.data["path"].as_str().unwrap_or("工具返回路径"))
        } else {
            format!("{}：{status}；原文件不变，见 image_warning。", outcome.tool)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn png_url(width: u32, height: u32) -> String {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(width, height).write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes.into_inner()))
    }

    #[test]
    fn tool_image_reencodes_bmp_and_explains_rejection() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3).write_to(&mut bytes, image::ImageFormat::Bmp).unwrap();
        let mut outcome = neo_tools::Outcome::ok("screenshot", "截图", serde_json::json!({"x": -20}))
            .with_image(format!("data:image/bmp;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())))
            .with_image("data:image/png;base64,broken");
        prepare_tool_images(&mut outcome);
        assert_eq!(outcome.images.len(), 1);
        neo_llm::validate_image(&outcome.images[0]).unwrap();
        assert_eq!(outcome.data["x"], -20);
        assert_eq!(outcome.data["image_attached"], true);
        assert!(outcome.data["image_warning"].as_str().unwrap().contains("未附送"));
    }

    #[test]
    fn rejected_tool_image_summary_matches_attachment_state() {
        let mut corrupt = base64::engine::general_purpose::STANDARD
            .decode(png_url(2, 3).split_once(',').unwrap().1).unwrap();
        corrupt.truncate(33);
        for url in [
            png_url(4096, 2161),
            format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(corrupt)),
        ] {
            let mut outcome = neo_tools::Outcome::ok("screenshot", "截屏已保存到 shot.png，已把图交给模型",
                serde_json::json!({"path": "shot.png", "image_attached": true, "region": {"x": -1920, "y": -100}}))
                .with_image(url);
            prepare_tool_images(&mut outcome);
            assert!(outcome.is_ok());
            assert!(outcome.images.is_empty());
            assert_eq!(outcome.data["image_attached"], false);
            assert_eq!(outcome.data["region"]["x"], -1920);
            assert!(outcome.summary.contains("已保存"));
            assert!(outcome.summary.contains("未发送"));
            assert!(!outcome.summary.contains("已把图交给模型"));
            assert!(outcome.data["next"].as_str().unwrap().contains("重新截取更小的区域"));
        }
    }

    #[test]
    fn synthetic_4k_tool_image_preserves_dimensions_and_region() {
        let url = png_url(3840, 2160);
        let region = serde_json::json!({"x": -3840, "y": -100, "width": 3840, "height": 2160});
        let mut outcome = neo_tools::Outcome::ok("screenshot", "已把图交给模型",
            serde_json::json!({"region": region, "screenshot_id": "synthetic", "image_attached": true}))
            .with_image(url.clone());
        prepare_tool_images(&mut outcome);
        assert_eq!(outcome.images, vec![url]);
        assert_eq!(outcome.data["image_attached"], true);
        assert_eq!(outcome.data["region"], region);
        assert_eq!(outcome.data["screenshot_id"], "synthetic");
        neo_llm::validate_image(&outcome.images[0]).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD.decode(outcome.images[0].split_once(',').unwrap().1).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (3840, 2160));
    }

    fn screenshot_data(width: u32, height: u32) -> serde_json::Value {
        use neo_tools::tools::{screen::Rect, screenshot_space::ImageSpace};
        let region = Rect { x: -100, y: -50, width: width as i32, height: height as i32 };
        let mut data = ImageSpace::new(region, width, height, vec![region]).unwrap().metadata();
        data["region"] = serde_json::json!({"x": -100, "y": -50, "width": width, "height": height});
        data["screenshot_id"] = serde_json::json!("synthetic-reference");
        data["sent_size"] = serde_json::json!({"width": width, "height": height});
        data["sent_width"] = serde_json::json!(width);
        data["sent_height"] = serde_json::json!(height);
        data["image_attached"] = serde_json::json!(true);
        data
    }

    #[test]
    fn reencoded_screenshot_mapping_and_reference_survive_repeated_preparation() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(20, 30).write_to(&mut bytes, image::ImageFormat::Bmp).unwrap();
        let data = screenshot_data(20, 30);
        let mut outcome = neo_tools::Outcome::ok("screenshot", "已把图交给模型", data.clone())
            .with_image(format!("data:image/bmp;base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())));
        prepare_tool_images(&mut outcome);
        let first = outcome.to_model_json(usize::MAX);
        let images = outcome.images.clone();
        for _ in 0..3 {
            prepare_tool_images(&mut outcome);
            assert_eq!(outcome.to_model_json(usize::MAX), first);
            assert_eq!(outcome.images, images);
            for key in ["screenshot_id", "image_to_desktop", "image_space", "sent_size", "region"] {
                assert_eq!(outcome.data[key], data[key]);
            }
        }
        let sent = base64::engine::general_purpose::STANDARD.decode(outcome.images[0].split_once(',').unwrap().1).unwrap();
        assert_eq!(outcome.data["sent_bytes"], sent.len());
        assert_eq!(outcome.data["sent_format"], "png");
        assert_eq!(outcome.data["lossless"], true);
    }

    #[test]
    fn missing_or_mismatched_screenshot_never_exposes_usable_reference() {
        for variant in 0..6 {
            let mut data = screenshot_data(20, 30);
            match variant {
                1 => data["sent_size"]["width"] = serde_json::json!(10),
                2 => data["image_space"]["height"] = serde_json::json!(15),
                3 => data["image_to_desktop"]["scale_x"] = serde_json::json!(2),
                4 => data["image_to_desktop"]["offset_x"] = serde_json::json!(0),
                5 => data["sent_width"] = serde_json::json!(10),
                _ => {}
            }
            let mut outcome = neo_tools::Outcome::ok("screenshot", "已把图交给模型", data);
            if variant != 0 { outcome = outcome.with_image(png_url(20, 30)); }
            prepare_tool_images(&mut outcome);
            assert!(outcome.images.is_empty(), "variant {variant}");
            assert_eq!(outcome.data["image_attached"], false);
            assert!(outcome.data["screenshot_id"].is_null());
            assert!(outcome.data["sent_size"].is_null());
            assert!(outcome.data.get("image_to_desktop").is_none());
            assert!(outcome.summary.contains("未发送"));
            assert!(!outcome.summary.contains("已把图交给模型"));
            let first = outcome.to_model_json(usize::MAX);
            prepare_tool_images(&mut outcome);
            assert_eq!(outcome.to_model_json(usize::MAX), first);
        }
    }

    #[test]
    fn rejected_reference_revokes_only_its_own_cache_entry() {
        use neo_tools::tools::{screen::Rect, screenshot_space};
        let rect = Rect { x: -100, y: -50, width: 20, height: 30 };
        let space = screenshot_space::ImageSpace::new(rect, 20, 30, vec![rect]).unwrap();
        let scope = neo_tools::Scope::new(std::env::temp_dir());
        for missing in [true, false] {
            let generation = screenshot_space::generation();
            let good = screenshot_space::register(space.clone(), &scope, generation).unwrap();
            let bad = screenshot_space::register(space.clone(), &scope, generation).unwrap();
            let mut good_data = screenshot_data(20, 30);
            good_data["screenshot_id"] = serde_json::json!(good);
            let mut accepted = neo_tools::Outcome::ok("screenshot", "截图", good_data).with_image(png_url(20, 30));
            prepare_tool_images(&mut accepted);
            let mut bad_data = screenshot_data(20, 30);
            bad_data["screenshot_id"] = serde_json::json!(bad);
            let mut rejected = neo_tools::Outcome::ok("screenshot", "截图", bad_data);
            if !missing { rejected = rejected.with_image("data:image/png;base64,broken"); }
            prepare_tool_images(&mut rejected);
            assert!(rejected.data["screenshot_id"].is_null());
            assert!(screenshot_space::require_known(&bad).is_err());
            assert!(screenshot_space::require_known(&good).is_ok());
            assert_eq!(screenshot_space::generation(), generation);
            screenshot_space::revoke(&good);
        }
    }

    #[test]
    fn missing_tool_image_cannot_claim_attachment() {
        let mut outcome = neo_tools::Outcome::ok("view_image", "已把图交给模型",
            serde_json::json!({"image_attached": true, "sent_size": {"width": 2, "height": 3}}));
        prepare_tool_images(&mut outcome);
        assert_eq!(outcome.data["image_attached"], false);
        assert!(outcome.data["sent_size"].is_null());
        assert!(!outcome.summary.contains("已把图交给模型"));
    }
}
