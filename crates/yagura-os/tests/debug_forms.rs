#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The `Debug` forms of public types that carry an environment: they name the
//! kind of value and never show a variable's name or value, which routinely
//! hold tokens.

use std::ffi::OsString;

use yagura_os::pty::EnvOp;

const SECRET_NAME: &str = "DEBUG_FORM_SECRET_NAME";
const SECRET_VALUE: &str = "debug-form-secret-value";

#[test]
fn an_env_op_names_its_kind_and_shows_neither_name_nor_value() {
    let set = EnvOp::Set(OsString::from(SECRET_NAME), OsString::from(SECRET_VALUE));
    let remove = EnvOp::Remove(OsString::from(SECRET_NAME));

    let set_text = format!("{set:?}");
    let remove_text = format!("{remove:?}");

    assert!(set_text.contains("Set"), "got {set_text:?}");
    assert!(remove_text.contains("Remove"), "got {remove_text:?}");
    for text in [&set_text, &remove_text] {
        assert!(
            !text.contains(SECRET_NAME) && !text.contains(SECRET_VALUE),
            "a name or value leaked into {text:?}"
        );
    }
}
