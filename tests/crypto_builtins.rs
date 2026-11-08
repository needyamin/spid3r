//! Integration tests for the crypto builtins: `sha512` and the Ed25519
//! functions, called from real Spid3r scripts through the embedding API.
//! These cover the whole path — semantic checker accepts the names, natives
//! are registered, and results round-trip through script values.

use spid3r_lang::embed::Engine;

/// RFC 8032 section 7.1 TEST 1 seed.
const TEST1_SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const TEST1_PK: &str = "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a";
const TEST1_SIG: &str = "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b";

#[test]
fn sha512_builtin_matches_reference_vectors() {
    let engine = Engine::new();
    let out = engine
        .run(
            "t",
            r#"fn main() {
    println(sha512(""));
    println(sha512("abc"));
    println(sha512("abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu"));
    return 0;
}"#,
        )
        .expect("run");
    assert_eq!(out.exit_code, 0);
    let lines: Vec<&str> = out.output.lines().collect();
    assert_eq!(
        lines[0],
        "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
    );
    assert_eq!(
        lines[1],
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        lines[2],
        "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
    );
}

#[test]
fn ed25519_keypair_returns_public_and_secret() {
    let engine = Engine::new();
    let src = format!(
        r#"fn main() {{
    let kp = ed25519_keypair("{}");
    println(kp["public"]);
    println(kp["secret"]);
    println(string(len(kp["secret"])));  // hex chars: 64-byte secret = 128
    return 0;
}}"#,
        TEST1_SEED
    );
    let out = engine.run("t", &src).expect("run");
    assert_eq!(out.exit_code, 0);
    let lines: Vec<&str> = out.output.lines().collect();
    assert_eq!(lines[0], TEST1_PK);
    assert_eq!(lines[1], format!("{}{}", TEST1_SEED, TEST1_PK));
    assert_eq!(lines[2], "128");
}

#[test]
fn ed25519_sign_and_verify_rfc_vector() {
    let engine = Engine::new();
    let src = format!(
        r#"fn main() {{
    let kp = ed25519_keypair("{}");
    let sig = ed25519_sign(kp["secret"], "");
    println(sig);
    if ed25519_verify(kp["public"], "", sig) {{
        println("verify-ok");
    }} else {{
        println("verify-bad");
    }}
    return 0;
}}"#,
        TEST1_SEED
    );
    let out = engine.run("t", &src).expect("run");
    assert_eq!(out.exit_code, 0);
    let lines: Vec<&str> = out.output.lines().collect();
    assert_eq!(lines[0], TEST1_SIG);
    assert_eq!(lines[1], "verify-ok");
}

#[test]
fn ed25519_verify_rejects_tampered_and_malformed() {
    let engine = Engine::new();
    let src = format!(
        r#"fn main() {{
    let kp = ed25519_keypair("{}");
    let sig = ed25519_sign(kp["secret"], "hello");
    // Tampered message.
    let bad1 = ed25519_verify(kp["public"], "hello2", sig);
    // Tampered signature: flip the first hex char.
    let flipped = "0" + sig.slice(1, 128);
    let bad2 = ed25519_verify(kp["public"], "hello", flipped);
    // Malformed hex, wrong lengths, wrong types.
    let bad3 = ed25519_verify("zz", "hello", sig);
    let bad4 = ed25519_verify("abcd", "hello", sig);
    let bad5 = ed25519_verify(kp["public"], "hello", "xyz");
    // Wrong key.
    let other = ed25519_keypair("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb");
    let bad6 = ed25519_verify(other["public"], "hello", sig);
    println(string(bad1) + string(bad2) + string(bad3) + string(bad4) + string(bad5) + string(bad6));
    println(string(ed25519_verify(kp["public"], "hello", sig)));
    return 0;
}}"#,
        TEST1_SEED
    );
    let out = engine.run("t", &src).expect("run");
    assert_eq!(out.exit_code, 0);
    assert!(out.output.contains("falsefalsefalsefalsefalsefalse"), "{}", out.output);
    assert!(out.output.contains("true"), "{}", out.output);
}

#[test]
fn ed25519_wrong_length_hex_inputs_are_rejected() {
    let engine = Engine::new();
    // verify(): malformed or wrong-length public key / signature is false.
    let src = format!(
        r#"fn main() {{
    let kp = ed25519_keypair("{}");
    let sig = ed25519_sign(kp["secret"], "len test");
    let results = "";
    // Public key too short / too long (64 hex chars is correct).
    results += string(ed25519_verify(kp["public"].slice(0, 62), "len test", sig));
    results += string(ed25519_verify(kp["public"] + "ab", "len test", sig));
    // Signature too short / too long (128 hex chars is correct).
    results += string(ed25519_verify(kp["public"], "len test", sig.slice(0, 126)));
    results += string(ed25519_verify(kp["public"], "len test", sig + "cd"));
    // Non-hex digits.
    results += string(ed25519_verify("zz" + kp["public"].slice(0, 62), "len test", sig));
    results += string(ed25519_verify(kp["public"], "len test", "gg" + sig.slice(0, 126)));
    println(results);
    println(string(ed25519_verify(kp["public"], "len test", sig)));
    return 0;
}}"#,
        TEST1_SEED
    );
    let out = engine.run("t", &src).expect("run");
    assert_eq!(out.exit_code, 0);
    let lines: Vec<&str> = out.output.lines().collect();
    assert_eq!(lines[0], "falsefalsefalsefalsefalsefalse", "{}", out.output);
    assert_eq!(lines[1], "true");
}

#[test]
fn ed25519_sign_with_malformed_secret_is_an_error() {
    let engine = Engine::new();
    // sign(): a secret that is not exactly 64 bytes of hex is a clean error.
    for secret in ["abcd", "zz", TEST1_PK] {
        let src = format!(
            r#"fn main() {{
    let sig = ed25519_sign("{}", "oops");
    return 0;
}}"#,
            secret
        );
        let result = engine.run("t", &src);
        assert!(result.is_err(), "sign with secret {:?} must error", secret);
    }
}

#[test]
fn ed25519_keypair_without_seed_uses_os_entropy() {
    let engine = Engine::new();
    let src = r#"fn main() {
    let kp = ed25519_keypair();
    if len(kp["public"]) == 64 && len(kp["secret"]) == 128 {
        println("entropy-ok");
    }
    return 0;
}"#;
    let out = engine.run("t", src).expect("run");
    assert!(out.output.contains("entropy-ok"), "{}", out.output);
}
