//! Live check that the control behind an element token still exists, made
//! before a foreground step brings its window forward, moves the pointer,
//! clicks, or sends a key.
//!
//! The snapshot store accepts a token while its snapshot is the latest for the
//! window, but the app can remove or replace the control after the read. A
//! background route then fails without disturbing anything. A foreground route
//! would first activate the window and only then fail on the dead element, so
//! it checks here first and refuses with the store's `stale_element_token`
//! envelope (see `SnapshotStore::dead_element_refusal`).

use crate::ax::bindings::{
    kAXErrorInvalidUIElement, AXError, AXUIElementCopyAttributeValue, AXUIElementRef,
};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::string::CFString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Liveness {
    Alive,
    Dead,
}

/// Only an invalid element (`-25202`) proves the control is gone. Any other
/// result, including a timeout, an unresponsive app or a missing attribute,
/// says nothing about it, so the call proceeds as it did before this check.
pub(crate) fn liveness_from_ax_error(error: AXError) -> Liveness {
    if error == kAXErrorInvalidUIElement {
        Liveness::Dead
    } else {
        Liveness::Alive
    }
}

/// One live accessibility read of the element's role.
pub(crate) fn read_liveness(element_ptr: usize) -> Liveness {
    let attr = CFString::from_static_string("AXRole");
    let mut value: CFTypeRef = std::ptr::null();
    let error = unsafe {
        AXUIElementCopyAttributeValue(
            element_ptr as AXUIElementRef,
            attr.as_concrete_TypeRef(),
            &mut value,
        )
    };
    if !value.is_null() {
        unsafe { CFRelease(value) };
    }
    liveness_from_ax_error(error)
}

/// The foreground step was refused before anything was activated or sent
/// because its element no longer exists. Tools turn this into the store's
/// `stale_element_token` refusal.
#[derive(Debug)]
pub(crate) struct DeadElement;

impl std::fmt::Display for DeadElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "stale_element_token: the element no longer exists; nothing was activated or sent",
        )
    }
}

impl std::error::Error for DeadElement {}

/// Run `foreground` only when `liveness` does not prove the element dead.
pub(crate) fn require_live<R>(
    liveness: impl FnOnce() -> Liveness,
    foreground: impl FnOnce() -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    match liveness() {
        Liveness::Dead => Err(DeadElement.into()),
        Liveness::Alive => foreground(),
    }
}

pub(crate) fn is_dead_element(error: &anyhow::Error) -> bool {
    error.downcast_ref::<DeadElement>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_invalid_element_is_dead() {
        assert_eq!(liveness_from_ax_error(-25202), Liveness::Dead);
        // Success, failure, illegal argument, cannot complete, attribute
        // unsupported, no value, API disabled, not implemented.
        for other in [0, -25200, -25201, -25204, -25205, -25212, -25211, -25208] {
            assert_eq!(liveness_from_ax_error(other), Liveness::Alive, "{other}");
        }
    }

    #[test]
    fn a_dead_element_never_reaches_the_foreground_step() {
        let mut ran = false;
        let result = require_live(
            || Liveness::Dead,
            || {
                ran = true;
                Ok(())
            },
        );
        assert!(!ran, "the foreground step ran for a dead element");
        let error = result.unwrap_err();
        assert!(is_dead_element(&error), "{error}");
    }

    #[test]
    fn an_unclear_liveness_read_proceeds_as_before() {
        let mut ran = false;
        let result = require_live(
            || liveness_from_ax_error(-25204),
            || {
                ran = true;
                Ok(7)
            },
        );
        assert!(ran);
        assert_eq!(result.unwrap(), 7);
        let failed: anyhow::Result<()> = require_live(
            || Liveness::Alive,
            || anyhow::bail!("AXPress returned -25202"),
        );
        assert!(!is_dead_element(&failed.unwrap_err()));
    }
}
