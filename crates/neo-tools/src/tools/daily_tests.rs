
    use super::*;

    #[test]
    fn filetime_conversion_uses_1601_epoch_and_100ns_units() {
        assert_eq!(unix_ms_to_filetime(0), Some(116_444_736_000_000_000));
        assert_eq!(unix_ms_to_filetime(-1), Some(116_444_735_999_990_000));
        assert_eq!(unix_ms_to_filetime(1_704_067_200_000), Some(133_485_408_000_000_000));
        assert_eq!(unix_ms_to_filetime(-11_644_473_600_000), Some(0));
        assert_eq!(unix_ms_to_filetime(i64::MAX), None);
        assert_eq!(unix_ms_to_filetime(i64::MIN), None);
    }

    #[cfg(windows)]
    #[test]
    fn filetime_converts_to_known_utc_date() {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::FileTimeToSystemTime;
        let ticks = unix_ms_to_filetime(1_704_067_200_000).unwrap();
        let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
        assert_ne!(unsafe { FileTimeToSystemTime(&ft, &mut utc) }, 0);
        assert_eq!((utc.wYear, utc.wMonth, utc.wDay, utc.wHour, utc.wMinute), (2024, 1, 1, 0, 0));
    }
