
    use super::*;
    fn space() -> ImageSpace {
        ImageSpace::new(Rect { x: -1920, y: -100, width: 1920, height: 1080 }, 1920, 1080,
            vec![Rect { x: -1920, y: -100, width: 1920, height: 1080 }, Rect { x: 0, y: 0, width: 3840, height: 2160 }]).unwrap()
    }
    #[test]
    fn negative_mixed_dpi_and_nested_crop_are_translation_only() {
        let original = space();
        assert_eq!(original.point(1919, 1079).unwrap(), (-1, 979));
        let crop = original.crop(Rect { x: 100, y: 200, width: 500, height: 300 }).unwrap();
        let child = ImageSpace::new(crop, 500, 300, original.topology.clone()).unwrap();
        let nested = child.crop(Rect { x: 20, y: 30, width: 100, height: 80 }).unwrap();
        assert_eq!((nested.x, nested.y), (-1800, 130));
        assert_eq!(child.point(20, 30).unwrap(), (-1800, 130));
        assert!(child.point(500, 0).is_err());
        assert!(child.crop(Rect { x: 450, y: 0, width: 100, height: 20 }).is_err());
        assert!(ImageSpace::new(crop, 250, 150, original.topology.clone()).is_err());
        assert_eq!(child.metadata()["image_to_desktop"]["dpi_conversion"], false);
    }
    #[test]
    fn bounded_ttl_topology_and_action_invalidation() {
        let mut cache = Cache::default();
        let scope = crate::Scope::new(std::env::temp_dir());
        let now = Instant::now();
        let first = cache.insert(space(), now, &scope);
        let child = cache.insert(space(), now, &scope);
        assert!(cache.resolve(&first, space().topology, now).is_ok());
        assert!(cache.lookup(&child, now).is_ok());
        assert!(cache.lookup("unknown", now).is_err());
        assert!(cache.lookup(&first, now + Duration::from_secs(TTL_SECS)).is_err());
        let first = cache.insert(space(), now, &scope);
        for _ in 0..CAPACITY { cache.insert(space(), now, &scope); }
        assert_eq!(cache.entries.len(), CAPACITY);
        assert!(cache.lookup(&first, now).is_err());
        let id = cache.insert(space(), now, &scope);
        assert!(cache.resolve(&id, vec![Rect { x: 0, y: 0, width: 1920, height: 1080 }], now).is_err());
        let id = cache.insert(space(), now, &scope);
        cache.entries.clear();
        assert!(cache.lookup(&id, now).is_err());
    }

    #[test]
    fn cancellation_concurrent_invalidation_and_uia_observation() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        let _interaction = super::super::screen_uia::INTERACTION.lock().unwrap();
        invalidate();
        let token = Arc::new(AtomicBool::new(false));
        let scope = crate::Scope::new(std::env::temp_dir()).with_cancel(token.clone());
        let before = generation();
        let id = register(space(), &scope, before).unwrap();
        super::super::screen_uia::cache_invalidate();
        assert!(require_known(&id).is_ok());
        token.store(true, Ordering::Release);
        assert!(require_known(&id).is_err());
        assert!(register(space(), &scope, before).is_err());
        assert!(confirm_delivery(&id).is_err());
        let delivered_token = Arc::new(AtomicBool::new(false));
        let delivered_scope = crate::Scope::new(std::env::temp_dir()).with_cancel(delivered_token.clone());
        let delivered = register(space(), &delivered_scope, generation()).unwrap();
        confirm_delivery(&delivered).unwrap();
        delivered_token.store(true, Ordering::Release);
        assert!(require_known(&delivered).is_ok());
        revoke(&delivered);
        assert!(require_known(&delivered).is_err());
        assert!(confirm_delivery(&delivered).is_err());
        let scope = crate::Scope::new(std::env::temp_dir());
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let worker_barrier = barrier.clone();
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            invalidate();
        });
        barrier.wait();
        worker.join().unwrap();
        assert!(register(space(), &scope, before).is_err());
        let id = register(space(), &scope, generation()).unwrap();
        assert!(require_known(&id).is_ok());
        invalidate();
        assert!(require_known(&id).is_err());
    }
