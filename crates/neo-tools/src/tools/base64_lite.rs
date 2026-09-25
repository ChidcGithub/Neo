//! 极小的 base64 编码器（只要 encode，不要依赖）。
//!
//! 图片要以 `data:…;base64,…` 的形式发给多模态模型，所以只要编码方向。
//! 正确性用 RFC 4648 §10 的标准测试向量钉住（见下方测试）。

const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
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
}
