#[cfg(windows)]
#[test]
fn sealed_value_roundtrips_and_hides_plaintext() {
    let plain = "sk-neo-test-测试-42";
    let sealed = super::seal(plain).unwrap();
    assert!(super::is_sealed(&sealed));
    assert!(!sealed.contains(plain));
    assert_eq!(super::open(&sealed).unwrap(), plain);
    assert_eq!(super::open(&super::seal("").unwrap()).unwrap(), "");
}

#[test]
fn legacy_plaintext_passes_through() {
    assert!(!super::is_sealed("sk-legacy"));
    assert_eq!(super::open("sk-legacy").unwrap(), "sk-legacy");
}

#[cfg(windows)]
#[test]
fn corrupt_sealed_value_is_rejected() {
    assert!(super::open("dpapi1:not-base64!!").is_err());
    assert!(super::open("dpapi1:AQID").is_err());
}
