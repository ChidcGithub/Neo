
    use super::encode;

    /// RFC 4648 §10 的标准测试向量 —— 编码器只有这七条能证明它是对的。
    #[test]
    fn matches_rfc4648_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    /// 补位的两种长度各走一遍（`=` 的个数是 1 和 2）。
    #[test]
    fn padding_marks_short_tail() {
        assert_eq!(encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(encode(&[0xff, 0xff]), "//8=");
        assert_eq!(encode(&[0xff]), "/w==");
    }
