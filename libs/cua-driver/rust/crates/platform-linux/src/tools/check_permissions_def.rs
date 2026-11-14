//! The `check_permissions` tool definition.
//!
//! The tool itself is Linux-only, but its definition has no X11 or AT-SPI
//! dependency, so it lives here where its tests run on every host.

use cua_driver_core::tool::ToolDef;
use serde_json::json;

pub(crate) fn check_permissions_def() -> ToolDef {
    ToolDef {
        name: "check_permissions".into(),
        description: "Check required permissions for cua-driver-rs on Linux.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "boolean",
                    "default": false,
                    "description": "Accepted so one host call works on every platform. Linux has no permission prompt, so it changes nothing."
                },
                "probe_direct_capture": {
                    "type": "boolean",
                    "description": "Accepted so one host call works on every platform. Linux has no permission prompt, so it changes nothing."
                }
            },
            "additionalProperties": false
        }),
        read_only: true,
        destructive: false,
        idempotent: true,
        open_world: false,
    }
}

#[cfg(test)]
mod tests {
    use super::check_permissions_def;
    use serde_json::json;

    #[test]
    fn the_desktop_hosts_permission_check_has_no_unknown_argument() {
        let def = check_permissions_def();
        for args in [
            json!({}),
            json!({"session": "bearly", "prompt": false}),
            json!({"session": "bearly", "prompt": false, "probe_direct_capture": false}),
        ] {
            assert_eq!(def.unknown_argument(&args), None, "{args}");
        }
        assert_eq!(
            def.unknown_argument(&json!({"session": "bearly", "promt": false})),
            Some("promt".to_owned())
        );
    }
}
