//! `ensureRuntimePluginSupport` — contract-level port of
//! `@opentui/solid/runtime-plugin-support/configure`
//! (third_party/opentui/packages/solid/scripts/runtime-plugin-support-configure.ts;
//! re-exported through package.json:39-42). Lane D1c item f.
//!
//! Gate D0 ruling: **contract-level port**. The upstream
//! mechanism — registering a Bun module-resolution plugin
//! (`registerBunPlugin(createRuntimePlugin(...))`) plus the solid
//! transform plugin so runtime imports resolve in-process — is
//! bun-specific loader machinery with no Rust equivalent; what ref
//! consumes (`plugin/runtime-plugin-support.bun.ts`) is the
//! *initialization semantics*:
//!
//! * first call installs the support and returns `true`;
//! * later calls with compatible options return `false` (idempotent);
//! * later calls with INCOMPATIBLE options fail with the three
//!   upstream errors (missing specifier / different core module /
//!   different rewrite options) — word-for-word;
//! * the default runtime-module set (the solid/solid-js specifiers,
//!   `defaultRuntimeModules`) plus `additional` form the installed
//!   specifier set.
//!
//! mechanism ≠ upstream (a process-global install record instead of
//! the bun loader plugin), output == upstream (boolean + compat
//! errors). `RuntimeModuleEntry`/`RuntimeModuleExports` have no Rust
//! counterpart — the install records the specifier KEY SET and an
//! opaque core-module identity (`core_id`), which is all the
//! compatibility assertions consume.

use std::sync::Mutex;

/// `RuntimePluginRewriteOptions` (via @opentui/core/runtime-plugin)
/// — only the normalization key matters for the compat assertions
/// (`${nodeModulesRuntimeSpecifiers ?? true}:${nodeModulesBareSpecifiers ?? false}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimePluginRewriteOptions {
    pub node_modules_runtime_specifiers: Option<bool>,
    pub node_modules_bare_specifiers: Option<bool>,
}

/// `SolidRuntimePluginSupportOptions`
/// (runtime-plugin-support-configure.ts:20-24).
pub struct SolidRuntimePluginSupportOptions<'a> {
    /// `additional` — extra runtime modules beyond the defaults.
    pub additional: &'a [(&'a str, &'a str)],
    /// `core` — a replacement core runtime module; the opaque entry
    /// identity (what `options.core !== install.core` compares).
    pub core: Option<&'a str>,
    pub rewrite: Option<RuntimePluginRewriteOptions>,
}

impl Default for SolidRuntimePluginSupportOptions<'_> {
    fn default() -> Self {
        SolidRuntimePluginSupportOptions {
            additional: &[],
            core: None,
            rewrite: None,
        }
    }
}

/// `defaultRuntimeModules`
/// (runtime-plugin-support-configure.ts:52-61).
pub const DEFAULT_RUNTIME_MODULES: [&str; 6] = [
    "@opentui/solid",
    "@opentui/solid/components",
    "@opentui/solid/jsx-runtime",
    "@opentui/solid/jsx-dev-runtime",
    "solid-js",
    "solid-js/store",
];

#[derive(Debug)]
struct RuntimePluginSupportInstall {
    specifiers: Vec<String>,
    core_id: Option<String>,
    rewrite_key: String,
}

struct InstallState {
    install: Option<RuntimePluginSupportInstall>,
}

static INSTALL_STATE: Mutex<InstallState> = Mutex::new(InstallState { install: None });

fn normalize_rewrite_key(rewrite: Option<RuntimePluginRewriteOptions>) -> String {
    match rewrite {
        None => "true:false".to_string(),
        Some(r) => format!(
            "{}:{}",
            r.node_modules_runtime_specifiers.unwrap_or(true),
            r.node_modules_bare_specifiers.unwrap_or(false),
        ),
    }
}

/// `ensureRuntimePluginSupport(options = {}) -> boolean`
/// (runtime-plugin-support-configure.ts:100-125). See the module doc
/// for the mechanism≠upstream ruling. Panics on incompatible
/// re-configuration exactly where upstream throws.
pub fn ensure_runtime_plugin_support(options: &SolidRuntimePluginSupportOptions<'_>) -> bool {
    let mut state = INSTALL_STATE
        .lock()
        .expect("runtime-plugin-support state poisoned");
    if let Some(install) = &state.install {
        assert_compatible_install(install, options);
        return false;
    }
    let mut specifiers: Vec<String> = DEFAULT_RUNTIME_MODULES
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for (specifier, _) in options.additional {
        specifiers.push((*specifier).to_string());
    }
    state.install = Some(RuntimePluginSupportInstall {
        specifiers,
        core_id: options.core.map(|c| c.to_string()),
        rewrite_key: normalize_rewrite_key(options.rewrite),
    });
    true
}

