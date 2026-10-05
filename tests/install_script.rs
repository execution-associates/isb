//! install.sh trusts a key isb compiles in, so the installer and `isb update`
//! accept the same signed releases.

#[test]
fn install_script_trusts_a_release_key() {
    let script = include_str!("../install.sh");
    let key = script
        .lines()
        .find_map(|l| l.strip_prefix("KEY="))
        .expect("install.sh sets KEY=");
    assert!(
        isb::self_update::RELEASE_KEYS.contains(&key),
        "install.sh KEY={key} is not in RELEASE_KEYS"
    );
    assert!(
        isb::self_update::INSTALL_COMMAND.ends_with("/install.sh | sh"),
        "INSTALL_COMMAND runs install.sh"
    );
}
