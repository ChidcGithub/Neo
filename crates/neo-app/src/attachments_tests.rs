
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
    fn historical_tool_images_only_change_outbound_copy() {
        for (summary, data) in [
            ("已附图", serde_json::json!({"path":"audit.png"})),
            ("历史截图", serde_json::json!({"image_to_desktop":{"offset_x":1},"path":"audit.png"})),
            ("历史图片", serde_json::json!({"image_attached":true,"sent_size":{"width":2,"height":3},"path":"audit.png"})),
            ("历史引用", serde_json::json!({"image_attached":true,"path":"audit.png",
                "historical_reference_audit":{"screenshot_id":"old-shot","image_to_desktop":{"offset_x":1}},
                "extra_audit":"x".repeat(24_000)})),
        ] {
            let content = neo_tools::Outcome::ok("view_image", summary, data).to_model_json(usize::MAX);
            let value: serde_json::Value = serde_json::from_str(&tool_content_without_images(&content)).unwrap();
            assert_eq!(value["historical_image"], true);
            assert!(value.get("historical_image_audit").is_none());
            assert!(value.to_string().len() < 1024);
            assert!(!value.to_string().contains("old-shot"));
            assert!(!value.to_string().contains("historical_reference_audit"));
            assert_eq!(tool_content_without_images(&value.to_string()), value.to_string());
            let original: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert_eq!(original["summary"], summary);
            assert_eq!(value["data"]["path"], "audit.png");
            assert_eq!(value["data"]["image_attached"], false);
            assert!(value["data"]["screenshot_id"].is_null());
            assert!(value["data"].get("image_to_desktop").is_none());
            assert!(value["data"].get("sent_size").is_none());
            assert!(value["summary"].as_str().unwrap().contains("未随本次请求提供"));
        }
        let plain = "旧记录：已附图，路径 audit.png";
        let outbound = tool_content_without_images(plain);
        assert!(outbound.contains("未随本次请求提供") && !outbound.contains(plain));
        let normal = neo_tools::Outcome::ok("read_file", "读取完成", serde_json::json!({"content":"x"})).to_model_json(usize::MAX);
        assert_eq!(tool_content_without_images(&normal), normal);
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
