//! Key model — `packages/keymap/src/types.ts` (KeyStrokeInput,
//! NormalizedKeyStroke, Binding, Command) shaped for the Rust port.

/// `KeyStrokeInput`/`NormalizedKeyStroke` (types.ts; normalize in
/// services/keys.ts:25-33: name trimmed+lowercased, flags default false).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyStroke {
    pub name: &'static str,
    pub ctrl: bool,
    pub shift: bool,
    pub meta: bool,
    pub super_key: bool,
}

impl KeyStroke {
    #[must_use]
    pub fn new(name: &str) -> Self {
        KeyStroke {
            name: leak_name(name.to_ascii_lowercase()),
            ctrl: false,
            shift: false,
            meta: false,
            super_key: false,
        }
    }

    /// Parse a `ctrl+shift+x`-style token (the binding-table grammar).
    /// `return` normalizes to the display name `enter` at stringify time
    /// (services/keys.ts:158).
    #[must_use]
    pub fn parse(token: &str) -> Self {
        let mut stroke = KeyStroke {
            name: "",
            ctrl: false,
            shift: false,
            meta: false,
            super_key: false,
        };
        for part in token.split('+') {
            match part.trim().to_ascii_lowercase().as_str() {
                "ctrl" => stroke.ctrl = true,
                "shift" => stroke.shift = true,
                "meta" | "alt" | "option" => stroke.meta = true,
                "super" | "cmd" | "command" => stroke.super_key = true,
                "" => {}
                name => stroke.name = leak_name(name.to_owned()),
            }
        }
        if stroke.name.is_empty() {
            stroke.name = "";
        }
        stroke
    }

    /// `buildKeyMatchId` (services/keys.ts:162-164).
    #[must_use]
    pub fn match_id(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.name,
            i32::from(self.ctrl),
            i32::from(self.shift),
            i32::from(self.meta),
            i32::from(self.super_key),
            0
        )
    }
}

fn leak_name(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// Alias mirroring the upstream input type name.
pub type KeyStrokeInput = KeyStroke;

/// `Binding` (types.ts:22-30 area): key → command with optional args and
/// when-guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub key: String,
    pub command: String,
    pub args: Option<String>,
}

/// `Command` (types.ts:40-49 area).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub id: String,
    pub title: String,
    pub category: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modifiers() {
        let k = KeyStroke::parse("ctrl+shift+x");
        assert!(k.ctrl && k.shift);
        assert_eq!(k.name, "x");
        let k = KeyStroke::parse("Cmd+K");
        assert!(k.super_key);
        assert_eq!(k.name, "k");
    }

    #[test]
    fn match_id_shape() {
        assert_eq!(KeyStroke::new("x").match_id(), "x:0:0:0:0:0");
    }
}
