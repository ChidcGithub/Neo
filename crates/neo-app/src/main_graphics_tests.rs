
    use super::*;

    #[test]
    fn window_setup_excludes_vulkan_before_adapter_enumeration() {
        let setup = window_wgpu_setup();
        assert_eq!(setup.instance_descriptor.backends, eframe::wgpu::Backends::DX12);
    }

    #[test]
    fn window_limits_follow_adapter_without_8192_requirement() {
        for max_dimension in [4096, 8192, 16384] {
            let supported = eframe::wgpu::Limits {
                max_texture_dimension_2d: max_dimension,
                ..eframe::wgpu::Limits::downlevel_defaults()
            };
            let requested = window_limits(&supported);
            assert!(requested.check_limits(&supported));
            assert_eq!(requested.max_texture_dimension_2d, max_dimension);
            assert!(requested.max_texture_dimension_2d >= 3840);
        }
    }