/// `assertCompatibleInstall` (runtime-plugin-support-configure.ts:75-97)
/// — the three upstream errors, word-for-word.
fn assert_compatible_install(
    install: &RuntimePluginSupportInstall,
    options: &SolidRuntimePluginSupportOptions<'_>,
) {
    let all_specifiers: Vec<&str> = DEFAULT_RUNTIME_MODULES
        .iter()
        .copied()
        .chain(options.additional.iter().map(|(s, _)| *s))
        .collect();
    for specifier in all_specifiers {
        if !install.specifiers.iter().any(|s| s == specifier) {
            panic!(
                "OpenTUI Solid runtime plugin support is already installed without {specifier}. \
Call ensureRuntimePluginSupport({{ additional }}) from \
@opentui/solid/runtime-plugin-support/configure before importing \
@opentui/solid/runtime-plugin-support."
            );
        }
    }
    let core_id = options.core.map(|c| c.to_string());
    if core_id.is_some() && core_id != install.core_id {
        panic!(
            "OpenTUI Solid runtime plugin support is already installed with a different core runtime module."
        );
    }
    if options.rewrite.is_some() && normalize_rewrite_key(options.rewrite) != install.rewrite_key {
        panic!("OpenTUI Solid runtime plugin support is already installed with different rewrite options.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_install_true_then_idempotent_false() {
        // reset for the test process
        INSTALL_STATE.lock().unwrap().install = None;
        assert!(ensure_runtime_plugin_support(
            &SolidRuntimePluginSupportOptions::default()
        ));
        assert!(!ensure_runtime_plugin_support(
            &SolidRuntimePluginSupportOptions::default()
        ));
    }

    #[test]
    fn additional_specifiers_extend_and_are_accepted() {
        INSTALL_STATE.lock().unwrap().install = None;
        let opts = SolidRuntimePluginSupportOptions {
            additional: &[("@opencode/plugin/tui", "entry")],
            ..Default::default()
        };
        assert!(ensure_runtime_plugin_support(&opts));
        // same shape again → false, no error
        assert!(!ensure_runtime_plugin_support(&opts));
    }

    #[test]
    #[should_panic(expected = "already installed without @opencode/plugin/tui")]
    fn later_additional_specifier_fails_word_for_word() {
        INSTALL_STATE.lock().unwrap().install = None;
        ensure_runtime_plugin_support(&SolidRuntimePluginSupportOptions::default());
        let opts = SolidRuntimePluginSupportOptions {
            additional: &[("@opencode/plugin/tui", "entry")],
            ..Default::default()
        };
        let _ = ensure_runtime_plugin_support(&opts);
    }

    #[test]
    #[should_panic(expected = "different core runtime module")]
    fn different_core_module_fails() {
        INSTALL_STATE.lock().unwrap().install = None;
        ensure_runtime_plugin_support(&SolidRuntimePluginSupportOptions::default());
        let opts = SolidRuntimePluginSupportOptions {
            core: Some("other-core"),
            ..Default::default()
        };
        let _ = ensure_runtime_plugin_support(&opts);
    }

    #[test]
    #[should_panic(expected = "different rewrite options")]
    fn different_rewrite_options_fail() {
        INSTALL_STATE.lock().unwrap().install = None;
        ensure_runtime_plugin_support(&SolidRuntimePluginSupportOptions::default());
        let opts = SolidRuntimePluginSupportOptions {
            rewrite: Some(RuntimePluginRewriteOptions {
                node_modules_bare_specifiers: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let _ = ensure_runtime_plugin_support(&opts);
    }

    #[test]
    fn rewrite_key_normalizes_like_upstream() {
        assert_eq!(normalize_rewrite_key(None), "true:false");
        assert_eq!(
            normalize_rewrite_key(Some(RuntimePluginRewriteOptions::default())),
            "true:false"
        );
        assert_eq!(
            normalize_rewrite_key(Some(RuntimePluginRewriteOptions {
                node_modules_runtime_specifiers: Some(false),
                node_modules_bare_specifiers: Some(true),
            })),
            "false:true"
        );
    }
}
