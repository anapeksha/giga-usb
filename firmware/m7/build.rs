use std::{env, fs, path::PathBuf};

const DEFAULT_PUBLIC_KEY: &str = "49d8df2bc10f06a76ef2d42298ccc1f2b699ea6c338a03ef66dfb31026395706";

fn public_key(hex: &str) -> Result<[u8; 32], &'static str> {
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("GIGAUSB_OTA_PUBLIC_KEY_HEX must contain exactly 64 ASCII hex digits");
    }
    let mut key = [0; 32];
    for (index, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    if key.iter().all(|byte| *byte == 0) {
        return Err("GIGAUSB_OTA_PUBLIC_KEY_HEX must not be the all-zero key");
    }
    Ok(key)
}

fn build_id(value: &str) -> Result<&str, &'static str> {
    if value.is_empty()
        || value.len() > 48
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(
            "GIGAUSB_OTA_BUILD_ID must contain 1..=48 ASCII letters, digits, '.', '_' or '-'",
        );
    }
    Ok(value)
}

fn setting(name: &str, default: &str) -> String {
    match env::var(name) {
        Ok(value) => value,
        Err(env::VarError::NotPresent) => default.to_owned(),
        Err(env::VarError::NotUnicode(_)) => panic!("{name} must be ASCII"),
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=GIGAUSB_OTA_PUBLIC_KEY_HEX");
    println!("cargo:rerun-if-env-changed=GIGAUSB_OTA_BUILD_ID");
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let key = public_key(&setting("GIGAUSB_OTA_PUBLIC_KEY_HEX", DEFAULT_PUBLIC_KEY))
        .unwrap_or_else(|error| panic!("{error}"));
    let identity = setting("GIGAUSB_OTA_BUILD_ID", "development");
    let identity = build_id(&identity).unwrap_or_else(|error| panic!("{error}"));
    fs::write(
        out.join("ota_build.rs"),
        format!(
            "pub const TRUSTED_KEY: [u8; 32] = {key:?};\npub const BUILD_ID: &str = {identity:?};\n"
        ),
    )
    .unwrap();
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("none") {
        return;
    }
    fs::copy("memory.x", out.join("memory.x")).unwrap();
    fs::copy("bootloader.x", out.join("bootloader.x")).unwrap();
    println!("cargo:rustc-link-arg-bin=bootloader=-Tbootloader.x");
    println!("cargo:rerun-if-changed=bootloader.x");

    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg-bin=m7=-Tlink.x");

    if env::var_os("CARGO_FEATURE_DEFMT").is_some() {
        println!("cargo:rustc-link-arg-bin=m7=-Tdefmt.x");
    }
    println!("cargo:rerun-if-changed=memory.x");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_key_is_the_original_rc3_public_key() {
        assert_eq!(
            public_key(DEFAULT_PUBLIC_KEY).unwrap(),
            [
                0x49, 0xd8, 0xdf, 0x2b, 0xc1, 0x0f, 0x06, 0xa7, 0x6e, 0xf2, 0xd4, 0x22, 0x98, 0xcc,
                0xc1, 0xf2, 0xb6, 0x99, 0xea, 0x6c, 0x33, 0x8a, 0x03, 0xef, 0x66, 0xdf, 0xb3, 0x10,
                0x26, 0x39, 0x57, 0x06,
            ]
        );
        assert_eq!(
            public_key(&DEFAULT_PUBLIC_KEY.to_ascii_uppercase()),
            public_key(DEFAULT_PUBLIC_KEY)
        );
    }

    #[test]
    fn malformed_and_zero_public_keys_are_rejected() {
        for value in [
            "",
            "00",
            &"0".repeat(64),
            &"f".repeat(63),
            &"f".repeat(65),
            &"g".repeat(64),
            &"é".repeat(32),
        ] {
            assert!(public_key(value).is_err());
        }
    }

    #[test]
    fn identity_is_bounded_and_safe_without_escaping() {
        assert!(build_id("bootstrap-test-a").is_ok());
        assert!(build_id("candidate_test.b-1").is_ok());
        assert!(build_id(&"a".repeat(48)).is_ok());
        for value in [
            "",
            "has space",
            "has\nnewline",
            "quote\"",
            "slash/",
            "é",
            &"a".repeat(49),
        ] {
            assert!(build_id(value).is_err());
        }
    }
}
