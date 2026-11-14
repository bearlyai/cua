//! The `check_permissions` tool definition.
//!
//! The tool itself is Windows-only, but its definition has no Win32
//! dependency, so it lives here where its tests run on every host.

use cua_driver_core::tool::ToolDef;
use serde_json::json;

pub(crate) fn check_permissions_def() -> ToolDef {
    ToolDef {
        name: "check_permissions".into(),
        description: "Check required permissions for cua-driver-rs on Windows.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "boolean",
                    "default": false,
                    "description": "Accepted so one host call works on every platform. Windows has no permission prompt, so it changes nothing."
                },
                "probe_direct_capture": {
                    "type": "boolean",
                    "description": "Accepted so one host call works on every platform. Windows has no permission prompt, so it changes nothing."
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

    /// The Bearly desktop app's and dashboard's `check_permissions` calls,
    /// checked against the Windows definition on every host.
    #[test]
    fn every_bearly_host_permission_check_is_accepted_on_windows() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../compat-fixtures/bearly-host-calls.json"
        ))
        .unwrap();
        let def = check_permissions_def();
        let rows: Vec<_> = fixture["calls"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["tool"] == "check_permissions")
            .filter(|row| {
                row["platforms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|name| name == "windows")
            })
            .collect();
        assert!(!rows.is_empty());
        for row in rows {
            assert_eq!(def.unknown_argument(&row["args"]), None, "{row}");
        }
    }
}
