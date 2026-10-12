# opentui-keymap

API-shape port of **`@opentui/keymap` 0.5.14** (spec: the vendored
byte-exact reference at `third_party/opentui/packages/keymap`). The
crate is standalone — no dependency on opencode-rust crates — and maps
every upstream package subpath onto a Rust module:

| upstream subpath (`package.json` exports) | module |
|---|---|
| `.` (`src/index.ts`) | [`Keymap`] facade, [`types`], [`stringify`] |
| `./addons` + `./addons/opentui` | [`addons`], [`textarea`] |
| `./extras` | [`extras`] |
| `./opentui` | [`opentui`] |
| `./solid` | [`solid`] |
| `./testing` | [`testing`] |
| `./react` | folded into [`solid`] (see the mapping below) |
| `./html`, `./runtime-modules` | elided (DOM hydration / bundler machinery) |

The dispatch engine ([`engine`]) is the upstream 0.5.14 dispatch
contract (services/dispatch.ts, sequence-index.ts, activation.ts,
active-layers.ts) reshaped onto the skeleton proven in opencode-rust's
`oc-tui-core::keymap` (Wave A replay-pinned). Every contract point
carries its file:line citation in the module docs; the behavioral
suite (`tests/behavior.rs`) ports the upstream addon/extras tests and
re-proves the oc-tui-core characterization cases against the upstream
semantics.

## The solid provider mapping (`/solid` + `/react`)

Upstream is JSX + solid-js (with a react twin). Rust has neither, so
the provider is a context struct and the hooks are accessor methods —
names kept parallel (solid/index.ts citations):

| upstream (file:line) | port |
|---|---|
| `KeymapProvider { keymap, children }` (solid:27) | `solid::KeymapProvider::new(keymap)` |
| `useKeymap()` (solid:52) | `KeymapProvider::use_keymap()` / `KeymapHandle` |
| `useKeymapStateVersion` (solid:62) | the engine `state` event (version counter + subscribers) |
| `useKeymapSelector(selector)` (solid:85) | `KeymapProvider::use_keymap_selector` → `Selector::read` re-runs on version change |
| `useBindings(createLayer)` (solid:107) | `KeymapProvider::use_bindings` → `BindingsGuard` (Drop = `onCleanup`) |
| `useActiveKeys` / `usePendingSequence` (react:82/92) | `solid::use_active_keys` / `solid::use_pending_sequence` |
| `reactiveMatcherFromSignal` (solid:156) | `solid::reactive_matcher_from_signal` |
| react's `reactiveMatcherFromStore` (react:181) | the same fn over a get+subscribe pair |

No JSX: "children" hold `KeymapHandle` clones (a `Weak` shared-keymap
reference); "unmounting" is dropping the provider.

## Divergences (Rust discipline, documented)

* **Disposers → `Registration` tokens.** Upstream `register*`
  functions return `() => void`; the port returns a
  [`Registration`] released via `Keymap::release` (mirrors
  oc-tui-core's layer-handle convention; composites via
  `Registration::Composite`).
* **Interceptors are decisions.** Upstream interceptors mutate the
  keymap inside the hook (`clearPendingSequence()` etc.). Rust borrow
  discipline expresses that as a returned
  [`InterceptDecision`] the engine applies — the escape/backspace
  addons are exactly their upstream behaviors over this channel.
* **Timed leader expiry is lazy.** Upstream arms a `setTimeout`; the
  port stores `expires_at` (now + timeout, re-armed on every pending
  change, exactly `syncArmedState`/`scheduleTimeout`) and evaluates
  expiry at each keymap interaction (`handle_key_event`, pending
  queries) or host `tick()`. Hosts without key traffic call
  `Keymap::handle…`/`tick` from their loop.
* **Throws → `Result`/`Option`/panics.** `createOpenTuiKeymap` on a
  destroyed host returns `None` (upstream throws); `new Keymap(host)`
  panics like upstream; binding-config errors are `Result::Err`
  strings carrying the upstream messages.
* **Host events are explicit.** Upstream subscribes to the host
  (`onKeyPress`/`onKeyRelease`/`onFocusChange`); the port's
  [`KeymapHost`] trait covers the focus tree + metadata the ENGINE
  needs, and hosts pump `Keymap::handle_key_event` (opentui-core's
  renderer grows the event wiring in the input lane).
* **Sequence patterns (`{count}`) and disambiguation resolvers are
  elided** (grammar positions parse and report; the default
  ambiguity resolution — continuations win — is the no-resolver path
  upstream installs).
* **Command execution resolves names through the registered
  catalog**; an unregistered command rejects the binding (upstream
  `runBinding` → false) with a warning — v2 always registers its
  commands first.

## Testing

`tests/behavior.rs` ports (per-test citations in file):

* `addons/universal/tests/{escape-clears-pending-sequence,
  backspace-pops-pending-sequence, comma-bindings, timed-leader}.test.ts`
* `addons/opentui/tests/{base-layout, edit-buffer-bindings}.test.ts`
  (the engine-side behaviors; the renderable-tree assertions land with
  the opentui-core input lane)
* `extras/tests/{binding-lookup, formatting, command-bindings}.test.ts`
* the oc-tui-core characterization suite (layer ordering, focus
  scopes, mode gating, rejection fall-through, preventDefault,
  interceptor precedence, which-key data model) re-proven against the
  upstream source — including the deltas listed in the lane report.
