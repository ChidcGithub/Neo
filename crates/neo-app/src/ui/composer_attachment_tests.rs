
    use super::*;
    use base64::Engine;

    #[test]
    fn thumbnail_is_small_and_reuses_texture() {
        let ctx = egui::Context::default();
        let image = image::DynamicImage::new_rgba8(240, 120);
        let mut bytes = std::io::Cursor::new(Vec::new());
        image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
        let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes.get_ref())
        );
        let first = attachment_thumbnail(&ctx, &url).unwrap();
        let second = attachment_thumbnail(&ctx, &url).unwrap();
        assert_eq!(first.size(), [96, 48]);
        assert_eq!(first.id(), second.id());
        let cache = ctx.data(|data| {
            data.get_temp::<ThumbnailCache>(egui::Id::new("neo-attachment-thumbnails"))
                .unwrap()
        });
        assert_eq!(cache.entries.len(), 1);
    }

    #[test]
    fn failed_thumbnails_are_cached_and_bounded() {
        let ctx = egui::Context::default();
        let first = "data:image/png;base64,broken";
        assert!(attachment_thumbnail(&ctx, first).is_none());
        assert!(attachment_thumbnail(&ctx, first).is_none());
        let cache_id = egui::Id::new("neo-attachment-thumbnails");
        assert_eq!(
            ctx.data(|data| data
                .get_temp::<ThumbnailCache>(cache_id)
                .unwrap()
                .entries
                .len()),
            1
        );
        for index in 0..70 {
            assert!(attachment_thumbnail(&ctx, &format!("invalid-{index}")).is_none());
        }
        let cache = ctx.data(|data| data.get_temp::<ThumbnailCache>(cache_id).unwrap());
        assert_eq!(cache.entries.len(), 64);
        assert!(cache.entries.iter().all(|(_, texture)| texture.is_none()));
        assert!(!cache
            .entries
            .iter()
            .any(|(id, _)| *id == egui::Id::new(first)));
    }
