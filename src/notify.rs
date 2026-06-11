use std::collections::HashMap;
use std::error::Error;

use zbus::blocking::Connection;
use zvariant::Value;

pub fn notify(summary: &str, body: &str) -> Result<(), Box<dyn Error>> {
    let connection = Connection::session()?;

    let _reply = connection
        .call_method(
            Some("org.freedesktop.Notifications"),
            "/org/freedesktop/Notifications",
            Some("org.freedesktop.Notifications"),
            "Notify",
            &(
                "Tourbox",
                0u32,
                "dialog-information",
                summary,
                body,
                vec![""; 0],
                HashMap::<&str, &Value>::new(),
                1000,
            ),
        )?
        .body();

    Ok(())
}
